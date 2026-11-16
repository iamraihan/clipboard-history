//! Popup window lifecycle: show at the cursor, hide on blur or pick.

use crate::state::AppState;
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

pub fn hide(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(POPUP) {
        let _ = win.hide();
    }
}

/// Hidden: remember the focused window, place at the cursor, show, focus and
/// tell the UI. Visible: hide.
pub fn toggle(app: &AppHandle) {
    let Some(win) = app.get_webview_window(POPUP) else { return };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        return;
    }
    let state = app.state::<AppState>();
    let focus = state.focus.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let current = focus.and_then(|f| f.current());
    *state.focused_before_popup.lock().unwrap_or_else(|p| p.into_inner()) = current;
    place_at_cursor(&win);
    let _ = win.show();
    let _ = win.set_focus();
    let _ = app.emit(POPUP_OPENED, ());
}

#[cfg(test)]
mod tests {
    use super::*;

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
