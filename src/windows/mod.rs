pub mod clipboard;
pub mod credential;
mod overlay;
mod sound;

pub use overlay::Overlay;
pub use sound::play;

use crate::{
    config::Config,
    dictation::{Shortcut, run_controlled},
    pipeline::Pipeline,
};
use anyhow::{Result, bail, ensure};
use std::{
    mem::size_of,
    ptr::null_mut,
    sync::atomic::{AtomicBool, Ordering},
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

/// Modifier names shown with the shortcut keys.
pub const KEYS: &str = "Ctrl+Alt";

const TOGGLE: i32 = 1;
const CANCEL: i32 = 2;
const RECOVER: i32 = 3;

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
            ensure!(!handle.is_null(), "Cannot check for another running Dictly");
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                bail!("Dictly is already running. Use the open Dictly window or close it first.");
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

/// Global shortcuts bound to this thread's message queue, and the names of
/// those another app owns.
pub struct Shortcuts(Vec<i32>, Vec<&'static str>);

impl Shortcuts {
    pub fn register() -> Result<Self> {
        let mut keys = Self(vec![], vec![]);
        for (id, key) in [(TOGGLE, VK_SPACE), (CANCEL, VK_ESCAPE), (RECOVER, 0x56)] {
            // SAFETY: null HWND binds the hotkey to this thread's message queue.
            let registered = unsafe {
                RegisterHotKey(
                    null_mut(),
                    id,
                    MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
                    key as u32,
                )
            };
            let name = match id {
                TOGGLE => "Space",
                CANCEL => "Esc",
                _ => "V",
            };
            if registered != 0 {
                keys.0.push(id);
                continue;
            }
            // Only the recording shortcut is essential; the others are reported.
            ensure!(
                id != TOGGLE,
                "Cannot register Ctrl+Alt+{name}. Another app or instance may own this shortcut."
            );
            keys.1.push(name);
        }
        Ok(keys)
    }

    /// Shortcut keys ("Esc", "V") another app owns, so they do nothing.
    pub fn unavailable(&self) -> &[&'static str] {
        &self.1
    }
}

impl Shortcuts {
    /// The next shortcut press, dispatching other messages (such as overlay
    /// painting) on the way. `None` once the queue is empty.
    pub fn poll(&mut self) -> Option<Shortcut> {
        // SAFETY: MSG is plain data, for which all-zero bytes are valid.
        let mut message: MSG = unsafe { std::mem::zeroed() };
        // SAFETY: message is a valid writable MSG.
        while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
            if message.message == WM_QUIT {
                return Some(Shortcut::Quit);
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
                TOGGLE => return Some(Shortcut::Toggle),
                CANCEL => return Some(Shortcut::Cancel),
                RECOVER => return Some(Shortcut::Recover),
                _ => {}
            }
        }
        None
    }
}

impl Drop for Shortcuts {
    fn drop(&mut self) {
        for &id in &self.0 {
            // SAFETY: these IDs were registered by this thread.
            unsafe {
                UnregisterHotKey(null_mut(), id);
            }
        }
    }
}

/// The foreground window and its focused control.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub(super) window: HWND,
    focus: HWND,
}

impl Target {
    pub fn current() -> Option<Self> {
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

/// Whether Ctrl, Alt, and Space are all still down.
pub fn shortcut_held() -> bool {
    [VK_CONTROL, VK_MENU, VK_SPACE]
        .iter()
        .all(|&key| key_down(key))
}

fn key_down(key: u16) -> bool {
    // SAFETY: GetAsyncKeyState only reads global key state.
    unsafe { GetAsyncKeyState(key as i32) < 0 }
}

pub fn escape_down() -> bool {
    key_down(VK_ESCAPE)
}

/// Whether any modifier or shortcut key is still down, which would turn
/// synthesized text into shortcuts.
pub fn shortcut_keys_down() -> bool {
    [
        VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN, VK_SPACE, 0x56,
    ]
    .iter()
    .any(|&key| key_down(key))
}

/// Type one character as Unicode input, which preserves the clipboard.
pub fn type_char(c: char) -> Result<()> {
    let (events, len) = char_events(c);
    send(&events[..len])
}

pub fn send_paste() -> Result<()> {
    send(&[
        key_event(VK_CONTROL, 0, 0),
        key_event(0x56, 0, 0),
        key_event(0x56, 0, KEYEVENTF_KEYUP),
        key_event(VK_CONTROL, 0, KEYEVENTF_KEYUP),
    ])
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

/// Key down and up for each UTF-16 unit of `c`. Both halves of a surrogate
/// pair go into one SendInput call, so an interruption or other input cannot
/// split them and leave a lone high surrogate in the target.
fn char_events(c: char) -> ([INPUT; 4], usize) {
    let mut buffer = [0; 2];
    let units = c.encode_utf16(&mut buffer);
    let mut events = [key_event(0, 0, 0); 4];
    for (index, &unit) in units.iter().enumerate() {
        events[2 * index] = key_event(0, unit, KEYEVENTF_UNICODE);
        events[2 * index + 1] = key_event(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
    }
    (events, units.len() * 2)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_events_keep_surrogate_pairs_in_one_batch() {
        let keys = |c| {
            let (events, len) = char_events(c);
            events[..len]
                .iter()
                // SAFETY: char_events fills only the keyboard variant.
                .map(|event| unsafe { (event.Anonymous.ki.wScan, event.Anonymous.ki.dwFlags) })
                .collect::<Vec<_>>()
        };
        let up = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
        assert_eq!(keys('a'), [(0x61, KEYEVENTF_UNICODE), (0x61, up)]);
        assert_eq!(
            keys('\u{1F600}'),
            [
                (0xD83D, KEYEVENTF_UNICODE),
                (0xD83D, up),
                (0xDE00, KEYEVENTF_UNICODE),
                (0xDE00, up),
            ]
        );
    }
}
