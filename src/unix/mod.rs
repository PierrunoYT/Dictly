//! Live dictation on macOS and Linux (X11). Shortcuts, focus tracking, and key
//! state are OS-specific; typing, the clipboard, saved keys, and sounds use
//! cross-platform crates.

pub mod clipboard;
pub mod credential;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
pub use x11::{Shortcuts, Target, escape_down, shortcut_held, shortcut_keys_down};
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{Shortcuts, Target, escape_down, shortcut_held, shortcut_keys_down};

use crate::{
    config::Config,
    dictation::{Color, run_controlled},
    pipeline::Pipeline,
    sound::{Cue, RATE, samples},
};
use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::{
    cell::RefCell,
    fs::{File, TryLockError},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

/// Modifier names shown with the shortcut keys.
#[cfg(target_os = "macos")]
pub const KEYS: &str = "Control+Option";
#[cfg(target_os = "linux")]
pub const KEYS: &str = "Ctrl+Alt";

/// One Dictly per user session: two would compete for the same shortcuts.
pub struct SingleInstance(#[allow(dead_code)] File);

impl SingleInstance {
    pub fn acquire() -> Result<Self> {
        // XDG_RUNTIME_DIR is per user; the shared temp directory needs the
        // user name so one user's instance cannot block another's.
        let path = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) => std::path::PathBuf::from(dir).join("dictly.lock"),
            None => std::env::temp_dir().join(format!(
                "dictly-{}.lock",
                std::env::var("USER").unwrap_or_default()
            )),
        };
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "Cannot check for another running Dictly ({})",
                    path.display()
                )
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self(file)),
            Err(TryLockError::WouldBlock) => {
                bail!("Dictly is already running. Use the open Dictly window or close it first.")
            }
            Err(TryLockError::Error(error)) => {
                Err(error).context("Cannot check for another running Dictly")
            }
        }
    }
}

thread_local! {
    // Created on first use by the dictation thread, which does all typing.
    static ENIGO: RefCell<Option<Enigo>> = const { RefCell::new(None) };
}

fn with_enigo<T>(action: impl FnOnce(&mut Enigo) -> enigo::InputResult<T>) -> Result<T> {
    ENIGO.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            *cell = Some(Enigo::new(&Settings::default()).map_err(|error| {
                anyhow!("Cannot send keystrokes to other apps: {error}. {INPUT_PERMISSION}")
            })?);
        }
        action(cell.as_mut().unwrap()).map_err(|error| anyhow!("Keystroke failed: {error}"))
    })
}

#[cfg(target_os = "macos")]
const INPUT_PERMISSION: &str =
    "Allow the app that started Dictly in System Settings › Privacy & Security › Accessibility.";
#[cfg(target_os = "linux")]
const INPUT_PERMISSION: &str = "Dictly needs an X11 session with the XTEST extension.";

/// Type one character, which leaves the clipboard untouched.
pub fn type_char(c: char) -> Result<()> {
    let mut buffer = [0; 4];
    with_enigo(|enigo| enigo.text(c.encode_utf8(&mut buffer)))
}

/// Press the system paste shortcut: Command+V on macOS, Ctrl+V on Linux.
pub fn send_paste() -> Result<()> {
    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(target_os = "linux")]
    let modifier = Key::Control;
    with_enigo(|enigo| {
        enigo.key(modifier, Direction::Press)?;
        let pasted = enigo.key(Key::Unicode('v'), Direction::Click);
        enigo.key(modifier, Direction::Release)?;
        pasted
    })
}

/// The on-screen status pill exists only on Windows; elsewhere sounds and the
/// window's status line report progress.
pub struct Overlay;

impl Overlay {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    pub fn show(&self, _text: &str, _color: Color, _near: Option<Target>) {}

    pub fn hide(&self) {}
}

/// Play a cue on the default output device without blocking the caller.
pub fn play(cue: Cue) {
    thread::spawn(move || {
        // A missing or busy output device only loses the tone.
        let _ = play_blocking(cue);
    });
}

fn play_blocking(cue: Cue) -> Result<()> {
    let device = cpal::default_host()
        .default_output_device()
        .context("No output device")?;
    let supported = device.default_output_config()?;
    let config: cpal::StreamConfig = supported.clone().into();
    let rate = config.sample_rate.0 as usize;
    let tone = samples(cue);
    // Nearest-sample resampling is fine for short sine tones.
    let tone: Arc<Vec<f32>> = Arc::new(
        (0..tone.len() * rate / RATE as usize)
            .map(|index| tone[index * RATE as usize / rate])
            .collect(),
    );
    let length = tone.len();
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => output::<f32>(&device, &config, tone)?,
        cpal::SampleFormat::I16 => output::<i16>(&device, &config, tone)?,
        cpal::SampleFormat::U16 => output::<u16>(&device, &config, tone)?,
        format => bail!("Unsupported output format {format}"),
    };
    stream.play()?;
    thread::sleep(Duration::from_millis((length * 1000 / rate) as u64 + 100));
    Ok(())
}

fn output<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    tone: Arc<Vec<f32>>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels as usize;
    let position = AtomicUsize::new(0);
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            for frame in data.chunks_mut(channels) {
                let index = position.fetch_add(1, Ordering::Relaxed);
                let sample = tone.get(index).copied().unwrap_or(0.0);
                frame.fill(T::from_sample(sample));
            }
        },
        |_| {},
        None,
    )?)
}

/// Console dictation; Ctrl+C ends the process.
pub fn run(config: Config) -> Result<()> {
    static STOP: AtomicBool = AtomicBool::new(false);
    let _instance = SingleInstance::acquire()?;
    let pipeline = Pipeline::new(config.clone())?;
    run_controlled(config, pipeline, &STOP, |message| eprintln!("{message}"))
}
