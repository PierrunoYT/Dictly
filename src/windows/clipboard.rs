//! Optional paste insertion: put the transcript on the clipboard, send Ctrl+V,
//! then restore the previous text. Only plain-text clipboards can be restored
//! faithfully, so other content makes the caller type instead.

use super::wide;
use anyhow::{Result, bail, ensure};
use std::{ptr::null_mut, thread, time::Duration};
use windows_sys::Win32::{
    Foundation::GlobalFree,
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, OpenClipboard,
            RegisterClipboardFormatW, SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
    },
};

const CF_TEXT: u32 = 1;
const CF_OEMTEXT: u32 = 7;
const CF_UNICODETEXT: u32 = 13;
const CF_LOCALE: u32 = 16;

/// Clipboard contents before pasting.
pub enum Saved {
    Empty,
    /// Text and its CF_LOCALE identifier, when the clipboard carried one.
    Text {
        units: Vec<u16>,
        locale: Option<u32>,
    },
}

struct Open;

impl Open {
    fn new() -> Result<Self> {
        for _ in 0..10 {
            // SAFETY: a null owner associates the clipboard with this task.
            if unsafe { OpenClipboard(null_mut()) } != 0 {
                return Ok(Self);
            }
            thread::sleep(Duration::from_millis(20));
        }
        bail!("The clipboard is in use by another app")
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: the clipboard was opened by Open::new on this thread.
        unsafe {
            CloseClipboard();
        }
    }
}

/// Save plain-text clipboard contents; None when anything else is present.
pub fn save() -> Result<Option<Saved>> {
    let _open = Open::new()?;
    let mut format = 0;
    let mut has_text = false;
    let mut has_locale = false;
    loop {
        // SAFETY: the clipboard is open.
        format = unsafe { EnumClipboardFormats(format) };
        match format {
            0 => break,
            CF_UNICODETEXT => has_text = true,
            CF_LOCALE => has_locale = true,
            CF_TEXT | CF_OEMTEXT => {}
            _ => return Ok(None),
        }
    }
    if !has_text {
        return Ok(Some(Saved::Empty));
    }
    // SAFETY: the clipboard owns the handles; each lock is released before closing.
    unsafe {
        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle.is_null() {
            return Ok(None);
        }
        let pointer = GlobalLock(handle) as *const u16;
        if pointer.is_null() {
            return Ok(None);
        }
        let capacity = GlobalSize(handle) / 2;
        let units = std::slice::from_raw_parts(pointer, capacity);
        let text = units[..units.iter().position(|&unit| unit == 0).unwrap_or(capacity)].to_vec();
        GlobalUnlock(handle);
        let mut locale = None;
        let handle = if has_locale {
            GetClipboardData(CF_LOCALE)
        } else {
            null_mut()
        };
        if !handle.is_null() && GlobalSize(handle) >= 4 {
            let pointer = GlobalLock(handle) as *const u32;
            if !pointer.is_null() {
                locale = Some(pointer.read_unaligned());
                GlobalUnlock(handle);
            }
        }
        Ok(Some(Saved::Text {
            units: text,
            locale,
        }))
    }
}

/// Replace the clipboard with `units`. Private text is marked so Windows
/// clipboard history, cloud sync, and clipboard monitors skip it.
pub fn set(units: &[u16], private: bool) -> Result<()> {
    write(units, None, private)
}

fn write(units: &[u16], locale: Option<u32>, private: bool) -> Result<()> {
    let _open = Open::new()?;
    // SAFETY: the clipboard is open; ownership of each successfully set
    // allocation passes to the system, and failed ones are freed here.
    unsafe {
        ensure!(EmptyClipboard() != 0, "Cannot clear the clipboard");
        let memory = GlobalAlloc(GMEM_MOVEABLE, (units.len() + 1) * 2);
        ensure!(!memory.is_null(), "Cannot allocate clipboard memory");
        let pointer = GlobalLock(memory) as *mut u16;
        if pointer.is_null() {
            GlobalFree(memory);
            bail!("Cannot write clipboard memory");
        }
        pointer.copy_from_nonoverlapping(units.as_ptr(), units.len());
        *pointer.add(units.len()) = 0;
        GlobalUnlock(memory);
        if SetClipboardData(CF_UNICODETEXT, memory).is_null() {
            GlobalFree(memory);
            bail!("Cannot write to the clipboard");
        }
        // Without CF_LOCALE, Windows derives one from the current keyboard
        // layout, which changes how CF_TEXT is synthesized for other apps.
        if let Some(locale) = locale {
            set_dword(CF_LOCALE, locale);
        }
        if private {
            for name in [
                "ExcludeClipboardContentFromMonitorProcessing",
                "CanIncludeInClipboardHistory",
                "CanUploadToCloudClipboard",
            ] {
                let format = RegisterClipboardFormatW(wide(name).as_ptr());
                // A zero DWORD means "no" for the two Can* formats.
                if format != 0 {
                    set_dword(format, 0);
                }
            }
        }
    }
    Ok(())
}

/// Best effort: put a DWORD on the clipboard, freeing it if Windows refuses.
///
/// # Safety
/// The clipboard must be open and emptied by this thread.
unsafe fn set_dword(format: u32, value: u32) {
    // SAFETY: per the caller; a successfully set allocation belongs to the system.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, 4);
        if memory.is_null() {
            return;
        }
        let pointer = GlobalLock(memory) as *mut u32;
        if pointer.is_null() {
            GlobalFree(memory);
            return;
        }
        pointer.write_unaligned(value);
        GlobalUnlock(memory);
        if SetClipboardData(format, memory).is_null() {
            GlobalFree(memory);
        }
    }
}

pub fn restore(saved: Saved) -> Result<()> {
    match saved {
        Saved::Text { units, locale } => write(&units, locale, false),
        Saved::Empty => {
            let _open = Open::new()?;
            // SAFETY: the clipboard is open.
            ensure!(
                unsafe { EmptyClipboard() } != 0,
                "Cannot clear the clipboard"
            );
            Ok(())
        }
    }
}
