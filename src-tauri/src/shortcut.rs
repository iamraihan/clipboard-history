//! Shortcut orchestration: which registrar this session gets, the first-run
//! and reset flows, and the tray status text.
//!
//! The decisions are pure functions (tested below); `init` and the `run_*`
//! functions apply them to the running app.

use crate::core::settings::{self, Settings};
use crate::platform::{BackendError, ForceReport, ShortcutRegistrar};
use crate::popup;
use crate::state::{AppState, StartupReport};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
#[cfg(test)]
use ts_rs::TS;

pub const SETTINGS_WINDOW: &str = "settings";
/// Emitted with the new `SettingsView` when the settings window is shown for a purpose.
const SETTINGS_VIEW_CHANGED: &str = "settings-view-changed";

/// How the global shortcut gets registered in this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrarKind {
    /// GNOME (X11 or Wayland): a custom keybinding that runs `<app> --toggle`.
    Gnome,
    /// tauri-plugin-global-shortcut (non-GNOME X11, macOS, Windows).
    Plugin,
    /// Non-GNOME Wayland: nothing can register a shortcut yet.
    None,
}

/// Pure choice from the session environment. GNOME wins on both X11 and
/// Wayland because GNOME itself grabs Super+V.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn registrar_kind(desktop: Option<&str>, session_type: Option<&str>, wayland_display: Option<&str>) -> RegistrarKind {
    if desktop.is_some_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("GNOME"))) {
        return RegistrarKind::Gnome;
    }
    let wayland = session_type == Some("wayland") || wayland_display.is_some_and(|w| !w.is_empty());
    if session_type == Some("x11") || !wayland {
        RegistrarKind::Plugin
    } else {
        RegistrarKind::None
    }
}

#[cfg(target_os = "linux")]
fn detect_kind() -> RegistrarKind {
    let get = |k: &str| std::env::var(k).ok();
    registrar_kind(get("XDG_CURRENT_DESKTOP").as_deref(), get("XDG_SESSION_TYPE").as_deref(), get("WAYLAND_DISPLAY").as_deref())
}

#[cfg(not(target_os = "linux"))]
fn detect_kind() -> RegistrarKind {
    kind_for_os(std::env::consts::OS, RegistrarKind::Plugin)
}

/// macOS gets no registrar until Plan 3 (Ctrl+Cmd+V): Cmd+V must never be taken.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn kind_for_os(os: &str, plugin: RegistrarKind) -> RegistrarKind {
    if os == "macos" { RegistrarKind::None } else { plugin }
}

/// Whether the global-shortcut plugin must be loaded (it is not on GNOME,
/// where it would grab keys for nothing, nor on a Wayland-only session).
pub fn uses_plugin() -> bool {
    detect_kind() == RegistrarKind::Plugin
}

/// The executable the GNOME keybinding runs: `$APPIMAGE` when set (the
/// running binary is inside a temporary mount), else the current exe without
/// the " (deleted)" Linux adds after an update. A path that is not UTF-8 is an
/// error, never a lossy guess.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn exe_for_command(appimage: Option<&str>, current_exe: Option<&std::ffi::OsStr>) -> Result<String, String> {
    if let Some(a) = appimage.filter(|a| !a.is_empty()) {
        return Ok(a.to_string());
    }
    let exe = current_exe.ok_or("cannot find the app's own path")?;
    let exe = exe.to_str().ok_or("the app's path is not valid UTF-8")?;
    Ok(exe.strip_suffix(" (deleted)").unwrap_or(exe).to_string())
}

/// What startup does about the shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupPlan {
    /// GNOME, originals saved but Force is off (crash mid-force or a failed
    /// rollback): give them back before anything else.
    Restore,
    /// GNOME, never asked: show the first-run dialog.
    AskFirstRun,
    Register,
    /// Nothing to register.
    Idle,
}

pub fn startup_plan(kind: RegistrarKind, settings: &Settings) -> StartupPlan {
    match kind {
        RegistrarKind::None => StartupPlan::Idle,
        RegistrarKind::Plugin => StartupPlan::Register,
        RegistrarKind::Gnome if !settings.force_super_v && !settings.saved_bindings.is_empty() => StartupPlan::Restore,
        RegistrarKind::Gnome if !settings.first_run_done => StartupPlan::AskFirstRun,
        RegistrarKind::Gnome if settings.force_super_v => StartupPlan::Register,
        RegistrarKind::Gnome => StartupPlan::Idle,
    }
}

/// Whether starting the app opens the popup. The shortcut always does. A
/// plain launch (the app grid, a terminal) does too, so the user sees the app
/// and its shortcut, except while the first-run dialog asks about Super+V.
/// Login never does: the app starts in the tray.
pub fn opens_popup_at_launch(plan: StartupPlan, launch: crate::core::autostart::Launch) -> bool {
    use crate::core::autostart::Launch;
    match launch {
        Launch::Toggle => true,
        Launch::Plain => plan != StartupPlan::AskFirstRun,
        Launch::Autostart => false,
    }
}

/// The shortcut to show the user (`combo` from Settings), only while it works.
pub fn active_shortcut(status: ShortcutStatus, combo: &str) -> Option<String> {
    (status == ShortcutStatus::Active).then(|| combo.to_string())
}

/// The tray's open item, naming the shortcut when there is one.
pub fn open_label(shortcut: Option<&str>) -> String {
    match shortcut {
        Some(combo) => format!("Open clipboard history ({combo})"),
        None => "Open clipboard history".to_string(),
    }
}

/// The tray's "Use Super+V…" item: only where it can help, i.e. GNOME with
/// Force off and no shortcut registered (the user said "Not now").
pub fn offers_use_super_v(kind: RegistrarKind, settings: &Settings, status: ShortcutStatus) -> bool {
    kind == RegistrarKind::Gnome && !settings.force_super_v && status != ShortcutStatus::Active
}

/// Whether the shortcut works, for the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutStatus {
    Active,
    NotSet,
    /// This desktop has no way to register one.
    Unavailable,
}

/// Tray tooltip: the name, the shortcut state if not working, and startup problems, one per line.
pub fn tooltip(status: ShortcutStatus, report: &StartupReport) -> String {
    let mut lines = vec!["Clipboard History"];
    match status {
        ShortcutStatus::Active => {}
        ShortcutStatus::NotSet => lines.push("Shortcut not set: choose \u{201c}Use Super+V\u{2026}\u{201d}"),
        ShortcutStatus::Unavailable => lines.push("Shortcut not available on this desktop yet"),
    }
    if report.backend_error.is_some() {
        lines.push("Clipboard watching is off");
    }
    if report.store_error.is_some() {
        lines.push("History is off");
    }
    if report.key_unavailable {
        lines.push("History locked: waiting for the keyring");
    }
    lines.join("\n")
}

/// The part of a Force report the user must hear about: what Super+V was
/// taken from besides GNOME's notification list (which moves to Super+M and
/// is the expected case), named readably. None when there is nothing to say.
pub fn conflict_notice(report: &ForceReport) -> Option<ForceReport> {
    let names: Vec<String> = report
        .freed_from
        .iter()
        .filter(|label| !label.ends_with(" toggle-message-tray"))
        .map(|label| match label.strip_prefix("custom shortcut \"").and_then(|l| l.strip_suffix('"')) {
            Some(name) => name.to_string(),
            // "<schema> <key>": the key says what it does.
            None => label.rsplit(' ').next().unwrap_or(label).replace('-', " "),
        })
        .collect();
    (!names.is_empty()).then_some(ForceReport { freed_from: names })
}

fn failed(what: &str, e: &BackendError) -> String {
    eprintln!("{what}: {e}");
    format!("Couldn't {what}")
}

/// The first-run answer. Yes: free Super+V and register it; on any failure
/// everything is given back and the question stays open. No (or no registrar):
/// nothing changes in the OS. Settings are saved either way.
pub fn first_run_choice(
    settings: &mut Settings,
    registrar: Option<&dyn ShortcutRegistrar>,
    use_super_v: bool,
    save: &dyn Fn(&Settings) -> io::Result<()>,
) -> Result<Option<ForceReport>, String> {
    let mut report = None;
    if let (true, Some(reg)) = (use_super_v, registrar) {
        let taken = reg.force_super_v(settings).and_then(|r| reg.register(&settings.shortcut).map(|()| r));
        match taken {
            Ok(r) => {
                report = Some(r);
                settings.force_super_v = true;
            }
            Err(e) => {
                let _ = reg.restore(settings);
                settings.force_super_v = false;
                return Err(failed("set up Super+V", &e));
            }
        }
    }
    settings.first_run_done = true;
    save(settings).map_err(|e| {
        eprintln!("settings: cannot save: {e}");
        "Couldn't save the settings".to_string()
    })?;
    Ok(report)
}

/// Reset shortcuts to default: give every binding back and stop forcing Super+V.
pub fn reset_shortcuts(
    settings: &mut Settings,
    registrar: Option<&dyn ShortcutRegistrar>,
    save: &dyn Fn(&Settings) -> io::Result<()>,
) -> Result<(), String> {
    if let Some(reg) = registrar {
        reg.restore(settings).map_err(|e| failed("give Super+V back", &e))?;
    }
    settings.force_super_v = false;
    save(settings).map_err(|e| {
        eprintln!("settings: cannot save: {e}");
        "Couldn't save the settings".to_string()
    })
}

/// What Quit does first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitStep {
    Quit,
    /// Super+V is forced: ask whether to give it back.
    AskGiveBack,
}

pub fn quit_step(settings: &Settings) -> QuitStep {
    if settings.force_super_v { QuitStep::AskGiveBack } else { QuitStep::Quit }
}

/// What the settings window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(TS), ts(export))]
pub enum SettingsView {
    Home,
    FirstRun,
    QuitPrompt,
}

/// The answer to the quit prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(TS), ts(export))]
pub enum QuitChoice {
    GiveBackAndQuit,
    KeepAndQuit,
    Cancel,
}

// ---- Applying the decisions to the running app ----

/// tauri-plugin-global-shortcut. Used for non-GNOME X11 and, for now, macOS and Windows.
pub struct X11Registrar {
    app: AppHandle,
}

impl ShortcutRegistrar for X11Registrar {
    fn register(&self, combo: &str) -> Result<(), BackendError> {
        use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
        let shortcuts = self.app.global_shortcut();
        shortcuts.unregister_all().map_err(|e| BackendError(e.to_string()))?;
        shortcuts
            .on_shortcut(combo, |app, _shortcut, event| {
                if event.state == ShortcutState::Pressed {
                    popup::toggle(app);
                }
            })
            .map_err(|e| BackendError(format!("cannot register {combo}: {e}")))
    }

    fn force_super_v(&self, _settings: &mut Settings) -> Result<ForceReport, BackendError> {
        Ok(ForceReport { freed_from: Vec::new() })
    }

    fn restore(&self, _settings: &mut Settings) -> Result<(), BackendError> {
        Ok(())
    }
}

/// A save function for `path`; creates the directory first.
fn saver(path: Option<PathBuf>) -> impl Fn(&Settings) -> io::Result<()> + Send + Sync + 'static {
    move |s: &Settings| {
        let path = path.as_deref().ok_or_else(|| io::Error::other("no settings path"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        settings::save(path, s)
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn make_registrar(app: &AppHandle, kind: RegistrarKind, path: Option<PathBuf>) -> Option<Arc<dyn ShortcutRegistrar>> {
    match kind {
        RegistrarKind::None => None,
        RegistrarKind::Plugin => Some(Arc::new(X11Registrar { app: app.clone() })),
        #[cfg(target_os = "linux")]
        RegistrarKind::Gnome => {
            use crate::platform::linux::{GnomeRegistrar, SystemGSettings};
            let current = std::env::current_exe().ok();
            let exe = exe_for_command(std::env::var("APPIMAGE").ok().as_deref(), current.as_deref().map(|p| p.as_os_str()));
            if let Err(e) = &exe {
                eprintln!("shortcut: {e}");
            }
            Some(Arc::new(GnomeRegistrar::new(SystemGSettings::new(), exe, Box::new(saver(path)))))
        }
        #[cfg(not(target_os = "linux"))]
        RegistrarKind::Gnome => None,
    }
}

/// Startup: load settings, pick the registrar, give back originals left by a
/// crash, then register or ask. Returns the plan it carried out.
pub fn init(app: &AppHandle) -> StartupPlan {
    let path = app.path().app_data_dir().ok().map(|d| d.join(crate::SETTINGS_FILE));
    let loaded = path.as_deref().map(settings::load).unwrap_or_default();
    let kind = detect_kind();
    let registrar = make_registrar(app, kind, path.clone());
    let state = app.state::<AppState>();
    *lock(&state.settings_path) = path;
    *lock(&state.registrar) = registrar.clone();
    *lock(&state.settings) = loaded;
    let _op = lock(&state.shortcut_op);

    let mut plan = startup_plan(kind, &lock(&state.settings));
    if plan == StartupPlan::Restore {
        let restored = {
            let mut copy = lock(&state.settings).clone();
            let out = reset_shortcuts(&mut copy, registrar.as_deref(), &saver(lock(&state.settings_path).clone()));
            *lock(&state.settings) = copy;
            out
        };
        if let Err(e) = restored {
            eprintln!("startup: {e}");
        }
        plan = startup_plan(kind, &lock(&state.settings));
    }
    let shortcut = lock(&state.settings).shortcut.clone();
    let status = match (plan, registrar) {
        (StartupPlan::Register, Some(reg)) => match reg.register(&shortcut) {
            Ok(()) => ShortcutStatus::Active,
            Err(e) => {
                eprintln!("shortcut: {e}");
                ShortcutStatus::NotSet
            }
        },
        (StartupPlan::AskFirstRun, _) => {
            show_settings(app, SettingsView::FirstRun);
            ShortcutStatus::NotSet
        }
        _ if kind == RegistrarKind::None => ShortcutStatus::Unavailable,
        _ => ShortcutStatus::NotSet,
    };
    *lock(&state.shortcut_status) = status;
    plan
}

/// Shows the settings window with `view` selected.
pub fn show_settings(app: &AppHandle, view: SettingsView) {
    *lock(&app.state::<AppState>().settings_view) = view;
    let _ = app.emit(SETTINGS_VIEW_CHANGED, view);
    if let Some(win) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

pub fn hide_settings(app: &AppHandle) {
    *lock(&app.state::<AppState>().settings_view) = SettingsView::Home;
    if let Some(win) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = win.hide();
    }
}

/// Runs `f` on a copy of the settings and stores the copy back, so the
/// `settings` lock is not held while gsettings runs. `shortcut_op` is held
/// throughout, so two operations never interleave. The copy is written back
/// even on error: a failed Force may have recorded originals that Reset needs.
fn with_settings_state<T>(
    state: &AppState,
    f: impl FnOnce(&mut Settings, Option<&dyn ShortcutRegistrar>, &dyn Fn(&Settings) -> io::Result<()>) -> T,
) -> T {
    let _op = lock(&state.shortcut_op);
    let registrar = lock(&state.registrar).clone();
    let path: Option<PathBuf> = lock(&state.settings_path).clone();
    let mut copy = lock(&state.settings).clone();
    let out = f(&mut copy, registrar.as_deref(), &saver(path));
    *lock(&state.settings) = copy;
    out
}

fn with_settings<T>(
    app: &AppHandle,
    f: impl FnOnce(&mut Settings, Option<&dyn ShortcutRegistrar>, &dyn Fn(&Settings) -> io::Result<()>) -> T,
) -> T {
    with_settings_state(&app.state::<AppState>(), f)
}

/// Applies the first-run answer. Returns the conflicts the user must be told
/// about (see `conflict_notice`); the settings window stays open until the UI
/// calls `hide_settings`.
pub fn run_first_run_choice(app: &AppHandle, use_super_v: bool) -> Result<Option<ForceReport>, String> {
    let report = with_settings(app, |s, reg, save| first_run_choice(s, reg, use_super_v, save))?;
    *lock(&app.state::<AppState>().shortcut_status) =
        if report.is_some() { ShortcutStatus::Active } else { ShortcutStatus::NotSet };
    crate::tray::refresh(app);
    // The dialog closes the window itself (`hide_settings`), after showing any conflict.
    Ok(report.as_ref().and_then(conflict_notice))
}

pub fn run_reset(app: &AppHandle) -> Result<(), String> {
    with_settings(app, |s, reg, save| reset_shortcuts(s, reg, save))?;
    *lock(&app.state::<AppState>().shortcut_status) = ShortcutStatus::NotSet;
    crate::tray::refresh(app);
    Ok(())
}

/// Tray "Quit".
pub fn request_quit(app: &AppHandle) {
    let step = quit_step(&lock(&app.state::<AppState>().settings));
    match step {
        QuitStep::Quit => app.exit(0),
        QuitStep::AskGiveBack => show_settings(app, SettingsView::QuitPrompt),
    }
}

pub fn run_quit_choice(app: &AppHandle, choice: QuitChoice) -> Result<(), String> {
    match choice {
        QuitChoice::GiveBackAndQuit => {
            run_reset(app)?;
            app.exit(0);
        }
        QuitChoice::KeepAndQuit => app.exit(0),
        QuitChoice::Cancel => hide_settings(app),
    }
    Ok(())
}

/// Whether the tray shows "Use Super+V…" now.
pub fn show_use_super_v(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let status = *lock(&state.shortcut_status);
    let settings = lock(&state.settings).clone();
    offers_use_super_v(detect_kind(), &settings, status)
}

/// The working shortcut, if any, as the user should see it.
pub fn current_shortcut(app: &AppHandle) -> Option<String> {
    let state = app.state::<AppState>();
    let status = *lock(&state.shortcut_status);
    let combo = lock(&state.settings).shortcut.clone();
    active_shortcut(status, &combo)
}

/// The current tooltip text.
pub fn current_tooltip(app: &AppHandle) -> String {
    let state = app.state::<AppState>();
    let status = *lock(&state.shortcut_status);
    let report = lock(&state.report).clone();
    tooltip(status, &report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::settings::{SavedBinding, Settings};
    use crate::platform::{BackendError, ForceReport, ShortcutRegistrar};
    use crate::state::StartupReport;
    use std::sync::Mutex;

    /// Records calls; `fail_on` makes the named call fail.
    #[derive(Default)]
    struct FakeRegistrar {
        calls: Mutex<Vec<String>>,
        fail_on: Option<&'static str>,
    }

    impl FakeRegistrar {
        fn failing(op: &'static str) -> Self {
            Self { fail_on: Some(op), ..Self::default() }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn step(&self, op: &str, call: String) -> Result<(), BackendError> {
            self.calls.lock().unwrap().push(call);
            if self.fail_on == Some(op) { Err(BackendError(format!("{op} failed"))) } else { Ok(()) }
        }
    }

    impl ShortcutRegistrar for FakeRegistrar {
        fn register(&self, combo: &str) -> Result<(), BackendError> {
            self.step("register", format!("register {combo}"))
        }
        fn force_super_v(&self, settings: &mut Settings) -> Result<ForceReport, BackendError> {
            settings.saved_bindings.push(SavedBinding { schema: "s".into(), key: "k".into(), original: "o".into() });
            self.step("force", "force".into())?;
            Ok(ForceReport { freed_from: vec!["s k".into()] })
        }
        fn restore(&self, settings: &mut Settings) -> Result<(), BackendError> {
            self.step("restore", "restore".into())?;
            settings.saved_bindings.clear();
            Ok(())
        }
    }

    fn saved_ok(_: &Settings) -> std::io::Result<()> {
        Ok(())
    }

    #[test]
    fn gnome_desktops_get_the_gnome_registrar_on_x11_and_wayland() {
        for session in [Some("x11"), Some("wayland")] {
            assert_eq!(registrar_kind(Some("ubuntu:GNOME"), session, Some("wayland-0")), RegistrarKind::Gnome);
        }
        assert_eq!(registrar_kind(Some("GNOME"), None, None), RegistrarKind::Gnome);
    }

    #[test]
    fn other_x11_desktops_use_the_plugin_and_other_wayland_desktops_have_none() {
        assert_eq!(registrar_kind(Some("XFCE"), Some("x11"), None), RegistrarKind::Plugin);
        assert_eq!(registrar_kind(Some("KDE"), None, None), RegistrarKind::Plugin);
        assert_eq!(registrar_kind(Some("KDE"), Some("wayland"), Some("wayland-0")), RegistrarKind::None);
        assert_eq!(registrar_kind(None, Some("wayland"), Some("wayland-0")), RegistrarKind::None);
    }

    #[test]
    fn startup_plan_asks_once_on_gnome_and_registers_elsewhere() {
        let fresh = Settings::default();
        let done = Settings { first_run_done: true, ..Settings::default() };
        let forced = Settings { first_run_done: true, force_super_v: true, ..Settings::default() };
        assert_eq!(startup_plan(RegistrarKind::Gnome, &fresh), StartupPlan::AskFirstRun);
        assert_eq!(startup_plan(RegistrarKind::Gnome, &done), StartupPlan::Idle);
        assert_eq!(startup_plan(RegistrarKind::Gnome, &forced), StartupPlan::Register);
        assert_eq!(startup_plan(RegistrarKind::Plugin, &fresh), StartupPlan::Register);
        assert_eq!(startup_plan(RegistrarKind::None, &fresh), StartupPlan::Idle);
    }

    #[test]
    fn startup_restores_first_when_originals_are_saved_but_force_is_off() {
        let crashed = Settings {
            first_run_done: true,
            saved_bindings: vec![SavedBinding { schema: "s".into(), key: "k".into(), original: "o".into() }],
            ..Settings::default()
        };
        assert_eq!(startup_plan(RegistrarKind::Gnome, &crashed), StartupPlan::Restore);
        assert_eq!(startup_plan(RegistrarKind::Gnome, &Settings { force_super_v: true, ..crashed.clone() }), StartupPlan::Register);
        assert_eq!(startup_plan(RegistrarKind::None, &crashed), StartupPlan::Idle);
    }

    #[test]
    fn exe_path_for_the_command() {
        use std::ffi::OsStr;
        fn exe(p: &str) -> Option<&OsStr> {
            Some(OsStr::new(p))
        }
        assert_eq!(exe_for_command(Some("/opt/App.AppImage"), exe("/tmp/.mount_x/usr/bin/app")).unwrap(), "/opt/App.AppImage");
        assert_eq!(exe_for_command(Some(""), exe("/usr/bin/app")).unwrap(), "/usr/bin/app");
        assert_eq!(exe_for_command(None, exe("/usr/bin/app (deleted)")).unwrap(), "/usr/bin/app");
        assert!(exe_for_command(None, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_exe_path_is_an_error_not_a_lossy_path() {
        use std::os::unix::ffi::OsStrExt;
        assert!(exe_for_command(None, Some(std::ffi::OsStr::from_bytes(b"/opt/\xff/app"))).is_err());
    }

    /// Blocks inside `force_super_v` until released; `restore` records what it sees.
    struct BlockingRegistrar {
        started: Mutex<std::sync::mpsc::Sender<()>>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        restore_saw: std::sync::Arc<Mutex<Vec<usize>>>,
    }

    impl ShortcutRegistrar for BlockingRegistrar {
        fn register(&self, _: &str) -> Result<(), BackendError> {
            Ok(())
        }
        fn force_super_v(&self, settings: &mut Settings) -> Result<ForceReport, BackendError> {
            settings.saved_bindings.push(SavedBinding { schema: "s".into(), key: "k".into(), original: "o".into() });
            self.started.lock().unwrap().send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            Ok(ForceReport { freed_from: vec![] })
        }
        fn restore(&self, settings: &mut Settings) -> Result<(), BackendError> {
            self.restore_saw.lock().unwrap().push(settings.saved_bindings.len());
            settings.saved_bindings.clear();
            Ok(())
        }
    }

    #[test]
    fn shortcut_operations_run_one_at_a_time() {
        use std::sync::mpsc::channel;
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(crate::state::AppState::new());
        *state.settings_path.lock().unwrap() = Some(dir.path().join("settings.json"));
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let saw = Arc::new(Mutex::new(Vec::new()));
        *state.registrar.lock().unwrap() = Some(Arc::new(BlockingRegistrar {
            started: Mutex::new(started_tx),
            release: Mutex::new(release_rx),
            restore_saw: saw.clone(),
        }));

        let a = {
            let state = state.clone();
            std::thread::spawn(move || with_settings_state(&state, |s, reg, save| first_run_choice(s, reg, true, save)))
        };
        started_rx.recv().unwrap(); // A is inside force_super_v, holding the operation lock.
        let b = {
            let state = state.clone();
            std::thread::spawn(move || with_settings_state(&state, |s, reg, save| reset_shortcuts(s, reg, save)))
        };
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(saw.lock().unwrap().is_empty(), "reset must wait for the first-run operation");
        release_tx.send(()).unwrap();
        a.join().unwrap().unwrap();
        b.join().unwrap().unwrap();
        assert_eq!(*saw.lock().unwrap(), vec![1], "reset saw the saved originals of the finished operation");
        assert!(state.settings.lock().unwrap().saved_bindings.is_empty());
    }

    #[test]
    fn first_run_yes_forces_registers_and_marks_done() {
        let reg = FakeRegistrar::default();
        let mut s = Settings::default();
        let report = first_run_choice(&mut s, Some(&reg), true, &saved_ok).unwrap();
        assert_eq!(report, Some(ForceReport { freed_from: vec!["s k".into()] }));
        assert_eq!(reg.calls(), ["force", "register Super+V"]);
        assert!(s.force_super_v && s.first_run_done);
    }

    #[test]
    fn first_run_no_registers_nothing_but_marks_done() {
        let reg = FakeRegistrar::default();
        let mut s = Settings::default();
        assert_eq!(first_run_choice(&mut s, Some(&reg), false, &saved_ok).unwrap(), None);
        assert!(reg.calls().is_empty());
        assert!(!s.force_super_v && s.first_run_done);
    }

    #[test]
    fn first_run_failure_gives_everything_back_and_asks_again_next_time() {
        for op in ["force", "register"] {
            let reg = FakeRegistrar::failing(op);
            let mut s = Settings::default();
            assert!(first_run_choice(&mut s, Some(&reg), true, &saved_ok).is_err());
            assert_eq!(reg.calls().last().unwrap(), "restore", "{op}");
            assert!(!s.first_run_done && !s.force_super_v && s.saved_bindings.is_empty());
        }
    }

    #[test]
    fn first_run_without_a_registrar_just_marks_done() {
        let mut s = Settings::default();
        assert_eq!(first_run_choice(&mut s, None, true, &saved_ok).unwrap(), None);
        assert!(s.first_run_done && !s.force_super_v);
    }

    #[test]
    fn first_run_saves_settings() {
        let saved = Mutex::new(Vec::new());
        let save = |s: &Settings| {
            saved.lock().unwrap().push((s.first_run_done, s.force_super_v));
            Ok(())
        };
        first_run_choice(&mut Settings::default(), Some(&FakeRegistrar::default()), true, &save).unwrap();
        assert_eq!(saved.lock().unwrap().last(), Some(&(true, true)));
    }

    #[test]
    fn reset_restores_and_turns_force_off() {
        let reg = FakeRegistrar::default();
        let mut s = Settings { force_super_v: true, first_run_done: true, ..Settings::default() };
        reset_shortcuts(&mut s, Some(&reg), &saved_ok).unwrap();
        assert_eq!(reg.calls(), ["restore"]);
        assert!(!s.force_super_v && s.first_run_done);
    }

    #[test]
    fn reset_failure_keeps_force_on_so_it_can_be_retried() {
        let reg = FakeRegistrar::failing("restore");
        let mut s = Settings { force_super_v: true, ..Settings::default() };
        assert!(reset_shortcuts(&mut s, Some(&reg), &saved_ok).is_err());
        assert!(s.force_super_v);
    }

    #[test]
    fn quit_asks_only_while_super_v_is_forced() {
        assert_eq!(quit_step(&Settings::default()), QuitStep::Quit);
        assert_eq!(quit_step(&Settings { force_super_v: true, ..Settings::default() }), QuitStep::AskGiveBack);
    }

    #[test]
    fn conflict_notice_ignores_the_notification_list() {
        let f = |names: &[&str]| ForceReport { freed_from: names.iter().map(|s| s.to_string()).collect() };
        let tray = "org.gnome.shell.keybindings toggle-message-tray";
        assert_eq!(conflict_notice(&f(&[])), None);
        assert_eq!(conflict_notice(&f(&[tray])), None);
        assert_eq!(
            conflict_notice(&f(&[tray, "custom shortcut \"Paste thing\"", "org.gnome.desktop.wm.keybindings switch-to-workspace-6"])),
            Some(f(&["Paste thing", "switch to workspace 6"]))
        );
    }

    #[test]
    fn tray_offers_use_super_v_only_on_gnome_without_force_or_shortcut() {
        let off = Settings { first_run_done: true, ..Settings::default() };
        let on = Settings { force_super_v: true, ..off.clone() };
        assert!(offers_use_super_v(RegistrarKind::Gnome, &off, ShortcutStatus::NotSet));
        assert!(!offers_use_super_v(RegistrarKind::Gnome, &on, ShortcutStatus::Active));
        assert!(!offers_use_super_v(RegistrarKind::Gnome, &off, ShortcutStatus::Active));
        assert!(!offers_use_super_v(RegistrarKind::Plugin, &off, ShortcutStatus::NotSet));
        assert!(!offers_use_super_v(RegistrarKind::None, &off, ShortcutStatus::Unavailable));
    }

    #[test]
    fn macos_has_no_registrar_for_now_and_windows_uses_the_plugin() {
        assert_eq!(kind_for_os("macos", RegistrarKind::Plugin), RegistrarKind::None);
        assert_eq!(kind_for_os("windows", RegistrarKind::Plugin), RegistrarKind::Plugin);
    }

    #[test]
    fn the_shortcut_is_shown_only_while_it_works() {
        assert_eq!(active_shortcut(ShortcutStatus::Active, "Super+V"), Some("Super+V".to_string()));
        assert_eq!(active_shortcut(ShortcutStatus::NotSet, "Super+V"), None);
        assert_eq!(active_shortcut(ShortcutStatus::Unavailable, "Super+V"), None);
    }

    #[test]
    fn the_tray_open_item_names_the_shortcut() {
        assert_eq!(open_label(Some("Super+V")), "Open clipboard history (Super+V)");
        assert_eq!(open_label(None), "Open clipboard history");
    }

    #[test]
    fn a_plain_launch_opens_the_popup_unless_the_first_run_dialog_is_up() {
        use crate::core::autostart::Launch;
        assert!(opens_popup_at_launch(StartupPlan::Register, Launch::Plain));
        assert!(opens_popup_at_launch(StartupPlan::Idle, Launch::Plain));
        assert!(opens_popup_at_launch(StartupPlan::Restore, Launch::Plain));
        assert!(!opens_popup_at_launch(StartupPlan::AskFirstRun, Launch::Plain));
    }

    #[test]
    fn the_shortcut_always_opens_the_popup_and_login_never_does() {
        use crate::core::autostart::Launch;
        assert!(opens_popup_at_launch(StartupPlan::Register, Launch::Toggle));
        assert!(opens_popup_at_launch(StartupPlan::AskFirstRun, Launch::Toggle));
        assert!(!opens_popup_at_launch(StartupPlan::Register, Launch::Autostart));
        assert!(!opens_popup_at_launch(StartupPlan::Idle, Launch::Autostart));
    }

    #[test]
    fn tooltip_names_the_shortcut_state_and_startup_problems() {
        let ok = StartupReport::default();
        assert_eq!(tooltip(ShortcutStatus::Active, &ok), "Clipboard History");
        assert_eq!(tooltip(ShortcutStatus::NotSet, &ok), "Clipboard History\nShortcut not set: choose \u{201c}Use Super+V\u{2026}\u{201d}");
        assert_eq!(
            tooltip(ShortcutStatus::Unavailable, &ok),
            "Clipboard History\nShortcut not available on this desktop yet"
        );
        let bad = StartupReport {
            backend_error: Some("no display".into()),
            store_error: Some("History cannot be opened: x".into()),
            ..StartupReport::default()
        };
        assert_eq!(
            tooltip(ShortcutStatus::Active, &bad),
            "Clipboard History\nClipboard watching is off\nHistory is off"
        );
        let locked = StartupReport { key_unavailable: true, ..StartupReport::default() };
        assert_eq!(tooltip(ShortcutStatus::Active, &locked), "Clipboard History\nHistory locked: waiting for the keyring");
    }
}
