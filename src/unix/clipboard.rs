//! Paste insertion on macOS and Linux. Only plain text can be saved and
//! restored; when the clipboard holds no text (empty, an image, or files),
//! dictation types instead so nothing is lost.

use anyhow::{Result, anyhow};
use arboard::Clipboard;
use std::sync::{Mutex, OnceLock};

#[cfg(target_os = "macos")]
use arboard::SetExtApple;
#[cfg(target_os = "linux")]
use arboard::SetExtLinux;

pub struct Saved(String);

/// One clipboard for the life of the process: on X11 the owner must stay
/// alive to serve pasted and restored text to other apps.
fn with_clipboard<T>(action: impl FnOnce(&mut Clipboard) -> Result<T>) -> Result<T> {
    static CLIPBOARD: OnceLock<Mutex<Option<Clipboard>>> = OnceLock::new();
    let mut clipboard = CLIPBOARD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| anyhow!("Clipboard unavailable"))?;
    if clipboard.is_none() {
        *clipboard =
            Some(Clipboard::new().map_err(|error| anyhow!("Cannot open the clipboard: {error}"))?);
    }
    action(clipboard.as_mut().unwrap())
}

/// Save the clipboard's text; None when it holds no text.
pub fn save() -> Result<Option<Saved>> {
    with_clipboard(|clipboard| match clipboard.get_text() {
        Ok(text) => Ok(Some(Saved(text))),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(error) => Err(anyhow!("Cannot read the clipboard: {error}")),
    })
}

/// Replace the clipboard with `text`. Private text is marked so clipboard
/// managers that honor the hint (macOS concealed type, KDE Klipper) skip it.
pub fn set(text: &str, private: bool) -> Result<()> {
    with_clipboard(|clipboard| {
        let set = clipboard.set();
        let set = if private {
            set.exclude_from_history()
        } else {
            set
        };
        set.text(text)
            .map_err(|error| anyhow!("Cannot write the clipboard: {error}"))
    })
}

pub fn restore(saved: Saved) -> Result<()> {
    set(&saved.0, false)
}
