//! Where the mouse pointer is, for placing the popup.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;

/// Pointer position in root-window (physical) pixels on `$DISPLAY`, or None
/// when the display cannot be reached.
pub fn cursor_position() -> Option<(i32, i32)> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let p = conn.query_pointer(root).ok()?.reply().ok()?;
    Some((i32::from(p.root_x), i32::from(p.root_y)))
}
