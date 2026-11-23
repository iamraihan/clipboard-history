mod autostart;
mod commands;
pub mod core;
pub mod engine;
pub mod models;
pub mod platform;
mod shortcut;
mod popup;
pub mod state;
pub mod store;
mod tray;
mod updater;

use crate::core::clip::content_hash;
use crate::core::rules::Decision;
use crate::engine::Engine;
use crate::platform::ClipboardBackend;
use crate::state::{describe_outcome, now_unix_secs, retry_delays, AppState, StartupFailure, StartupReport};
use crate::store::{FileKey, HistoryStore, OpenReport, OsKeyring, StoreError};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

const HISTORY_FILE: &str = "history.db";
const KEY_FILE: &str = "history.key";
pub(crate) const SETTINGS_FILE: &str = "settings.json";
/// Emitted (no payload) whenever the history changed.
const HISTORY_CHANGED: &str = "history-changed";

/// The Backend for this platform, not yet watching.
#[cfg(target_os = "linux")]
fn make_backend() -> Result<Arc<dyn ClipboardBackend>, String> {
    use crate::platform::linux::{choose_from_env, LinuxBackendChoice, X11Backend};
    match choose_from_env(|k| std::env::var(k).ok()) {
        // `None`: connect to $DISPLAY (XWayland on GNOME Wayland).
        LinuxBackendChoice::X11 | LinuxBackendChoice::XWayland => X11Backend::connect(None)
            .map(|b| Arc::new(b) as Arc<dyn ClipboardBackend>)
            .map_err(|e| e.0),
        LinuxBackendChoice::Unsupported(why) => Err(why),
    }
}

#[cfg(target_os = "macos")]
fn make_backend() -> Result<Arc<dyn ClipboardBackend>, String> {
    use crate::platform::macos::{MacBackend, PasteboardName};
    Ok(Arc::new(MacBackend::new(PasteboardName::General)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn make_backend() -> Result<Arc<dyn ClipboardBackend>, String> {
    Err("This platform is not supported yet".into())
}

/// X11 only: the Paster and FocusTracker. XWayland and other sessions get
/// none (copy only; the user presses Ctrl+V).
#[cfg(target_os = "linux")]
fn make_paste(_app: &AppHandle) -> Option<(Arc<dyn platform::Paster>, Arc<dyn platform::FocusTracker>)> {
    use crate::platform::linux::{choose_from_env, x11_paste::{X11Focus, X11Paster}, LinuxBackendChoice};
    if choose_from_env(|k| std::env::var(k).ok()) != LinuxBackendChoice::X11 {
        return None;
    }
    let paste = X11Paster::connect(None).and_then(|p| Ok((p, X11Focus::connect(None)?)));
    match paste {
        Ok((p, f)) => Some((Arc::new(p), Arc::new(f))),
        Err(e) => {
            eprintln!("auto-paste is off: {e}");
            None
        }
    }
}

/// macOS: always present; `MacPaster::ready` says whether Accessibility is
/// granted. Hiding our app hands activation back to the previous one.
#[cfg(target_os = "macos")]
fn make_paste(app: &AppHandle) -> Option<(Arc<dyn platform::Paster>, Arc<dyn platform::FocusTracker>)> {
    use crate::platform::macos::{MacFocus, MacPaster};
    let handle = app.clone();
    // Through popup::hide, so an open settings window is never hidden.
    let focus = MacFocus::new(move || popup::hide(&handle));
    Some((Arc::new(MacPaster), Arc::new(focus)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn make_paste(_app: &AppHandle) -> Option<(Arc<dyn platform::Paster>, Arc<dyn platform::FocusTracker>)> {
    None
}

fn set_report(app: &AppHandle, f: impl FnOnce(&mut StartupReport)) {
    {
        let state = app.state::<AppState>();
        let mut report = state.report.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut report);
    }
    // The tray shows the report's problems; the lock is released first.
    tray::refresh(app);
}

/// Opens the store, or reports why not. The key file sits next to the database.
fn open_store(dir: &std::path::Path) -> Result<(HistoryStore, OpenReport), StoreError> {
    let keys = OsKeyring { fallback: FileKey { path: dir.join(KEY_FILE) } };
    HistoryStore::open(&dir.join(HISTORY_FILE), &keys, now_unix_secs())
}

/// Builds the engine around an open store, then starts the Backend forwarding
/// ClipEvents into it.
fn install(app: &AppHandle, store: HistoryStore, opened: &OpenReport, dir: &std::path::Path) {
    let settings = core::settings::load(&dir.join(SETTINGS_FILE));
    let handle = app.clone();
    // Runs under the engine lock: emit only, never touch the engine. No
    // Rust-side listener of "history-changed" may lock `state.engine`, because
    // `emit` runs listeners while the engine lock is held.
    let on_change = Box::new(move || {
        let _ = handle.emit(HISTORY_CHANGED, ());
    });
    let engine = Engine::new(store, settings, on_change);
    let state = app.state::<AppState>();
    *state.engine.lock().unwrap_or_else(|p| p.into_inner()) = Some(engine);
    set_report(app, |r| {
        r.key_in_file = opened.key_in_file;
        r.recovered_broken = opened.recovered_broken.is_some();
        r.key_unavailable = false;
    });
    start_watching(app);
    let _ = app.emit(HISTORY_CHANGED, ());
}

/// Starts the Backend (if there is one) and the thread that feeds its
/// ClipEvents to the engine.
fn start_watching(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (tx, rx) = channel();
    // Clone the Arc out so the lock is not held across `watch`.
    let Some(backend) = state.backend.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
        return;
    };
    let started = backend.watch(tx);
    if let Err(e) = started {
        eprintln!("clipboard watcher failed to start: {e}");
        set_report(app, |r| StartupFailure::Backend(e.0).record(r));
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        for event in rx {
            let hash = content_hash(&event.formats);
            let state = app.state::<AppState>();
            let mut guard = state.engine.lock().unwrap_or_else(|p| p.into_inner());
            let Some(engine) = guard.as_mut() else { continue };
            let outcome = engine.handle_event(event, now_unix_secs());
            let saved_id = match outcome {
                Ok(Decision::Insert) => engine.store.find_by_hash(hash).ok().flatten(),
                _ => None,
            };
            drop(guard);
            eprintln!("{}", describe_outcome(&outcome, saved_id));
        }
    });
}

/// Startup: settings, Backend, store. Never fails; problems go in the StartupReport.
fn start(app: &AppHandle) {
    let dir: PathBuf = match app.path().app_data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("no app data dir: {e}");
            set_report(app, |r| StartupFailure::NoDataDir(e.to_string()).record(r));
            return;
        }
    };
    match make_backend() {
        Ok(b) => *app.state::<AppState>().backend.lock().unwrap_or_else(|p| p.into_inner()) = Some(b),
        Err(why) => {
            eprintln!("no clipboard backend: {why}");
            set_report(app, |r| StartupFailure::Backend(why).record(r));
        }
    }
    if let Some((paster, focus)) = make_paste(app) {
        let state = app.state::<AppState>();
        *state.paster.lock().unwrap_or_else(|p| p.into_inner()) = Some(paster);
        *state.focus.lock().unwrap_or_else(|p| p.into_inner()) = Some(focus);
    }
    match open_store(&dir) {
        Ok((store, opened)) => install(app, store, &opened, &dir),
        Err(StoreError::KeyUnavailable(why)) => {
            eprintln!("history key unavailable, will retry: {why}");
            set_report(app, |r| r.key_unavailable = true);
            retry_open(app.clone(), dir);
        }
        Err(e) => {
            eprintln!("cannot open history: {e}");
            set_report(app, |r| StartupFailure::StoreOpen(e.to_string()).record(r));
        }
    }
}

/// Retries the open on a background thread (no lock is held while sleeping).
fn retry_open(app: AppHandle, dir: PathBuf) {
    std::thread::spawn(move || {
        for delay in retry_delays() {
            std::thread::sleep(delay);
            match open_store(&dir) {
                Ok((store, opened)) => {
                    install(&app, store, &opened, &dir);
                    return;
                }
                Err(StoreError::KeyUnavailable(_)) => {}
                Err(e) => {
                    eprintln!("cannot open history: {e}");
                    set_report(&app, |r| {
                        r.key_unavailable = false;
                        StartupFailure::StoreOpen(e.to_string()).record(r);
                    });
                    return;
                }
            }
        }
        eprintln!("history key still unavailable; giving up until the next start");
        set_report(&app, StartupReport::give_up_on_key);
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default()
        // First, so a second launch never gets as far as opening the store.
        // A second launch, with or without `--toggle`, opens (toggles) the popup.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // A second login entry while the app already runs changes nothing.
            if core::launch::launch_kind(&argv) != core::launch::Launch::Autostart {
                popup::toggle(app);
            }
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(autostart::plugin());
    if shortcut::uses_plugin() {
        builder = builder.plugin(tauri_plugin_global_shortcut::Builder::new().build());
    }
    builder
        .setup(|app| {
            app.manage(AppState::new());
            // macOS: a menu-bar app, no Dock icon; it can still activate its windows.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            start(app.handle());
            let plan = shortcut::init(app.handle());
            // Before the tray: the tray menu reads the update state.
            updater::start(app.handle());
            tray::build(app.handle())?;
            autostart::init(app.handle());
            let launch = core::launch::launch_kind(&std::env::args().collect::<Vec<_>>());
            if shortcut::opens_popup_at_launch(plan, launch) {
                // First launch from the shortcut or the app grid (not login): let the popup page load, then open it.
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    popup::toggle(&handle);
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_clips,
            commands::set_pinned,
            commands::delete_clip,
            commands::clear_unpinned,
            commands::paste_clip,
            commands::startup_report,
            commands::hide_popup,
            commands::settings_view,
            commands::first_run_choice,
            commands::hide_settings,
            commands::reset_shortcuts,
            commands::quit_choice,
        ])
        .on_window_event(|window, event| {
            // Hide the popup when it loses focus. In dev, focusing devtools hides it too.
            if window.label() == popup::POPUP {
                if let tauri::WindowEvent::Focused(false) = event {
                    popup::hide(window.app_handle());
                }
            }
            // Closing settings only hides it (the tray reopens it); an open
            // quit prompt closed this way counts as Cancel.
            if window.label() == shortcut::SETTINGS_WINDOW {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    shortcut::hide_settings(window.app_handle());
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Clipboard History");
}
