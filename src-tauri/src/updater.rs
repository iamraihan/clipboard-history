//! App updates from GitHub Releases: the app's only network access. Checks on
//! launch and once a day, and when the user clicks the tray's update line. A
//! newer release shows up at the top of the tray menu. The decisions (which
//! line, when to check) live in `core::update`.

use crate::core::update::{
    after_failed_check, click_action, should_check, tray_item, ClickAction, TrayUpdateItem, UpdateMethod, UpdateStatus,
};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

const METHOD: UpdateMethod = if cfg!(target_os = "macos") {
    UpdateMethod::ReleasePage
} else {
    UpdateMethod::InPlace
};

#[derive(Default)]
struct Inner {
    status: UpdateStatus,
    /// The release found by the last check; it carries the download.
    pending: Option<Update>,
}

/// Managed by Tauri.
#[derive(Default)]
pub struct UpdateState(Mutex<Inner>);

fn lock(app: &AppHandle) -> MutexGuard<'_, Inner> {
    app.state::<UpdateState>().inner().0.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts the periodic check. Debug builds (`pnpm tauri dev`) only check when
/// the user clicks the tray line.
pub fn start(app: &AppHandle) {
    app.manage(UpdateState::default());
    if cfg!(debug_assertions) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || loop {
        tauri::async_runtime::block_on(check(&app));
        std::thread::sleep(CHECK_EVERY);
    });
}

async fn check(app: &AppHandle) {
    if !should_check(&lock(app).status) {
        return;
    }
    let found = match app.updater() {
        Ok(updater) => updater.check().await,
        Err(e) => Err(e),
    };
    let found = match found {
        Ok(found) => found,
        // Offline or GitHub unreachable: try again tomorrow, and tell the
        // user only if they asked for this check.
        Err(e) => {
            eprintln!("update check: {e}");
            let mut inner = lock(app);
            inner.status = after_failed_check(&inner.status);
            drop(inner);
            crate::tray::refresh(app);
            return;
        }
    };
    {
        let mut inner = lock(app);
        // An install may have started while the check ran.
        if !should_check(&inner.status) {
            return;
        }
        inner.status = match &found {
            Some(update) => UpdateStatus::Available { version: update.version.clone() },
            None => UpdateStatus::UpToDate,
        };
        inner.pending = found;
    }
    crate::tray::refresh(app);
}

/// The tray line for updates.
pub fn current_tray_item(app: &AppHandle) -> TrayUpdateItem {
    tray_item(&lock(app).status, METHOD, &app.package_info().version.to_string())
}

/// The tray line was clicked.
pub fn on_click(app: &AppHandle) {
    let action = click_action(&lock(app).status);
    match (action, METHOD) {
        (ClickAction::Install, UpdateMethod::ReleasePage) => open_release_page(),
        (ClickAction::Install, UpdateMethod::InPlace) => install(app),
        (ClickAction::Check, _) => check_now(app),
        (ClickAction::Nothing, _) => {}
    }
}

/// A check the user asked for: the tray says "Checking…" until it ends.
fn check_now(app: &AppHandle) {
    lock(app).status = UpdateStatus::Checking;
    crate::tray::refresh(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move { check(&app).await });
}

fn install(app: &AppHandle) {
    let (update, version) = {
        let mut inner = lock(app);
        let UpdateStatus::Available { version } = inner.status.clone() else { return };
        let Some(update) = inner.pending.clone() else { return };
        inner.status = UpdateStatus::Installing { version: version.clone() };
        (update, version)
    };
    crate::tray::refresh(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match update.download_and_install(|_, _| {}, || {}).await {
            Ok(()) => app.restart(),
            Err(e) => {
                eprintln!("update install: {e}");
                lock(&app).status = UpdateStatus::Available { version };
                crate::tray::refresh(&app);
            }
        }
    });
}

fn open_release_page() {
    #[cfg(target_os = "macos")]
    {
        const RELEASES_URL: &str = "https://github.com/iamraihan/clipboard-history/releases/latest";
        if let Err(e) = std::process::Command::new("open").arg(RELEASES_URL).spawn() {
            eprintln!("open release page: {e}");
        }
    }
}
