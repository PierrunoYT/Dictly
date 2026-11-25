mod capture;
mod clipboard;
pub mod credential;
mod overlay;
mod sound;

pub use capture::microphones;

use crate::{
    config::{Config, Insertion},
    pipeline::{Pipeline, Transcript},
};
use anyhow::{Result, bail, ensure};
use capture::Capture;
use overlay::Overlay;
use sound::Cue;
use std::{
    mem::size_of,
    ptr::null_mut,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND},
    System::Threading::{CreateMutexW, GetCurrentProcessId},
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
            KEYEVENTF_UNICODE, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, SendInput,
            UnregisterHotKey, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_SPACE,
        },
        WindowsAndMessaging::{
            DispatchMessageW, GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo,
            GetWindowThreadProcessId, MSG, PM_REMOVE, PeekMessageW, WM_HOTKEY, WM_QUIT,
        },
    },
};

const TOGGLE: i32 = 1;
const CANCEL: i32 = 2;
const RECOVER: i32 = 3;
/// Some editors, Windows 11 Notepad among them, read the most recently
/// injected character when they process a keystroke, so a batch arrives as
/// repeats of its last character. Typing one UTF-16 unit at a time with a short
/// pause lets each be consumed first; 1 ms was measured to be too short.
const TYPING_INTERVAL: Duration = Duration::from_millis(10);
/// Time for the target to read the clipboard after Ctrl+V before it is restored.
const PASTE_SETTLE: Duration = Duration::from_millis(400);
/// Holding the shortcut longer than this is push-to-talk: releasing it
/// finishes. A shorter tap keeps recording hands-free until the next press.
const HOLD_THRESHOLD: Duration = Duration::from_millis(500);

pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// One Dictly per user session: two would compete for the same shortcuts.
pub struct SingleInstance(HANDLE);

impl SingleInstance {
    pub fn acquire() -> Result<Self> {
        let name = wide("Local\\Dictly.SingleInstance");
        // SAFETY: name is NUL-terminated; the handle is closed on drop or below.
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            ensure!(
                !handle.is_null(),
                "Cannot check for another running Dictly"
            );
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                bail!(
                    "Dictly is already running. Use the open Dictly window or close it first."
                );
            }
            Ok(Self(handle))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by acquire and is closed once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct Hotkeys(Vec<i32>);

impl Hotkeys {
    fn register() -> Result<Self> {
        let mut keys = Self(vec![]);
        for (id, key) in [(TOGGLE, VK_SPACE), (CANCEL, VK_ESCAPE), (RECOVER, 0x56)] {
            // SAFETY: null HWND binds the hotkey to this thread's message queue.
            ensure!(
                unsafe {
                    RegisterHotKey(
                        null_mut(),
                        id,
                        MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
                        key as u32,
                    )
                } != 0,
                "Cannot register Ctrl+Alt+{}. Another app or instance may own this shortcut.",
                match id {
                    TOGGLE => "Space",
                    CANCEL => "Esc",
                    _ => "V",
                }
            );
            keys.0.push(id);
        }
        Ok(keys)
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        for &id in &self.0 {
            // SAFETY: these IDs were registered by this thread.
            unsafe {
                UnregisterHotKey(null_mut(), id);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    window: HWND,
    focus: HWND,
}

impl Target {
    fn current() -> Option<Self> {
        // SAFETY: queried handles are not dereferenced; info has the required size.
        unsafe {
            let window = GetForegroundWindow();
            if window.is_null() {
                return None;
            }
            let mut process_id = 0;
            let tid = GetWindowThreadProcessId(window, &mut process_id);
            // Never record for or insert into Dictly's own settings/API-key fields.
            if process_id == GetCurrentProcessId() {
                return None;
            }
            let mut info: GUITHREADINFO = std::mem::zeroed();
            info.cbSize = size_of::<GUITHREADINFO>() as u32;
            if tid == 0 || GetGUIThreadInfo(tid, &mut info) == 0 || info.hwndFocus.is_null() {
                return None;
            }
            Some(Self {
                window,
                focus: info.hwndFocus,
            })
        }
    }
}

/// Signals the transcription worker to skip retries and cleanup. Dropping the
/// processing state (cancel, disable, or exit) cancels automatically.
struct CancelOnDrop(Arc<AtomicBool>);

impl CancelOnDrop {
    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn is_canceled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.cancel();
    }
}

enum State {
    Idle,
    Recording {
        capture: Capture,
        target: Target,
        started: Instant,
        focus_changed: bool,
        /// Set while the starting shortcut is still held down.
        held_since: Option<Instant>,
    },
    Processing {
        target: Target,
        focus_changed: bool,
        cancel: CancelOnDrop,
    },
}

/// Optional sound cues and status overlay.
struct Feedback {
    overlay: Option<Overlay>,
    sounds: bool,
    hide_at: Option<Instant>,
}

impl Feedback {
    fn new(config: &Config, report: &impl Fn(&str)) -> Self {
        let overlay = if config.overlay {
            Overlay::new()
                .inspect_err(|error| report(&format!("{error:#}; continuing without it.")))
                .ok()
        } else {
            None
        };
        Self {
            overlay,
            sounds: config.sounds,
            hide_at: None,
        }
    }

    fn show(&mut self, text: &str, color: u32, near: HWND, linger: Option<Duration>) {
        if let Some(overlay) = &self.overlay {
            overlay.show(text, color, near);
        }
        self.hide_at = linger.map(|linger| Instant::now() + linger);
    }

    fn cue(&self, cue: Cue) {
        if self.sounds {
            sound::play(cue);
        }
    }

    fn hide(&mut self) {
        self.hide_at = None;
        if let Some(overlay) = &self.overlay {
            overlay.hide();
        }
    }

    fn tick(&mut self) {
        if self.hide_at.is_some_and(|at| Instant::now() >= at) {
            self.hide();
        }
    }
}

fn shortcut_held() -> bool {
    [VK_CONTROL, VK_MENU, VK_SPACE]
        .iter()
        // SAFETY: GetAsyncKeyState only reads global key state.
        .all(|&key| unsafe { GetAsyncKeyState(key as i32) } < 0)
}

/// Set by Ctrl+C, Ctrl+Break, or closing the console; stops console `run` the
/// way **Disable dictation** stops the desktop worker.
static CONSOLE_STOP: AtomicBool = AtomicBool::new(false);
/// Set once console `run` has returned.
static CONSOLE_STOPPED: AtomicBool = AtomicBool::new(false);

extern "system" fn console_control(_event: u32) -> windows_sys::Win32::Foundation::BOOL {
    CONSOLE_STOP.store(true, Ordering::Release);
    // Windows ends the process when this returns for a closed console, logoff,
    // or shutdown. Wait briefly so a paste in progress restores the clipboard.
    let deadline = Instant::now() + Duration::from_secs(1);
    while !CONSOLE_STOPPED.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    windows_sys::Win32::Foundation::TRUE
}

pub fn run(config: Config) -> Result<()> {
    let _instance = SingleInstance::acquire()?;
    // The desktop window gets this from winit; the CLI sets it so the overlay
    // is drawn at the monitor's real resolution instead of being stretched.
    // SAFETY: only changes how this process is scaled.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let pipeline = Pipeline::new(config.clone())?;
    // SAFETY: console_control is a valid handler for the life of the process.
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(console_control), 1);
    }
    let result = run_controlled(config, pipeline, &CONSOLE_STOP, |message| {
        eprintln!("{message}");
    });
    CONSOLE_STOPPED.store(true, Ordering::Release);
    result
}

pub(crate) fn run_controlled(
    config: Config,
    pipeline: Pipeline,
    stopped: &AtomicBool,
    report: impl Fn(&str),
) -> Result<()> {
    let pipeline = Arc::new(pipeline);
    let _hotkeys = Hotkeys::register()?;
    let (tx, rx) = mpsc::channel::<Result<Transcript>>();
    let mut state = State::Idle;
    let mut recovery: Option<String> = None;
    let mut feedback = Feedback::new(&config, &report);
    report(
        "Dictly — Ctrl+Alt+Space: hold to talk, or tap to start/stop; Ctrl+Alt+Esc: cancel/clear; Ctrl+Alt+V: insert retained text.",
    );
    report(&format!(
        "Audio is sent to {}. Cleanup: {}.",
        config.base_url, config.cleanup
    ));
    report(
        "Ready. Focus an editable text field before starting. Do not change fields until insertion finishes.",
    );
    while !stopped.load(Ordering::Acquire) {
        if let State::Recording {
            target,
            focus_changed,
            ..
        }
        | State::Processing {
            target,
            focus_changed,
            ..
        } = &mut state
        {
            *focus_changed |= Target::current() != Some(*target);
        }
        feedback.tick();
        let mut message: MSG = unsafe { std::mem::zeroed() };
        // SAFETY: message is a valid writable MSG.
        while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
            if message.message == WM_QUIT || stopped.load(Ordering::Acquire) {
                return Ok(());
            }
            if message.message != WM_HOTKEY {
                // The overlay window belongs to this thread and needs painting.
                // SAFETY: message was filled by PeekMessageW.
                unsafe {
                    DispatchMessageW(&message);
                }
                continue;
            }
            match message.wParam as i32 {
                CANCEL => {
                    recovery = None;
                    feedback.cue(Cue::Cancel);
                    let near = Target::current().map_or(null_mut(), |target| target.window);
                    feedback.show(
                        "Canceled",
                        overlay::GRAY,
                        near,
                        Some(Duration::from_secs(1)),
                    );
                    if let State::Processing { cancel, .. } = &state {
                        cancel.cancel();
                        report(
                            "Canceled. Waiting for in-flight request to finish; its result will be discarded.",
                        );
                    } else {
                        state = State::Idle;
                        report("Canceled; audio and retained text discarded.");
                    }
                }
                TOGGLE => match state {
                    State::Idle => {
                        if let Some(target) = Target::current() {
                            match Capture::start(
                                config.max_recording_seconds,
                                config.microphone.as_deref(),
                            ) {
                                Ok(capture) => {
                                    feedback.cue(Cue::Start);
                                    feedback.show("Recording…", overlay::RED, target.window, None);
                                    report(&format!(
                                        "Recording… ({}) Release the shortcut to finish, or after a quick tap press it again. Ctrl+Alt+Esc discards.",
                                        capture.device
                                    ));
                                    state = State::Recording {
                                        capture,
                                        target,
                                        started: Instant::now(),
                                        focus_changed: false,
                                        held_since: Some(Instant::now()),
                                    };
                                }
                                Err(error) => {
                                    feedback.cue(Cue::Error);
                                    feedback.show(
                                        "Microphone unavailable",
                                        overlay::RED,
                                        target.window,
                                        Some(Duration::from_secs(3)),
                                    );
                                    report(&format!("Microphone error: {error:#}"));
                                }
                            }
                        } else {
                            report("Focus a text field in another application first.");
                        }
                    }
                    State::Recording { .. } => {
                        finish_recording(&mut state, &pipeline, &tx, &report, &mut feedback)
                    }
                    State::Processing { .. } => report("Still processing; wait or cancel."),
                },
                RECOVER if matches!(state, State::Idle) => {
                    if let Some(text) = &recovery {
                        if let Some(target) = Target::current() {
                            match insert(text, target, stopped, config.insertion) {
                                Ok(()) => {
                                    recovery = None;
                                    feedback.show(
                                        "Inserted",
                                        overlay::GREEN,
                                        target.window,
                                        Some(Duration::from_secs(1)),
                                    );
                                    report("Retained text inserted.");
                                }
                                Err(error) => report(&format!("Insertion failed: {error:#}")),
                            }
                        }
                    } else {
                        report("No retained transcript.");
                    }
                }
                _ => {}
            }
        }
        if let State::Recording {
            capture,
            started,
            held_since,
            target,
            ..
        } = &mut state
        {
            let released = held_since.is_some() && !shortcut_held();
            let push_to_talk =
                released && held_since.is_some_and(|at| at.elapsed() >= HOLD_THRESHOLD);
            if released && !push_to_talk {
                *held_since = None;
                feedback.show(
                    "Recording · Ctrl+Alt+Space to finish",
                    overlay::RED,
                    target.window,
                    None,
                );
                report(
                    "Hands-free recording. Press Ctrl+Alt+Space to finish, Ctrl+Alt+Esc to discard.",
                );
            }
            if capture.failed() {
                state = State::Idle;
                feedback.cue(Cue::Error);
                feedback.hide();
                report("Microphone failed; recording discarded. Check your input device.");
            } else if capture.is_full()
                || started.elapsed().as_secs() >= config.max_recording_seconds as u64
            {
                if stopped.load(Ordering::Acquire) {
                    return Ok(());
                }
                report("Recording limit reached.");
                finish_recording(&mut state, &pipeline, &tx, &report, &mut feedback);
            } else if push_to_talk {
                finish_recording(&mut state, &pipeline, &tx, &report, &mut feedback);
            }
        }
        if let Ok(result) = rx.try_recv() {
            let previous = std::mem::replace(&mut state, State::Idle);
            if let State::Processing {
                target,
                focus_changed,
                cancel,
            } = previous
            {
                if cancel.is_canceled() || stopped.load(Ordering::Acquire) {
                    report("Ready.");
                    continue;
                }
                match result {
                    Ok(transcript) => {
                        let warning = if transcript.cleanup_failed {
                            "Cleanup failed; using original transcription. "
                        } else {
                            ""
                        };
                        let insertion = if focus_changed {
                            Err(anyhow::anyhow!("Focus changed during dictation"))
                        } else {
                            insert(&transcript.text, target, stopped, config.insertion)
                        };
                        match insertion {
                            Ok(()) => {
                                recovery = None;
                                feedback.show(
                                    "Inserted",
                                    overlay::GREEN,
                                    target.window,
                                    Some(Duration::from_secs(1)),
                                );
                                report(&format!("{warning}Inserted. Ready."));
                            }
                            Err(error) => {
                                recovery = Some(transcript.text);
                                feedback.cue(Cue::Error);
                                feedback.show(
                                    "Text retained · Ctrl+Alt+V to insert",
                                    overlay::AMBER,
                                    target.window,
                                    Some(Duration::from_secs(4)),
                                );
                                report(&format!(
                                    "{warning}{error:#}. Text retained in memory. Focus the intended field and press Ctrl+Alt+V to retry, or Ctrl+Alt+Esc to discard. If any text was inserted, remove it before retrying."
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        feedback.cue(Cue::Error);
                        feedback.show(
                            "Transcription failed",
                            overlay::RED,
                            target.window,
                            Some(Duration::from_secs(3)),
                        );
                        report(&format!(
                            "Transcription failed: {error:#}. Ready to record again."
                        ));
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(15));
    }
    Ok(())
}

fn finish_recording(
    state: &mut State,
    pipeline: &Arc<Pipeline>,
    tx: &mpsc::Sender<Result<Transcript>>,
    report: &impl Fn(&str),
    feedback: &mut Feedback,
) {
    let State::Recording {
        capture,
        target,
        focus_changed,
        ..
    } = std::mem::replace(state, State::Idle)
    else {
        return;
    };
    feedback.cue(Cue::Stop);
    match capture.finish() {
        Ok(wav) => {
            let pipeline = Arc::clone(pipeline);
            let tx = tx.clone();
            let canceled = Arc::new(AtomicBool::new(false));
            let worker_canceled = Arc::clone(&canceled);
            *state = State::Processing {
                target,
                focus_changed,
                cancel: CancelOnDrop(canceled),
            };
            thread::spawn(move || {
                let _ =
                    tx.send(pipeline.transcribe_cancelable(wav, "dictation.wav", &worker_canceled));
            });
            feedback.show("Transcribing…", overlay::AMBER, target.window, None);
            report("Transcribing…");
        }
        Err(error) => {
            feedback.show(
                "Recording discarded",
                overlay::RED,
                target.window,
                Some(Duration::from_secs(3)),
            );
            report(&format!("Recording discarded: {error:#}"));
        }
    }
}

fn insert(text: &str, target: Target, stopped: &AtomicBool, method: Insertion) -> Result<()> {
    // Let the user release the shortcut before synthesizing ordinary text.
    let started = Instant::now();
    loop {
        ensure!(!stopped.load(Ordering::Acquire), "Dictation disabled");
        ensure!(
            unsafe { GetAsyncKeyState(VK_ESCAPE as i32) } >= 0,
            "Insertion canceled"
        );
        ensure!(
            Target::current() == Some(target),
            "Target field changed; insertion skipped"
        );
        let held = [
            VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN, VK_SPACE, 0x56,
        ]
        .iter()
        .any(|&key| unsafe { GetAsyncKeyState(key as i32) } < 0);
        if !held {
            break;
        }
        ensure!(
            started.elapsed() < Duration::from_secs(2),
            "Release the shortcut keys before insertion"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let text = insertion_text(text)?;
    let units: Vec<_> = text.encode_utf16().collect();
    ensure!(
        units.len() <= 16_000,
        "Transcript too long for one insertion"
    );
    if method == Insertion::Paste {
        // Anything but plain text could not be restored, so type instead.
        if let Some(saved) = clipboard::save()? {
            return paste(&units, target, stopped, saved);
        }
    }
    // Unicode events preserve the clipboard and do not synthesize Enter shortcuts.
    for (typed, &unit) in units.iter().enumerate() {
        let interrupted = if stopped.load(Ordering::Acquire) {
            Some("dictation disabled")
        } else if unsafe { GetAsyncKeyState(VK_ESCAPE as i32) } < 0 {
            Some("Esc pressed")
        } else if Target::current() != Some(target) {
            Some("target field changed")
        } else {
            None
        };
        if let Some(reason) = interrupted {
            if typed == 0 {
                bail!("Insertion skipped: {reason}");
            }
            bail!(
                "Insertion stopped after {typed} of {} characters: {reason}",
                units.len()
            );
        }
        let events = [0, KEYEVENTF_KEYUP].map(|up| key_event(0, unit, KEYEVENTF_UNICODE | up));
        send(&events)?;
        thread::sleep(TYPING_INTERVAL);
    }
    Ok(())
}

/// Prepare provider-controlled text for insertion. Line endings become `\n`,
/// so a lone CR is never typed or pasted. Bidi embeddings, overrides, and
/// isolates (U+202A–U+202E, U+2066–U+2069) and the zero-width characters
/// U+200B, U+200C, U+2060, and U+FEFF are removed: they could reorder or hide
/// inserted text. The zero-width joiner U+200D stays because emoji sequences
/// need it. Any other control character except tab rejects the transcript.
fn insertion_text(text: &str) -> Result<String> {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                output.push('\n');
            }
            '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200B}'
            | '\u{200C}'
            | '\u{2060}'
            | '\u{FEFF}' => {}
            '\n' | '\t' => output.push(c),
            c if c.is_control() => bail!("Transcript contains unsupported control characters"),
            c => output.push(c),
        }
    }
    Ok(output)
}

fn key_event(key: u16, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(events: &[INPUT]) -> Result<()> {
    // SAFETY: events contains initialized INPUT structures and lives through SendInput.
    let sent = unsafe {
        SendInput(
            events.len() as u32,
            events.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    ensure!(
        sent == events.len() as u32,
        "Windows accepted {sent}/{} input events; the target may be elevated",
        events.len()
    );
    Ok(())
}

fn paste(
    units: &[u16],
    target: Target,
    stopped: &AtomicBool,
    saved: clipboard::Saved,
) -> Result<()> {
    clipboard::set(units, true)?;
    let pasted = if stopped.load(Ordering::Acquire) || Target::current() != Some(target) {
        Err(anyhow::anyhow!("Target field changed; insertion skipped"))
    } else {
        send(&[
            key_event(VK_CONTROL, 0, 0),
            key_event(0x56, 0, 0),
            key_event(0x56, 0, KEYEVENTF_KEYUP),
            key_event(VK_CONTROL, 0, KEYEVENTF_KEYUP),
        ])
    };
    if pasted.is_ok() {
        thread::sleep(PASTE_SETTLE);
    }
    let restored = clipboard::restore(saved);
    pasted?;
    restored
        .map_err(|error| error.context("Text was pasted, but the clipboard could not be restored"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_text_normalises_line_endings() {
        assert_eq!(insertion_text("a\r\nb\rc\nd").unwrap(), "a\nb\nc\nd");
        assert_eq!(insertion_text("a\r\r\nb").unwrap(), "a\n\nb");
        assert_eq!(insertion_text("tab\there").unwrap(), "tab\there");
    }

    #[test]
    fn insertion_text_strips_bidi_and_zero_width_characters() {
        let text =
            "\u{202A}a\u{202B}b\u{202C}c\u{202D}d\u{202E}e\u{2066}f\u{2067}g\u{2068}h\u{2069}";
        assert_eq!(insertion_text(text).unwrap(), "abcdefgh");
        assert_eq!(
            insertion_text("\u{FEFF}zero\u{200B}width\u{200C}chars\u{2060}").unwrap(),
            "zerowidthchars"
        );
    }

    #[test]
    fn insertion_text_keeps_zero_width_joiner() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(insertion_text(family).unwrap(), family);
    }

    #[test]
    fn insertion_text_rejects_other_control_characters() {
        for text in ["a\u{0}b", "a\u{1B}[31mb", "a\u{7F}", "a\u{85}b"] {
            assert!(insertion_text(text).is_err(), "{text:?}");
        }
    }
}
