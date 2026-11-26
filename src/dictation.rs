//! The live-dictation state machine shared by every desktop OS. The platform
//! module supplies shortcuts, focus tracking, key state, text input, the
//! clipboard, sounds, and the status overlay.

use crate::{
    capture::Capture,
    config::{Config, Insertion},
    pipeline::{Pipeline, Transcript},
    platform::{self, KEYS, Overlay, Shortcuts, Target, clipboard},
    sound::Cue,
};
use anyhow::{Result, bail, ensure};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// Some editors, Windows 11 Notepad among them, read the most recently
/// injected character when they process a keystroke, so a batch arrives as
/// repeats of its last character. Typing one character at a time with a short
/// pause lets each be consumed first; 1 ms was measured to be too short.
const TYPING_INTERVAL: Duration = Duration::from_millis(10);
/// Time for the target to read the clipboard after the paste shortcut before it is restored.
const PASTE_SETTLE: Duration = Duration::from_millis(400);
/// Holding the shortcut longer than this is push-to-talk: releasing it
/// finishes. A shorter tap keeps recording hands-free until the next press.
const HOLD_THRESHOLD: Duration = Duration::from_millis(500);

/// A global shortcut press, or a request from the OS to quit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shortcut {
    /// Modifiers + Space: start or finish recording.
    Toggle,
    /// Modifiers + Esc: cancel and clear retained text.
    Cancel,
    /// Modifiers + V: insert retained text.
    Recover,
    Quit,
}

/// Status overlay colors.
#[derive(Clone, Copy)]
pub enum Color {
    Red,
    Amber,
    Green,
    Gray,
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

    fn show(&mut self, text: &str, color: Color, near: Option<Target>, linger: Option<Duration>) {
        if let Some(overlay) = &self.overlay {
            overlay.show(text, color, near);
        }
        self.hide_at = linger.map(|linger| Instant::now() + linger);
    }

    fn cue(&self, cue: Cue) {
        if self.sounds {
            platform::play(cue);
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

/// Run live dictation until `stopped` is set, reporting status lines through `report`.
pub fn run_controlled(
    config: Config,
    pipeline: Pipeline,
    stopped: &AtomicBool,
    report: impl Fn(&str),
) -> Result<()> {
    let pipeline = Arc::new(pipeline);
    let mut shortcuts = Shortcuts::register()?;
    let (tx, rx) = mpsc::channel::<Result<Transcript>>();
    let mut state = State::Idle;
    let mut recovery: Option<String> = None;
    let mut feedback = Feedback::new(&config, &report);
    report(&format!(
        "Dictly — {KEYS}+Space: hold to talk, or tap to start/stop; {KEYS}+Esc: cancel/clear; {KEYS}+V: insert retained text."
    ));
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
        while let Some(shortcut) = shortcuts.poll() {
            if shortcut == Shortcut::Quit || stopped.load(Ordering::Acquire) {
                return Ok(());
            }
            match shortcut {
                Shortcut::Cancel => {
                    recovery = None;
                    feedback.cue(Cue::Cancel);
                    feedback.show(
                        "Canceled",
                        Color::Gray,
                        Target::current(),
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
                Shortcut::Toggle => match state {
                    State::Idle => {
                        if let Some(target) = Target::current() {
                            match Capture::start(
                                config.max_recording_seconds,
                                config.microphone.as_deref(),
                            ) {
                                Ok(capture) => {
                                    feedback.cue(Cue::Start);
                                    feedback.show("Recording…", Color::Red, Some(target), None);
                                    report(&format!(
                                        "Recording… ({}) Release the shortcut to finish, or after a quick tap press it again. {KEYS}+Esc discards.",
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
                                        Color::Red,
                                        Some(target),
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
                Shortcut::Recover if matches!(state, State::Idle) => {
                    if let Some(text) = &recovery {
                        if let Some(target) = Target::current() {
                            match insert(text, target, stopped, config.insertion) {
                                Ok(()) => {
                                    recovery = None;
                                    feedback.show(
                                        "Inserted",
                                        Color::Green,
                                        Some(target),
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
            let released = held_since.is_some() && !platform::shortcut_held();
            let push_to_talk =
                released && held_since.is_some_and(|at| at.elapsed() >= HOLD_THRESHOLD);
            if released && !push_to_talk {
                *held_since = None;
                feedback.show(
                    &format!("Recording · {KEYS}+Space to finish"),
                    Color::Red,
                    Some(*target),
                    None,
                );
                report(&format!(
                    "Hands-free recording. Press {KEYS}+Space to finish, {KEYS}+Esc to discard."
                ));
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
                        let warning = match &transcript.cleanup_error {
                            Some(error) => {
                                format!("Cleanup failed ({error}); using original transcription. ")
                            }
                            None => String::new(),
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
                                    Color::Green,
                                    Some(target),
                                    Some(Duration::from_secs(1)),
                                );
                                report(&format!("{warning}Inserted. Ready."));
                            }
                            Err(error) => {
                                recovery = Some(transcript.text);
                                feedback.cue(Cue::Error);
                                feedback.show(
                                    &format!("Text retained · {KEYS}+V to insert"),
                                    Color::Amber,
                                    Some(target),
                                    Some(Duration::from_secs(4)),
                                );
                                report(&format!(
                                    "{warning}{error:#}. Text retained in memory. Focus the intended field and press {KEYS}+V to retry, or {KEYS}+Esc to discard. If any text was inserted, remove it before retrying."
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        feedback.cue(Cue::Error);
                        feedback.show(
                            "Transcription failed",
                            Color::Red,
                            Some(target),
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
            feedback.show("Transcribing…", Color::Amber, Some(target), None);
            report("Transcribing…");
        }
        Err(error) => {
            feedback.show(
                "Recording discarded",
                Color::Red,
                Some(target),
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
        ensure!(!platform::escape_down(), "Insertion canceled");
        ensure!(
            Target::current() == Some(target),
            "Target field changed; insertion skipped"
        );
        if !platform::shortcut_keys_down() {
            break;
        }
        ensure!(
            started.elapsed() < Duration::from_secs(2),
            "Release the shortcut keys before insertion"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let text = insertion_text(text)?;
    ensure!(
        text.encode_utf16().count() <= 16_000,
        "Transcript too long for one insertion"
    );
    if method == Insertion::Paste {
        // Anything but plain text could not be restored, so type instead.
        if let Some(saved) = clipboard::save()? {
            return paste(&text, target, stopped, saved);
        }
    }
    // Synthesized characters preserve the clipboard and do not trigger Enter shortcuts.
    let total = text.chars().count();
    for (typed, c) in text.chars().enumerate() {
        let interrupted = if stopped.load(Ordering::Acquire) {
            Some("dictation disabled")
        } else if platform::escape_down() {
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
            bail!("Insertion stopped after {typed} of {total} characters: {reason}");
        }
        platform::type_char(c)?;
        thread::sleep(TYPING_INTERVAL);
    }
    Ok(())
}

fn paste(text: &str, target: Target, stopped: &AtomicBool, saved: clipboard::Saved) -> Result<()> {
    clipboard::set(text, true)?;
    let pasted = if stopped.load(Ordering::Acquire) || Target::current() != Some(target) {
        Err(anyhow::anyhow!("Target field changed; insertion skipped"))
    } else {
        platform::send_paste()
    };
    if pasted.is_ok() {
        thread::sleep(PASTE_SETTLE);
    }
    let restored = clipboard::restore(saved);
    pasted?;
    restored
        .map_err(|error| error.context("Text was pasted, but the clipboard could not be restored"))
}

/// Prepare provider-controlled text for insertion. Line endings become `\n`,
/// so a lone CR is never typed or pasted. Bidi embeddings, overrides, and
/// isolates (U+202A–U+202E, U+2066–U+2069) and the zero-width characters
/// U+200B, U+2060, and U+FEFF are removed: they could reorder or hide
/// inserted text. The zero-width joiner U+200D (emoji sequences) and
/// non-joiner U+200C (Persian and Indic spelling) stay. Any other control
/// character except tab rejects the transcript.
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
            | '\u{2060}'
            | '\u{FEFF}' => {}
            '\n' | '\t' => output.push(c),
            c if c.is_control() => bail!("Transcript contains unsupported control characters"),
            c => output.push(c),
        }
    }
    Ok(output)
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
            insertion_text("\u{FEFF}zero\u{200B}width\u{2060}").unwrap(),
            "zerowidth"
        );
        assert_eq!(
            insertion_text("می\u{200C}خواهم").unwrap(),
            "می\u{200C}خواهم"
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
