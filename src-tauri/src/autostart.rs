//! Start at login through `tauri-plugin-autostart` (Linux: an XDG autostart
//! `.desktop` entry, macOS: a LaunchAgent, Windows: the Run registry key). The
//! entry runs the app with `--autostart`, so it starts in the tray. The
//! decisions live in `core::launch`.

use crate::core::launch::{applied_after, enable_by_default, tray_toggle_enabled, AutostartEvent, AUTOSTART_ARG};
use crate::core::settings;
use crate::state::{lock, AppState};
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt;

const RELEASE_BUILD: bool = !cfg!(debug_assertions);

pub fn plugin() -> TauriPlugin<Wry> {
    tauri_plugin_autostart::Builder::new().arg(AUTOSTART_ARG).build()
}

/// Records `event` in `Settings::autostart_default_applied` and saves the
/// settings when that changed. Runs under `shortcut_op`, the lock every
/// settings read-modify-save holds.
fn record(app: &AppHandle, event: AutostartEvent) {
    let state = app.state::<AppState>();
    let _op = lock(&state.shortcut_op);
    let copy = {
        let mut s = lock(&state.settings);
        let applied = applied_after(s.autostart_default_applied, event);
        if applied == s.autostart_default_applied {
            return;
        }
        s.autostart_default_applied = applied;
        s.clone()
    };
    let path = lock(&state.settings_path).clone();
    if let Some(path) = path {
        if let Err(e) = settings::save(&path, &copy) {
            eprintln!("start at login: cannot save settings: {e}");
        }
    }
}

/// Startup, after the settings are loaded: turn start at login on the first
/// time (its default). A failure is retried on the next start, unless the
/// user has chosen in the tray by then.
pub fn init(app: &AppHandle) {
    let applied = lock(&app.state::<AppState>().settings).autostart_default_applied;
    if !enable_by_default(applied, RELEASE_BUILD) {
        return;
    }
    let event = match app.autolaunch().enable() {
        Ok(()) => AutostartEvent::DefaultEnabled,
        Err(e) => {
            eprintln!("start at login: {e}");
            AutostartEvent::DefaultFailed
        }
    };
    record(app, event);
}

/// Whether the login entry exists now.
pub fn is_enabled(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// Whether the tray's "Start at login" can be clicked (never in a debug build).
pub fn can_toggle() -> bool {
    tray_toggle_enabled(RELEASE_BUILD)
}

/// The tray's "Start at login" was clicked.
pub fn toggle(app: &AppHandle) {
    if !can_toggle() {
        return;
    }
    let manager = app.autolaunch();
    let result = if is_enabled(app) { manager.disable() } else { manager.enable() };
    match result {
        Ok(()) => record(app, AutostartEvent::UserChose),
        Err(e) => eprintln!("start at login: {e}"),
    }
    crate::tray::refresh(app);
}
