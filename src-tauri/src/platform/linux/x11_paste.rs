//! Paste back on X11: `X11Focus` finds and restores the window the user came
//! from, `X11Paster` sends Ctrl+V (Ctrl+Shift+V into terminals) to it with XTest.
//!
//! Each opens its own X connection (separate from the clipboard thread's),
//! connects at construction, and reconnects lazily if the connection died.
//! Neither holds a lock across a wait.

use crate::platform::{BackendError, FocusTracker, FocusedWindow, Paster};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, InputFocus, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::{CURRENT_TIME, NONE};

const KEYSYM_CONTROL_L: u32 = 0xffe3;
const KEYSYM_SHIFT_L: u32 = 0xffe1;
const KEYSYM_V: u32 = 0x0076;
const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
/// How long `restore` waits for the target to become the active window.
const ACTIVE_TIMEOUT: Duration = Duration::from_millis(300);
const ACTIVE_POLL: Duration = Duration::from_millis(10);
/// "Source indication" 2 of _NET_ACTIVE_WINDOW: a pager, i.e. a user action.
const SOURCE_PAGER: u32 = 2;

/// Which key chord pastes into a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chord {
    CtrlV,
    /// Terminals: Ctrl+V is a literal control character there.
    CtrlShiftV,
}

/// Pure choice from a WM_CLASS name (instance or class, any case).
pub fn paste_chord_for(class: &str) -> Chord {
    const TERMINALS: &[&str] = &[
        "gnome-terminal-server", "kgx", "org.gnome.console", "konsole", "xterm", "uxterm", "alacritty", "kitty",
        "wezterm", "org.wezfurlong.wezterm", "tilix", "terminator", "xfce4-terminal", "urxvt", "foot", "ptyxis",
    ];
    if TERMINALS.iter().any(|t| class.eq_ignore_ascii_case(t)) { Chord::CtrlShiftV } else { Chord::CtrlV }
}

/// WM_CLASS holds "instance\0class\0"; a terminal under either name counts.
pub fn chord_for_wm_class(raw: &[u8]) -> Chord {
    let terminal = raw
        .split(|b| *b == 0)
        .filter_map(|part| std::str::from_utf8(part).ok())
        .any(|name| paste_chord_for(name) == Chord::CtrlShiftV);
    if terminal { Chord::CtrlShiftV } else { Chord::CtrlV }
}

/// The window's WM_CLASS; empty when unset or unreadable (then plain Ctrl+V).
fn wm_class(c: &RustConnection, w: Window) -> Vec<u8> {
    c.get_property(false, w, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 64)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|r| r.value)
        .unwrap_or_default()
}

fn err(what: &str, e: impl std::fmt::Display) -> BackendError {
    BackendError(format!("{what}: {e}"))
}

/// One X connection to `display`, opened at construction and reopened on use
/// after a failure.
struct Link {
    display: Option<String>,
    conn: Mutex<Option<Arc<RustConnection>>>,
}

impl Link {
    fn open(display: Option<&str>) -> Result<Self, BackendError> {
        let link = Self { display: display.map(str::to_owned), conn: Mutex::new(None) };
        link.get()?;
        Ok(link)
    }

    fn get(&self) -> Result<(Arc<RustConnection>, Window), BackendError> {
        let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            let (conn, _) = x11rb::connect(self.display.as_deref()).map_err(|e| err("cannot connect to X", e))?;
            *guard = Some(Arc::new(conn));
        }
        let conn = guard.clone().ok_or_else(|| BackendError("no X connection".into()))?;
        let root = conn.setup().roots.first().map(|s| s.root).ok_or_else(|| BackendError("X has no screen".into()))?;
        Ok((conn, root))
    }

    /// Runs `f`; a failure drops the connection so the next call reconnects.
    fn with<T>(
        &self,
        f: impl FnOnce(&RustConnection, Window) -> Result<T, BackendError>,
    ) -> Result<T, BackendError> {
        let (conn, root) = self.get()?;
        let r = f(&conn, root);
        if r.is_err() && conn.flush().is_err() {
            *self.conn.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
        r
    }
}

fn atom(c: &RustConnection, name: &str) -> Result<Atom, BackendError> {
    let r = c.intern_atom(false, name.as_bytes()).map_err(|e| err("intern atom", e))?;
    Ok(r.reply().map_err(|e| err("intern atom", e))?.atom)
}

/// `_NET_ACTIVE_WINDOW` of the root: the active toplevel, if there is a WM
/// that sets it and one is active.
fn active_window(c: &RustConnection, root: Window) -> Result<Option<Window>, BackendError> {
    let prop = atom(c, "_NET_ACTIVE_WINDOW")?;
    let r = c
        .get_property(false, root, prop, AtomEnum::WINDOW, 0, 1)
        .map_err(|e| err("read active window", e))?
        .reply()
        .map_err(|e| err("read active window", e))?;
    Ok(r.value32().and_then(|mut v| v.next()).filter(|w| *w != NONE))
}

/// The window with the input focus; None for "none" and "pointer root".
fn input_focus(c: &RustConnection) -> Result<Option<Window>, BackendError> {
    let r = c
        .get_input_focus()
        .map_err(|e| err("read input focus", e))?
        .reply()
        .map_err(|e| err("read input focus", e))?;
    Ok(Some(r.focus).filter(|w| *w > 1))
}

/// Does the WM list `_NET_ACTIVE_WINDOW` in `_NET_SUPPORTED`?
fn wm_supports_active_window(c: &RustConnection, root: Window) -> Result<bool, BackendError> {
    let (supported, wanted) = (atom(c, "_NET_SUPPORTED")?, atom(c, "_NET_ACTIVE_WINDOW")?);
    let r = c
        .get_property(false, root, supported, AtomEnum::ATOM, 0, 4096)
        .map_err(|e| err("read _NET_SUPPORTED", e))?
        .reply()
        .map_err(|e| err("read _NET_SUPPORTED", e))?;
    Ok(r.value32().is_some_and(|mut v| v.any(|a| a == wanted)))
}

fn window_exists(c: &RustConnection, w: Window) -> bool {
    c.get_window_attributes(w).ok().and_then(|cookie| cookie.reply().ok()).is_some()
}

fn to_window(w: &FocusedWindow) -> Result<Window, BackendError> {
    Window::try_from(w.0).map_err(|_| BackendError("not an X11 window id".into()))
}

/// Finds and restores the window the user came from.
pub struct X11Focus {
    link: Link,
}

impl X11Focus {
    /// Connects to `display` (None: `$DISPLAY`).
    pub fn connect(display: Option<&str>) -> Result<Self, BackendError> {
        Ok(Self { link: Link::open(display)? })
    }
}

impl FocusTracker for X11Focus {
    fn current(&self) -> Option<FocusedWindow> {
        self.link
            .with(|c, root| match active_window(c, root)? {
                Some(w) => Ok(Some(w)),
                // No WM, or none active: whatever has the input focus.
                None => input_focus(c),
            })
            .ok()
            .flatten()
            .map(|w| FocusedWindow(u64::from(w)))
    }

    /// Returns Ok only once the target is the active (focused) window.
    fn restore(&self, target: &FocusedWindow) -> Result<(), BackendError> {
        let w = to_window(target)?;
        self.link.with(|c, root| {
            if !window_exists(c, w) {
                return Err(BackendError("the window is gone".into()));
            }
            let ewmh = wm_supports_active_window(c, root)?;
            if ewmh {
                let ev = ClientMessageEvent::new(
                    32,
                    w,
                    atom(c, "_NET_ACTIVE_WINDOW")?,
                    [SOURCE_PAGER, CURRENT_TIME, 0, 0, 0],
                );
                let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
                c.send_event(false, root, mask, ev).map_err(|e| err("activate window", e))?;
            } else {
                c.set_input_focus(InputFocus::PARENT, w, CURRENT_TIME)
                    .map_err(|e| err("focus window", e))?
                    .check()
                    .map_err(|e| err("focus window", e))?;
            }
            c.flush().map_err(|e| err("activate window", e))?;
            let deadline = Instant::now() + ACTIVE_TIMEOUT;
            loop {
                let now_active = if ewmh { active_window(c, root)? } else { input_focus(c)? };
                if now_active == Some(w) {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(BackendError("the window did not become active".into()));
                }
                std::thread::sleep(ACTIVE_POLL);
            }
        })
    }
}

/// Sends Ctrl+V with XTest. The keys go to whichever window has the focus,
/// so call `X11Focus::restore` first; `paste` still refuses a vanished target.
pub struct X11Paster {
    link: Link,
}

impl X11Paster {
    /// Connects to `display` (None: `$DISPLAY`).
    pub fn connect(display: Option<&str>) -> Result<Self, BackendError> {
        Ok(Self { link: Link::open(display)? })
    }
}

/// The keycode that produces `keysym`, from the server's keyboard mapping.
fn keycode_for(c: &RustConnection, keysym: u32) -> Result<u8, BackendError> {
    let setup = c.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = c
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(|e| err("read keyboard mapping", e))?
        .reply()
        .map_err(|e| err("read keyboard mapping", e))?;
    let per = usize::from(map.keysyms_per_keycode).max(1);
    // Lowest column first: the plain (unshifted) key beats a shifted alias.
    for col in 0..per {
        for (i, row) in map.keysyms.chunks(per).enumerate() {
            if row.get(col) == Some(&keysym) {
                return u8::try_from(usize::from(min) + i).map_err(|e| err("keycode", e));
            }
        }
    }
    Err(BackendError(format!("keysym {keysym:#x} is not on this keyboard")))
}

fn key_event(c: &RustConnection, kind: u8, keycode: u8) -> Result<(), BackendError> {
    c.xtest_fake_input(kind, keycode, 0, NONE, 0, 0, 0)
        .map_err(|e| err("send key", e))?
        .check()
        .map_err(|e| err("send key", e))
}

impl Paster for X11Paster {
    /// Ctrl+V, or Ctrl+Shift+V when the target is a terminal. `plain` makes no difference here: the caller already wrote
    /// only the plain text to the clipboard.
    fn paste(&self, target: &FocusedWindow, _plain: bool) -> Result<(), BackendError> {
        let w = to_window(target)?;
        self.link.with(|c, root| {
            if !window_exists(c, w) {
                return Err(BackendError("the window is gone".into()));
            }
            let chord = chord_for_wm_class(&wm_class(c, w));
            let ctrl = keycode_for(c, KEYSYM_CONTROL_L)?;
            let v = keycode_for(c, KEYSYM_V)?;
            let mut modifiers = vec![ctrl];
            if chord == Chord::CtrlShiftV {
                modifiers.push(keycode_for(c, KEYSYM_SHIFT_L)?);
            }
            // Last check before the keys, so they never reach another window.
            let active = if wm_supports_active_window(c, root)? { active_window(c, root)? } else { input_focus(c)? };
            if active != Some(w) {
                return Err(BackendError("the target window is not the active window".into()));
            }
            let mut pressed = Vec::new();
            let mut typed = Ok(());
            for m in &modifiers {
                typed = key_event(c, KEY_PRESS, *m);
                if typed.is_err() {
                    break;
                }
                pressed.push(*m);
            }
            if typed.is_ok() {
                typed = key_event(c, KEY_PRESS, v).and_then(|()| key_event(c, KEY_RELEASE, v));
            }
            // Modifiers are released in reverse, even if `v` failed, or they would stay stuck.
            let mut released = Ok(());
            for m in pressed.iter().rev() {
                let r = key_event(c, KEY_RELEASE, *m);
                released = released.and(r);
            }
            c.flush().map_err(|e| err("send key", e))?;
            typed.and(released)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::x11_test_support::PrivateX;
    use super::*;
    use std::time::{Duration, Instant};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::*;
    use x11rb::protocol::Event;
    use x11rb::wrapper::ConnectionExt as _;
    use x11rb::rust_connection::RustConnection;
    use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME};

    const CONTROL_L: u32 = 0xffe3;
    const KEY_V: u32 = 0x76;

    /// An app window that records key events, on its own connection.
    struct App {
        conn: RustConnection,
        win: Window,
    }

    impl App {
        fn new(display: &str) -> Self {
            let (conn, screen) = x11rb::connect(Some(display)).unwrap();
            let root = conn.setup().roots[screen].root;
            let win = conn.generate_id().unwrap();
            let aux = CreateWindowAux::new()
                .event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE);
            conn.create_window(
                COPY_DEPTH_FROM_PARENT, win, root, 0, 0, 16, 16, 0,
                WindowClass::INPUT_OUTPUT, 0, &aux,
            )
            .unwrap();
            conn.map_window(win).unwrap();
            conn.sync().unwrap();
            Self { conn, win }
        }

        fn focus(&self) {
            self.conn
                .set_input_focus(InputFocus::PARENT, self.win, CURRENT_TIME)
                .unwrap();
            self.conn.sync().unwrap();
        }

        /// Keysyms of key events seen as (pressed, keysym), waiting up to `wait`
        /// for `n` events.
        fn keys(&self, n: usize, wait: Duration) -> Vec<(bool, u32)> {
            let setup = self.conn.setup();
            let (min, max) = (setup.min_keycode, setup.max_keycode);
            let map = self
                .conn
                .get_keyboard_mapping(min, max - min + 1)
                .unwrap()
                .reply()
                .unwrap();
            let per = usize::from(map.keysyms_per_keycode);
            let sym = |kc: u8| map.keysyms[usize::from(kc - min) * per];
            let deadline = Instant::now() + wait;
            let mut out = Vec::new();
            while out.len() < n && Instant::now() < deadline {
                match self.conn.poll_for_event().unwrap() {
                    Some(Event::KeyPress(e)) => out.push((true, sym(e.detail))),
                    Some(Event::KeyRelease(e)) => out.push((false, sym(e.detail))),
                    Some(_) => {}
                    None => std::thread::sleep(Duration::from_millis(5)),
                }
            }
            out
        }
    }

    #[test]
    fn terminals_get_ctrl_shift_v_and_everything_else_ctrl_v() {
        for t in [
            "gnome-terminal-server", "kgx", "org.gnome.Console", "Konsole", "XTerm", "UXTerm", "Alacritty", "kitty",
            "org.wezfurlong.wezterm", "WezTerm", "Tilix", "terminator", "Xfce4-terminal", "URxvt", "foot", "Ptyxis",
        ] {
            assert_eq!(paste_chord_for(t), Chord::CtrlShiftV, "{t}");
        }
        for other in ["firefox", "Navigator", "gedit", "code", "", "terminal-emulator-lookalike"] {
            assert_eq!(paste_chord_for(other), Chord::CtrlV, "{other}");
        }
    }

    #[test]
    fn wm_class_property_checks_instance_and_class() {
        assert_eq!(chord_for_wm_class(b"gnome-terminal-server\0Gnome-terminal\0"), Chord::CtrlShiftV);
        assert_eq!(chord_for_wm_class(b"xterm\0XTerm\0"), Chord::CtrlShiftV);
        assert_eq!(chord_for_wm_class(b"navigator\0Firefox\0"), Chord::CtrlV);
        assert_eq!(chord_for_wm_class(b""), Chord::CtrlV);
    }

    const SHIFT_L: u32 = 0xffe1;

    fn set_wm_class(app: &App, class: &[u8]) {
        app.conn
            .change_property8(PropMode::REPLACE, app.win, AtomEnum::WM_CLASS, AtomEnum::STRING, class)
            .unwrap();
        app.conn.sync().unwrap();
    }

    #[test]
    fn terminal_window_gets_ctrl_shift_v() {
        let x = PrivateX::start();
        let app = App::new(&x.display);
        set_wm_class(&app, b"gnome-terminal-server\0Gnome-terminal\0");
        app.focus();
        let paster = X11Paster::connect(Some(&x.display)).expect("paster");
        paster.paste(&FocusedWindow(u64::from(app.win)), false).expect("paste");
        assert_eq!(
            app.keys(6, Duration::from_secs(5)),
            vec![
                (true, CONTROL_L),
                (true, SHIFT_L),
                (true, KEY_V),
                (false, KEY_V),
                (false, SHIFT_L),
                (false, CONTROL_L)
            ]
        );
    }

    #[test]
    fn xtest_sends_ctrl_v_to_focused_window() {
        let x = PrivateX::start();
        let app = App::new(&x.display);
        app.focus();
        let paster = X11Paster::connect(Some(&x.display)).expect("paster");
        paster.paste(&FocusedWindow(u64::from(app.win)), false).expect("paste");
        assert_eq!(
            app.keys(4, Duration::from_secs(5)),
            vec![(true, CONTROL_L), (true, KEY_V), (false, KEY_V), (false, CONTROL_L)]
        );
    }

    #[test]
    fn paste_refuses_when_focus_moved_away() {
        let x = PrivateX::start();
        let (target, other) = (App::new(&x.display), App::new(&x.display));
        other.focus();
        let paster = X11Paster::connect(Some(&x.display)).expect("paster");
        assert!(paster.paste(&FocusedWindow(u64::from(target.win)), false).is_err());
        assert!(other.keys(1, Duration::from_millis(300)).is_empty());
        assert!(target.keys(1, Duration::from_millis(100)).is_empty());
    }

    #[test]
    fn paste_is_skipped_when_target_window_is_gone() {
        let x = PrivateX::start();
        let bystander = App::new(&x.display);
        bystander.focus();
        let gone = App::new(&x.display);
        let id = FocusedWindow(u64::from(gone.win));
        gone.conn.destroy_window(gone.win).unwrap();
        gone.conn.sync().unwrap();

        let focus = X11Focus::connect(Some(&x.display)).expect("focus");
        let paster = X11Paster::connect(Some(&x.display)).expect("paster");
        assert!(focus.restore(&id).is_err());
        assert!(paster.paste(&id, false).is_err());
        assert!(bystander.keys(1, Duration::from_millis(300)).is_empty());
    }

    #[test]
    fn restore_focuses_the_target_without_a_window_manager() {
        let x = PrivateX::start();
        let (a, b) = (App::new(&x.display), App::new(&x.display));
        a.focus();
        let focus = X11Focus::connect(Some(&x.display)).expect("focus");
        assert_eq!(focus.current(), Some(FocusedWindow(u64::from(a.win))));
        focus.restore(&FocusedWindow(u64::from(b.win))).expect("restore");
        assert_eq!(focus.current(), Some(FocusedWindow(u64::from(b.win))));
    }
}
