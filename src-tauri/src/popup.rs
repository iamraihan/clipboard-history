//! Popup window lifecycle: show at the cursor, hide on blur or pick.

use crate::platform::{BackendError, FocusTracker, FocusedWindow};
use crate::state::{lock, AppState};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

pub const POPUP: &str = "popup";
/// Emitted (no payload) each time the popup is shown.
const POPUP_OPENED: &str = "popup-opened";

/// Top-left for a window of `size` opened at `cursor`, kept fully inside the
/// monitor (`mon_pos`, `mon_size`). All values are physical pixels. A window
/// larger than the monitor sticks to the monitor's top-left.
pub fn clamp_to_monitor(
    cursor: (i32, i32),
    size: (i32, i32),
    mon_pos: (i32, i32),
    mon_size: (i32, i32),
) -> (i32, i32) {
    let clamp = |c: i32, s: i32, p: i32, m: i32| c.min(p + m - s).max(p);
    (
        clamp(cursor.0, size.0, mon_pos.0, mon_size.0),
        clamp(cursor.1, size.1, mon_pos.1, mon_size.1),
    )
}

/// The pointer, only on a real X11 session; on XWayland it would be stale.
#[cfg(target_os = "linux")]
fn cursor_position() -> Option<(i32, i32)> {
    use crate::platform::linux::{choose_from_env, pointer, LinuxBackendChoice};
    match choose_from_env(|k| std::env::var(k).ok()) {
        LinuxBackendChoice::X11 => pointer::cursor_position(),
        _ => None,
    }
}

#[cfg(not(target_os = "linux"))]
fn cursor_position() -> Option<(i32, i32)> {
    None
}

fn place_at_cursor(win: &WebviewWindow) {
    let Some((cx, cy)) = cursor_position() else { return };
    let Ok(size) = win.outer_size() else { return };
    let Ok(Some(mon)) = win.monitor_from_point(f64::from(cx), f64::from(cy)) else { return };
    let (mp, ms) = (mon.position(), mon.size());
    let w = i32::try_from(size.width).unwrap_or(i32::MAX);
    let h = i32::try_from(size.height).unwrap_or(i32::MAX);
    let mw = i32::try_from(ms.width).unwrap_or(i32::MAX);
    let mh = i32::try_from(ms.height).unwrap_or(i32::MAX);
    let (x, y) = clamp_to_monitor((cx, cy), (w, h), (mp.x, mp.y), (mw, mh));
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

/// How often `activate` asks the window manager before giving up; each ask
/// waits up to `X11Focus`'s own timeout.
const ACTIVATE_ATTEMPTS: usize = 3;
const ACTIVATE_GAP: Duration = Duration::from_millis(30);

/// How asking the window manager to activate the popup ended.
#[derive(Debug)]
enum Activation {
    Active,
    /// The popup was hidden meanwhile (Esc, a second Super+V): stopped asking.
    Abandoned,
    /// Every attempt failed while the popup stayed visible.
    Refused(BackendError),
}

impl PartialEq for Activation {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Active, Self::Active) | (Self::Abandoned, Self::Abandoned) | (Self::Refused(_), Self::Refused(_))
        )
    }
}

/// Makes `popup` the active window, retrying while the window manager has
/// not managed the freshly mapped window yet. `shown` is checked before each
/// attempt, so a popup hidden meanwhile is not activated again.
fn activate(
    focus: &dyn FocusTracker,
    popup: &FocusedWindow,
    attempts: usize,
    shown: impl Fn() -> bool,
) -> Activation {
    let mut last = BackendError("no attempt".into());
    for i in 0..attempts {
        if i > 0 {
            std::thread::sleep(ACTIVATE_GAP);
        }
        if !shown() {
            return Activation::Abandoned;
        }
        match focus.restore(popup) {
            Ok(()) => return Activation::Active,
            Err(e) => last = e,
        }
    }
    Activation::Refused(last)
}

/// The popup's X11 window id; None on Wayland and other platforms.
#[cfg(target_os = "linux")]
fn x11_window(win: &WebviewWindow) -> Option<FocusedWindow> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match win.window_handle().ok()?.as_raw() {
        // `c_ulong` is u64 here but u32 on 32-bit targets.
        #[allow(clippy::useless_conversion)]
        RawWindowHandle::Xlib(h) => Some(FocusedWindow(u64::from(h.window))),
        RawWindowHandle::Xcb(h) => Some(FocusedWindow(u64::from(h.window.get()))),
        _ => None,
    }
}

#[cfg(not(target_os = "linux"))]
fn x11_window(_win: &WebviewWindow) -> Option<FocusedWindow> {
    None
}

/// Asks the window manager to activate the popup as a user action (a pager
/// request). GTK's own `present` stamps the request with the last user time
/// GTK saw, which is older than the shortcut's key press, so Mutter's
/// focus-stealing prevention refuses it and only flags the popup "demands
/// attention": arrow keys and Esc then go to the previous app.
///
/// If the window manager still refuses, the popup is hidden: on screen without
/// the keyboard it could not be closed with Esc and would never see a blur.
fn take_focus(win: WebviewWindow, focus: Option<Arc<dyn FocusTracker>>, popup: Option<FocusedWindow>) {
    let (Some(focus), Some(popup)) = (focus, popup) else { return };
    // Off the main thread: the activation waits for the window manager.
    std::thread::spawn(move || {
        let shown = || win.is_visible().unwrap_or(false);
        if let Activation::Refused(e) = activate(focus.as_ref(), &popup, ACTIVATE_ATTEMPTS, shown) {
            eprintln!("popup focus: {e}; hiding the popup");
            let _ = win.hide();
        }
    });
}

pub fn hide(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(POPUP) {
        let _ = win.hide();
    }
}

/// Hidden: remember the focused window, place at the cursor, show, focus and
/// tell the UI. Visible: hide.
///
/// Runs on the main thread whatever the caller's thread. The GNOME shortcut
/// arrives on the single-instance D-Bus thread; from there `show` is only
/// queued, so tao's `set_focus` still sees a hidden window and drops the
/// request, leaving the popup on screen without keyboard focus.
pub fn toggle(app: &AppHandle) {
    let handle = app.clone();
    if let Err(e) = app.run_on_main_thread(move || toggle_now(&handle)) {
        eprintln!("popup: {e}");
    }
}

fn toggle_now(app: &AppHandle) {
    let Some(win) = app.get_webview_window(POPUP) else { return };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        return;
    }
    let state = app.state::<AppState>();
    let focus = lock(&state.focus).clone();
    let current = focus.as_ref().and_then(|f| f.current());
    *lock(&state.focused_before_popup) = current;
    place_at_cursor(&win);
    let _ = win.show();
    let _ = win.set_focus();
    take_focus(win.clone(), focus, x11_window(&win));
    let _ = app.emit(POPUP_OPENED, ());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Fails `restore` the first `failures` times, counting calls.
    struct Flaky {
        failures: usize,
        calls: Mutex<usize>,
    }

    impl FocusTracker for Flaky {
        fn current(&self) -> Option<FocusedWindow> {
            None
        }
        fn restore(&self, _: &FocusedWindow) -> Result<(), BackendError> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            if *calls > self.failures { Ok(()) } else { Err(BackendError("not yet".into())) }
        }
    }

    #[test]
    fn activation_retries_while_the_window_manager_catches_up() {
        let wm = Flaky { failures: 2, calls: Mutex::new(0) };
        assert_eq!(activate(&wm, &FocusedWindow(7), 3, || true), Activation::Active);
        assert_eq!(*wm.calls.lock().unwrap(), 3);
    }

    #[test]
    fn activation_gives_up_after_the_last_attempt() {
        let wm = Flaky { failures: 9, calls: Mutex::new(0) };
        assert!(matches!(activate(&wm, &FocusedWindow(7), 3, || true), Activation::Refused(_)));
        assert_eq!(*wm.calls.lock().unwrap(), 3);
    }

    #[test]
    fn activation_stops_once_the_popup_is_hidden() {
        // Esc or a second Super+V while the WM still refuses: stop asking.
        let wm = Flaky { failures: 9, calls: Mutex::new(0) };
        let shown = Mutex::new(2u32); // the check before the first ask says "shown", the next says "hidden"
        let still_shown = || {
            let mut left = shown.lock().unwrap();
            *left = left.saturating_sub(1);
            *left > 0
        };
        assert_eq!(activate(&wm, &FocusedWindow(7), 3, still_shown), Activation::Abandoned);
        assert_eq!(*wm.calls.lock().unwrap(), 1);
    }

    #[test]
    fn popup_stays_on_the_monitor() {
        let size = (360, 480);
        let mon = ((0, 0), (1920, 1080));
        assert_eq!(clamp_to_monitor((100, 100), size, mon.0, mon.1), (100, 100));
        assert_eq!(clamp_to_monitor((1900, 1070), size, mon.0, mon.1), (1560, 600));
        assert_eq!(clamp_to_monitor((-5, -5), size, mon.0, mon.1), (0, 0));
    }

    #[test]
    fn clamp_respects_a_monitor_offset_and_tiny_monitors() {
        assert_eq!(clamp_to_monitor((2000, 50), (360, 480), (1920, 0), (1280, 720)), (2000, 50));
        assert_eq!(clamp_to_monitor((3100, 700), (360, 480), (1920, 0), (1280, 720)), (2840, 240));
        assert_eq!(clamp_to_monitor((50, 50), (360, 480), (0, 0), (300, 400)), (0, 0));
    }
}
