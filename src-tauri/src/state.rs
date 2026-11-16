//! Shared app state, managed by Tauri, plus the pure helpers of the startup flow.

use crate::core::rules::Decision;
use crate::engine::Engine;
use crate::core::settings::Settings;
use crate::platform::{ClipboardBackend, FocusTracker, FocusedWindow, Paster, ShortcutRegistrar};
use crate::shortcut::{SettingsView, ShortcutStatus};
use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(test)]
use ts_rs::TS;

/// What the UI needs to know about how startup went.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(TS), ts(export))]
pub struct StartupReport {
    /// Why there is no Backend (clipboard is not being watched); None when it runs.
    pub backend_error: Option<String>,
    /// The database key lives in a file, not the OS keyring.
    pub key_in_file: bool,
    /// The history database was unreadable and moved aside; history started empty.
    pub recovered_broken: bool,
    /// The OS keyring could not be reached; history is off while startup retries.
    pub key_unavailable: bool,
    /// Why the history store could not be opened; independent of `backend_error`.
    pub store_error: Option<String>,
    /// Picking an item pastes it into the previous window (X11 with a Paster
    /// and the auto-paste setting on). When false the popup tells the user to
    /// press Ctrl+V. Filled in when the report is read, not stored.
    pub auto_paste_available: bool,
}

impl StartupReport {
    pub fn set_backend_error(&mut self, why: impl Into<String>) {
        self.backend_error = Some(why.into());
    }

    pub fn set_store_error(&mut self, why: impl Into<String>) {
        self.store_error = Some(why.into());
    }

    /// The retries for the keyring are used up.
    pub fn give_up_on_key(&mut self) {
        self.key_unavailable = false;
        self.set_store_error(KEY_GIVE_UP);
    }
}

/// A startup failure and the StartupReport field it belongs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupFailure {
    /// No clipboard Backend (unsupported session, connection failed, watch failed).
    Backend(String),
    /// The app data directory is unknown, so the store cannot open.
    NoDataDir(String),
    /// `HistoryStore::open` failed (not a key outage).
    StoreOpen(String),
}

impl StartupFailure {
    pub fn record(self, r: &mut StartupReport) {
        match self {
            Self::Backend(m) => r.set_backend_error(m),
            Self::NoDataDir(m) => r.set_store_error(format!("History cannot be opened: no data directory: {m}")),
            Self::StoreOpen(m) => r.set_store_error(format!("History cannot be opened: {m}")),
        }
    }
}

pub const KEY_GIVE_UP: &str =
    "History is locked: the keyring didn't unlock. Restart the app to try again.";

/// Managed by Tauri. Locks are never held across a sleep or a Backend call.
pub struct AppState {
    /// `None` until the store is open: at startup the keyring may be locked
    /// (`StartupReport::key_unavailable`), and the engine appears when a retry
    /// succeeds. Commands must handle `None`.
    pub engine: Mutex<Option<Engine>>,
    /// `None` when no Backend could start (`StartupReport::backend_error`).
    /// Set once at startup, then only cloned out, so no lock is held while
    /// the Backend runs.
    pub backend: Mutex<Option<Arc<dyn ClipboardBackend>>>,
    pub paster: Mutex<Option<Arc<dyn Paster>>>,
    pub focus: Mutex<Option<Arc<dyn FocusTracker>>>,
    pub focused_before_popup: Mutex<Option<FocusedWindow>>,
    pub report: Mutex<StartupReport>,
    /// Shortcut-related settings (shortcut, force_super_v, first_run_done,
    /// saved_bindings). The engine keeps its own copy for limits and rules.
    pub settings: Mutex<Settings>,
    /// Where `settings` is saved; `None` without an app data dir.
    pub settings_path: Mutex<Option<std::path::PathBuf>>,
    /// `None` when the session has no way to register a shortcut.
    pub registrar: Mutex<Option<Arc<dyn ShortcutRegistrar>>>,
    pub shortcut_status: Mutex<ShortcutStatus>,
    /// What the settings window shows next.
    pub settings_view: Mutex<SettingsView>,
    /// Held for the whole of every shortcut operation (startup, first run,
    /// reset, quit): they read, change and save the same settings and the OS
    /// bindings, so they run one at a time.
    pub shortcut_op: Mutex<()>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            engine: Mutex::new(None),
            backend: Mutex::new(None),
            paster: Mutex::new(None),
            focus: Mutex::new(None),
            focused_before_popup: Mutex::new(None),
            report: Mutex::new(StartupReport::default()),
            settings: Mutex::new(Settings::default()),
            settings_path: Mutex::new(None),
            registrar: Mutex::new(None),
            shortcut_status: Mutex::new(ShortcutStatus::NotSet),
            settings_view: Mutex::new(SettingsView::Home),
            shortcut_op: Mutex::new(()),
        }
    }
}

impl AppState {
    /// The StartupReport as the UI sees it, with `auto_paste_available` current.
    pub fn report_for_ui(&self) -> StartupReport {
        let mut report = self.report.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let has_paster = self.paster.lock().unwrap_or_else(|p| p.into_inner()).is_some();
        let enabled = self.settings.lock().unwrap_or_else(|p| p.into_inner()).auto_paste;
        report.auto_paste_available = has_paster && enabled;
        report
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Delay before each retry of opening the store while the key is unavailable:
/// every 5 s for 2 min (24 tries), then give up.
pub fn retry_delays() -> impl Iterator<Item = Duration> {
    std::iter::repeat_n(Duration::from_secs(5), 24)
}

/// Log text for the outcome of one ClipEvent. `saved_id` is the id of the
/// Clip an Insert created.
pub fn describe_outcome(r: &Result<Decision, StoreError>, saved_id: Option<i64>) -> String {
    match r {
        Ok(Decision::Insert) => match saved_id {
            Some(id) => format!("saved clip id={id}"),
            None => "saved clip".to_string(),
        },
        Ok(Decision::Bump(id)) => format!("saved clip id={id} (already in history, moved to top)"),
        Ok(Decision::Skip(reason)) => format!("skipped: {reason:?}"),
        Err(e) => format!("store error: {e}"),
    }
}

/// Seconds since the Unix epoch.
pub fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rules::SkipReason;

    #[test]
    fn backend_and_store_errors_do_not_overwrite_each_other() {
        let mut r = StartupReport::default();
        r.set_backend_error("no display");
        r.set_store_error("db broken");
        assert_eq!(r.backend_error.as_deref(), Some("no display"));
        assert_eq!(r.store_error.as_deref(), Some("db broken"));
        r.key_unavailable = true;
        r.give_up_on_key();
        assert!(!r.key_unavailable);
        assert_eq!(r.store_error.as_deref(), Some(KEY_GIVE_UP));
        assert_eq!(r.backend_error.as_deref(), Some("no display"));
    }

    #[test]
    fn startup_failures_route_to_their_own_field() {
        let mut r = StartupReport::default();
        StartupFailure::Backend("no display".into()).record(&mut r);
        StartupFailure::NoDataDir("denied".into()).record(&mut r);
        assert_eq!(r.backend_error.as_deref(), Some("no display"));
        assert_eq!(r.store_error.as_deref(), Some("History cannot be opened: no data directory: denied"));
        StartupFailure::StoreOpen("bad".into()).record(&mut r);
        assert_eq!(r.store_error.as_deref(), Some("History cannot be opened: bad"));
        assert_eq!(r.backend_error.as_deref(), Some("no display"));
    }

    #[test]
    fn auto_paste_is_available_only_with_a_paster_and_the_setting() {
        let state = AppState::new();
        assert!(!state.report_for_ui().auto_paste_available);
        *state.paster.lock().unwrap() = Some(Arc::new(crate::platform::fake::FakePaster::default()));
        assert!(state.report_for_ui().auto_paste_available);
        state.settings.lock().unwrap().auto_paste = false;
        assert!(!state.report_for_ui().auto_paste_available);
    }

    #[test]
    fn retry_delays_are_5s_for_2_minutes() {
        let d: Vec<Duration> = retry_delays().collect();
        assert_eq!(d.len(), 24);
        assert!(d.iter().all(|x| *x == Duration::from_secs(5)));
        assert_eq!(d.iter().sum::<Duration>(), Duration::from_secs(120));
    }

    #[test]
    fn outcome_lines_name_the_clip_or_the_reason() {
        assert_eq!(describe_outcome(&Ok(Decision::Insert), Some(7)), "saved clip id=7");
        assert!(describe_outcome(&Ok(Decision::Bump(3)), None).starts_with("saved clip id=3"));
        assert_eq!(describe_outcome(&Ok(Decision::Skip(SkipReason::Secret)), None), "skipped: Secret");
        assert!(describe_outcome(&Err(StoreError::InvalidClip("x")), None).starts_with("store error"));
    }
}
