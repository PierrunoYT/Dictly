//! Global shortcuts, focus tracking, and key state on macOS. The shortcuts are
//! read by a session event tap on its own thread, which needs the
//! Accessibility permission that typing into other apps needs anyway.

use crate::dictation::Shortcut;
use anyhow::{Result, anyhow, bail};
use core_foundation::{
    base::{CFType, TCFType},
    boolean::CFBoolean,
    dictionary::CFDictionary,
    number::CFNumber,
    runloop::{CFRunLoop, kCFRunLoopCommonModes, kCFRunLoopDefaultMode},
    string::{CFString, CFStringRef},
};
use core_graphics::{
    event::{
        CGEventField, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventType, CallbackResult, EventField,
    },
    window::{
        copy_window_info, kCGNullWindowID, kCGWindowLayer, kCGWindowListExcludeDesktopElements,
        kCGWindowListOptionOnScreenOnly, kCGWindowNumber, kCGWindowOwnerPID,
    },
};
use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicPtr, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

// Virtual key codes from HIToolbox/Events.h.
const SPACE: u16 = 49;
const ESCAPE: u16 = 53;
const V: u16 = 9;
const SHIFT: [u16; 2] = [56, 60];
const CONTROL: [u16; 2] = [59, 62];
const OPTION: [u16; 2] = [58, 61];
const COMMAND: [u16; 2] = [55, 54];

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

/// kCGEventSourceStateHIDSystemState: the physical keyboard.
const HID_SYSTEM_STATE: i32 = 1;

fn any_down(keys: &[u16]) -> bool {
    // SAFETY: CGEventSourceKeyState only reads global key state.
    keys.iter()
        .any(|&key| unsafe { CGEventSourceKeyState(HID_SYSTEM_STATE, key) })
}

/// Whether Control, Option, and Space are all still down.
pub fn shortcut_held() -> bool {
    any_down(&CONTROL) && any_down(&OPTION) && any_down(&[SPACE])
}

pub fn escape_down() -> bool {
    any_down(&[ESCAPE])
}

/// Whether any modifier or shortcut key is still down, which would turn
/// synthesized text into shortcuts.
pub fn shortcut_keys_down() -> bool {
    any_down(&[&CONTROL[..], &OPTION, &SHIFT, &COMMAND, &[SPACE, V]].concat())
}

/// The frontmost normal window: its owning process and window number.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pid: i64,
    window: i64,
}

impl Target {
    pub fn current() -> Option<Self> {
        let windows = copy_window_info(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        )?;
        // SAFETY: the keys are CoreGraphics' static CFString constants.
        let (layer, pid, number) = unsafe {
            (
                CFString::wrap_under_get_rule(kCGWindowLayer),
                CFString::wrap_under_get_rule(kCGWindowOwnerPID),
                CFString::wrap_under_get_rule(kCGWindowNumber),
            )
        };
        // Windows are listed front to back; layer 0 skips the menu bar, the
        // Dock, and other system overlays.
        for item in windows.iter() {
            // SAFETY: every element of the window list is a CFDictionary.
            let info: CFDictionary<CFString, CFType> =
                unsafe { CFDictionary::wrap_under_get_rule(*item as _) };
            let number_of = |key: &CFString| {
                info.find(key)
                    .and_then(|value| value.downcast::<CFNumber>())
                    .and_then(|value| value.to_i64())
            };
            if number_of(&layer) != Some(0) {
                continue;
            }
            let target = Self {
                pid: number_of(&pid)?,
                window: number_of(&number)?,
            };
            // Never record for or insert into Dictly's own settings/API-key fields.
            if target.pid == std::process::id() as i64 {
                return None;
            }
            return Some(target);
        }
        None
    }
}

/// Ask macOS whether Dictly may control the keyboard, showing the system
/// prompt that leads to the Accessibility settings when it may not.
fn trusted() -> bool {
    // SAFETY: the key is a static CFString; the dictionary lives through the call.
    unsafe {
        let options = CFDictionary::from_CFType_pairs(&[(
            CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt),
            CFBoolean::true_value(),
        )]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef() as *const c_void)
    }
}

/// Control+Option+Space, Esc, and V, read and swallowed by an event tap.
pub struct Shortcuts {
    events: mpsc::Receiver<Shortcut>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Shortcuts {
    pub fn register() -> Result<Self> {
        if !trusted() {
            bail!(
                "Dictly needs Accessibility access to use Control+Option shortcuts and type into other apps. Allow the app that started Dictly (Dictly, or Terminal for the command line) in System Settings › Privacy & Security › Accessibility, then enable dictation again."
            );
        }
        let (events, receiver) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let port = Arc::new(AtomicPtr::<c_void>::new(std::ptr::null_mut()));
            let tap_port = Arc::clone(&port);
            // Shortcut keys whose press was swallowed, so their release is too.
            let swallowed = [SPACE, ESCAPE, V].map(|_| AtomicBool::new(false));
            let tap = CGEventTap::new(
                CGEventTapLocation::Session,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                vec![CGEventType::KeyDown, CGEventType::KeyUp],
                move |_, kind, event| {
                    if matches!(
                        kind,
                        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
                    ) {
                        // SAFETY: the port belongs to this live tap.
                        unsafe { CGEventTapEnable(tap_port.load(Ordering::Acquire), true) };
                        return CallbackResult::Keep;
                    }
                    let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                    let Some(index) = [SPACE, ESCAPE, V]
                        .iter()
                        .position(|&key| key as i64 == code)
                    else {
                        return CallbackResult::Keep;
                    };
                    match kind {
                        CGEventType::KeyDown => {
                            let flags = event.get_flags();
                            let ours = flags.contains(CGEventFlags::CGEventFlagControl)
                                && flags.contains(CGEventFlags::CGEventFlagAlternate)
                                && !flags.contains(CGEventFlags::CGEventFlagCommand)
                                && !flags.contains(CGEventFlags::CGEventFlagShift);
                            if !ours {
                                return CallbackResult::Keep;
                            }
                            swallowed[index].store(true, Ordering::Relaxed);
                            const AUTOREPEAT: CGEventField = EventField::KEYBOARD_EVENT_AUTOREPEAT;
                            if event.get_integer_value_field(AUTOREPEAT) == 0 {
                                let _ = events.send(
                                    [Shortcut::Toggle, Shortcut::Cancel, Shortcut::Recover][index],
                                );
                            }
                            CallbackResult::Drop
                        }
                        _ if swallowed[index].swap(false, Ordering::Relaxed) => {
                            CallbackResult::Drop
                        }
                        _ => CallbackResult::Keep,
                    }
                },
            );
            let Ok(tap) = tap else {
                let _ = ready.send(Err(anyhow!(
                    "Cannot watch the keyboard for Control+Option shortcuts. Check Dictly's Accessibility access in System Settings › Privacy & Security."
                )));
                return;
            };
            port.store(
                tap.mach_port().as_concrete_TypeRef() as *mut c_void,
                Ordering::Release,
            );
            let Ok(source) = tap.mach_port().create_runloop_source(0) else {
                let _ = ready.send(Err(anyhow!("Cannot start the keyboard event tap")));
                return;
            };
            // SAFETY: kCFRunLoopCommonModes is a static CFString.
            CFRunLoop::get_current().add_source(&source, unsafe { kCFRunLoopCommonModes });
            tap.enable();
            let _ = ready.send(Ok(()));
            while !stopped.load(Ordering::Acquire) {
                // SAFETY: kCFRunLoopDefaultMode is a static CFString.
                CFRunLoop::run_in_mode(
                    unsafe { kCFRunLoopDefaultMode },
                    Duration::from_millis(100),
                    false,
                );
            }
        });
        let result = started
            .recv()
            .unwrap_or_else(|_| Err(anyhow!("The keyboard event tap stopped unexpectedly")));
        let shortcuts = Self {
            events: receiver,
            stop,
            thread: Some(thread),
        };
        result.map(|()| shortcuts)
    }

    /// The next shortcut press, or `None` once none is pending.
    pub fn poll(&mut self) -> Option<Shortcut> {
        self.events.try_recv().ok()
    }
}

impl Drop for Shortcuts {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
