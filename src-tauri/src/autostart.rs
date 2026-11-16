//! Start at login through `tauri-plugin-autostart` (Linux: an XDG autostart
//! `.desktop` entry, macOS: a LaunchAgent, Windows: the Run registry key). The
//! entry runs the app with `--autostart`, so it starts in the tray. The
//! decisions live in `core::autostart`.

use crate::core::autostart::{enable_by_default, AUTOSTART_ARG};
use crate::core::settings;
use crate::state::AppState;
use std::sync::{Mutex, MutexGuard};
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn plugin() -> TauriPlugin<Wry> {
    tauri_plugin_autostart::Builder::new().arg(AUTOSTART_ARG).build()
}

/// Startup, after the settings are loaded: turn start at login on the first
/// time (its default). A failure leaves the flag unset, so the next start tries again.
pub fn init(app: &AppHandle) {
    let state = app.state::<AppState>();
    // Settings are read, changed and saved under the same lock as shortcut operations.
    let _op = lock(&state.shortcut_op);
    if !enable_by_default(lock(&state.settings).autostart_default_applied, !cfg!(debug_assertions)) {
        return;
    }
    if let Err(e) = app.autolaunch().enable() {
        eprintln!("start at login: {e}");
        return;
    }
    let copy = {
        let mut s = lock(&state.settings);
        s.autostart_default_applied = true;
        s.clone()
    };
    let path = lock(&state.settings_path).clone();
    if let Some(path) = path {
        if let Err(e) = settings::save(&path, &copy) {
            eprintln!("start at login: cannot save settings: {e}");
        }
    }
}

/// Whether the login entry exists now.
pub fn is_enabled(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// The tray's "Start at login" was clicked.
pub fn toggle(app: &AppHandle) {
    let manager = app.autolaunch();
    let result = if is_enabled(app) { manager.disable() } else { manager.enable() };
    if let Err(e) = result {
        eprintln!("start at login: {e}");
    }
    crate::tray::refresh(app);
}
