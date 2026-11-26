//! Global shortcuts, focus tracking, and key state on X11. Wayland sessions
//! block all three for ordinary apps, so dictation requires an X11 session.

use crate::dictation::Shortcut;
use anyhow::{Context, Result, bail};
use std::sync::OnceLock;
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xproto::{
            Atom, AtomEnum, ConnectionExt, GrabMode, Keycode, Keysym, ModMask, Timestamp, Window,
        },
    },
    rust_connection::RustConnection,
};

const XK_SPACE: Keysym = 0x0020;
const XK_V: Keysym = 0x0076;
const XK_ESCAPE: Keysym = 0xff1b;
const XK_SHIFT: [Keysym; 2] = [0xffe1, 0xffe2];
const XK_CONTROL: [Keysym; 2] = [0xffe3, 0xffe4];
const XK_ALT: [Keysym; 2] = [0xffe9, 0xffea];
const XK_SUPER: [Keysym; 2] = [0xffeb, 0xffec];

/// Connection for queries, shared by the dictation thread's calls.
struct Display {
    connection: RustConnection,
    root: Window,
    active_window: Atom,
    pid: Atom,
    /// Keysym of each keycode (first column of the keyboard mapping).
    keysyms: Vec<(Keycode, Keysym)>,
}

fn connect() -> Result<(RustConnection, usize)> {
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|session| session == "wayland") {
        bail!(
            "Live dictation on Linux needs an X11 session: Wayland does not let apps register global shortcuts or type into other windows. Log in with an X11 (Xorg) session."
        );
    }
    x11rb::connect(None).context("Cannot connect to the X11 display (is DISPLAY set?)")
}

fn display() -> Option<&'static Display> {
    static DISPLAY: OnceLock<Option<Display>> = OnceLock::new();
    DISPLAY
        .get_or_init(|| {
            let (connection, screen) = connect().ok()?;
            let root = connection.setup().roots[screen].root;
            let atom = |name: &[u8]| -> Option<Atom> {
                Some(connection.intern_atom(false, name).ok()?.reply().ok()?.atom)
            };
            let active_window = atom(b"_NET_ACTIVE_WINDOW")?;
            let pid = atom(b"_NET_WM_PID")?;
            let keysyms = keysyms(&connection).ok()?;
            Some(Display {
                connection,
                root,
                active_window,
                pid,
                keysyms,
            })
        })
        .as_ref()
}

fn keysyms(connection: &RustConnection) -> Result<Vec<(Keycode, Keysym)>> {
    let setup = connection.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let mapping = connection
        .get_keyboard_mapping(min, max - min + 1)?
        .reply()?;
    let per = mapping.keysyms_per_keycode.max(1) as usize;
    Ok(mapping
        .keysyms
        .chunks(per)
        .zip(min..=max)
        .map(|(syms, code)| (code, syms[0]))
        .collect())
}

fn keycodes(keysyms: &[(Keycode, Keysym)], wanted: &[Keysym]) -> Vec<Keycode> {
    keysyms
        .iter()
        .filter(|(_, sym)| wanted.contains(sym))
        .map(|&(code, _)| code)
        .collect()
}

/// Whether any key with one of `keysyms` is physically down.
fn any_down(keysyms: &[Keysym]) -> bool {
    let Some(display) = display() else {
        return false;
    };
    let Ok(keymap) = display
        .connection
        .query_keymap()
        .map_err(anyhow::Error::from)
        .and_then(|cookie| Ok(cookie.reply()?))
    else {
        return false;
    };
    keycodes(&display.keysyms, keysyms)
        .iter()
        .any(|&code| keymap.keys[code as usize / 8] & (1 << (code % 8)) != 0)
}

/// Whether Ctrl, Alt, and Space are all still down.
pub fn shortcut_held() -> bool {
    any_down(&XK_CONTROL) && any_down(&XK_ALT) && any_down(&[XK_SPACE])
}

pub fn escape_down() -> bool {
    any_down(&[XK_ESCAPE])
}

/// Whether any modifier or shortcut key is still down, which would turn
/// synthesized text into shortcuts.
pub fn shortcut_keys_down() -> bool {
    any_down(
        &[
            &XK_CONTROL[..],
            &XK_ALT,
            &XK_SHIFT,
            &XK_SUPER,
            &[XK_SPACE, XK_V],
        ]
        .concat(),
    )
}

/// The active top-level window and the window with keyboard focus.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Target {
    window: Window,
    focus: Window,
}

impl Target {
    pub fn current() -> Option<Self> {
        let display = display()?;
        let connection = &display.connection;
        let focus = connection.get_input_focus().ok()?.reply().ok()?.focus;
        // 0 is None and 1 is PointerRoot: no window has the keyboard.
        if focus <= 1 || focus == display.root {
            return None;
        }
        let window = connection
            .get_property(
                false,
                display.root,
                display.active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()?
            .reply()
            .ok()?
            .value32()
            .and_then(|mut values| values.next())
            .filter(|&window| window != 0)
            .unwrap_or(focus);
        // Never record for or insert into Dictly's own settings/API-key fields.
        if [window, focus]
            .iter()
            .any(|&window| window_pid(display, window) == Some(std::process::id()))
        {
            return None;
        }
        Some(Self { window, focus })
    }
}

/// `_NET_WM_PID` of `window` or its nearest ancestor that has one.
fn window_pid(display: &Display, mut window: Window) -> Option<u32> {
    let connection = &display.connection;
    while window != display.root && window != 0 {
        let pid = connection
            .get_property(false, window, display.pid, AtomEnum::CARDINAL, 0, 1)
            .ok()?
            .reply()
            .ok()?
            .value32()
            .and_then(|mut values| values.next());
        if pid.is_some() {
            return pid;
        }
        window = connection.query_tree(window).ok()?.reply().ok()?.parent;
    }
    None
}

/// Passive grabs of Ctrl+Alt+Space, Esc, and V on the root window, on their
/// own connection so dropping them releases the keys.
pub struct Shortcuts {
    connection: RustConnection,
    root: Window,
    keys: Vec<(Keycode, Shortcut)>,
    /// Time of each key's last release: X11 auto-repeat sends a release and
    /// a press with the same timestamp, which is not a new press.
    released: Vec<(Keycode, Timestamp)>,
    down: Vec<Keycode>,
    unavailable: Vec<&'static str>,
}

/// Grabs must also match with Caps Lock and Num Lock (Mod2) on.
fn locks() -> [ModMask; 4] {
    [
        ModMask::from(0u16),
        ModMask::LOCK,
        ModMask::M2,
        ModMask::LOCK | ModMask::M2,
    ]
}

impl Shortcuts {
    pub fn register() -> Result<Self> {
        let (connection, screen) = connect()?;
        let root = connection.setup().roots[screen].root;
        let keysyms = keysyms(&connection)?;
        let mut shortcuts = Self {
            connection,
            root,
            keys: vec![],
            released: vec![],
            down: vec![],
            unavailable: vec![],
        };
        for (keysym, shortcut, name) in [
            (XK_SPACE, Shortcut::Toggle, "Space"),
            (XK_ESCAPE, Shortcut::Cancel, "Esc"),
            (XK_V, Shortcut::Recover, "V"),
        ] {
            let code = *keycodes(&keysyms, &[keysym])
                .first()
                .with_context(|| format!("The keyboard has no {name} key"))?;
            let mut grabbed = true;
            for lock in locks() {
                let modifiers = ModMask::CONTROL | ModMask::M1 | lock;
                grabbed &= shortcuts
                    .connection
                    .grab_key(
                        true,
                        root,
                        modifiers,
                        code,
                        GrabMode::ASYNC,
                        GrabMode::ASYNC,
                    )?
                    .check()
                    .is_ok();
            }
            if grabbed {
                shortcuts.keys.push((code, shortcut));
                continue;
            }
            // Xfce and KDE bind Ctrl+Alt+Esc to xkill by default. Only the
            // recording shortcut is essential; the others are reported.
            for lock in locks() {
                let _ = shortcuts.connection.ungrab_key(
                    code,
                    root,
                    ModMask::CONTROL | ModMask::M1 | lock,
                );
            }
            if shortcut == Shortcut::Toggle {
                bail!(
                    "Cannot register Ctrl+Alt+{name}. Another app or instance may own this shortcut."
                );
            }
            shortcuts.unavailable.push(name);
        }
        Ok(shortcuts)
    }

    /// Shortcut keys ("Esc", "V") another app owns, so they do nothing.
    pub fn unavailable(&self) -> &[&'static str] {
        &self.unavailable
    }

    /// The next new shortcut press, or `None` once no events are pending.
    pub fn poll(&mut self) -> Option<Shortcut> {
        while let Ok(Some(event)) = self.connection.poll_for_event() {
            match event {
                Event::KeyPress(press) => {
                    let Some(&(_, shortcut)) =
                        self.keys.iter().find(|(code, _)| *code == press.detail)
                    else {
                        continue;
                    };
                    let repeat = self.down.contains(&press.detail)
                        || self
                            .released
                            .iter()
                            .any(|&(code, time)| code == press.detail && time == press.time);
                    self.released.retain(|&(code, _)| code != press.detail);
                    if !self.down.contains(&press.detail) {
                        self.down.push(press.detail);
                    }
                    if !repeat {
                        return Some(shortcut);
                    }
                }
                Event::KeyRelease(release) => {
                    self.down.retain(|&code| code != release.detail);
                    self.released.retain(|&(code, _)| code != release.detail);
                    self.released.push((release.detail, release.time));
                }
                _ => {}
            }
        }
        None
    }
}

impl Drop for Shortcuts {
    fn drop(&mut self) {
        for &(code, _) in &self.keys {
            for lock in locks() {
                let _ = self.connection.ungrab_key(
                    code,
                    self.root,
                    ModMask::CONTROL | ModMask::M1 | lock,
                );
            }
        }
        let _ = self.connection.flush();
    }
}
