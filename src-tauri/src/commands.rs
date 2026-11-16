//! Tauri commands exposed to the frontend over IPC.
//!
//! Each command is a thin wrapper over a plain function taking `&AppState`,
//! which the tests call against a FakeBackend and a temp store. Errors are
//! short, user-readable strings; they never carry clip contents or key
//! material (details go to stderr).

use crate::models::ClipSummary;
use crate::platform::ForceReport;
use crate::shortcut::{QuitChoice, SettingsView};
use crate::state::{now_unix_secs, AppState, StartupReport};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};
#[cfg(test)]
use ts_rs::TS;

const NOT_AVAILABLE: &str = "History is not available yet";

/// What `paste_clip` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(TS), ts(export))]
pub enum PasteOutcome {
    /// Written and pasted into the previous window (auto-paste).
    Pasted,
    /// Written to the clipboard; the user pastes.
    Copied,
    /// A plain paste of a Clip with no plain text; nothing was written.
    NothingToPaste,
}

use crate::state::lock;

fn failed(what: &str, e: impl std::fmt::Display) -> String {
    eprintln!("{what}: {e}");
    format!("Couldn't {what}")
}

pub fn list_impl(state: &AppState, query: Option<&str>) -> Result<Vec<ClipSummary>, String> {
    let guard = lock(&state.engine);
    let Some(engine) = guard.as_ref() else { return Ok(vec![]) };
    engine.list(query).map_err(|e| failed("load the history", e))
}

pub fn set_pinned_impl(state: &AppState, id: i64, pinned: bool) -> Result<(), String> {
    let mut guard = lock(&state.engine);
    let engine = guard.as_mut().ok_or(NOT_AVAILABLE)?;
    engine.set_pinned(id, pinned).map_err(|e| failed("change the pin", e))
}

pub fn delete_impl(state: &AppState, id: i64) -> Result<(), String> {
    let mut guard = lock(&state.engine);
    let engine = guard.as_mut().ok_or(NOT_AVAILABLE)?;
    engine.delete(id).map_err(|e| failed("delete the item", e))
}

pub fn clear_unpinned_impl(state: &AppState) -> Result<(), String> {
    let mut guard = lock(&state.engine);
    let engine = guard.as_mut().ok_or(NOT_AVAILABLE)?;
    engine.clear_unpinned().map_err(|e| failed("clear the history", e))
}

/// Prepares and writes the Clip; the caller hides the popup afterwards.
/// The engine lock is released before the Backend is called.
pub fn paste_impl(state: &AppState, id: i64, plain: bool, now: i64) -> Result<PasteOutcome, String> {
    let formats = {
        let mut guard = lock(&state.engine);
        let engine = guard.as_mut().ok_or(NOT_AVAILABLE)?;
        engine.prepare_paste(id, plain, now).map_err(|e| failed("read the item", e))?
    };
    if formats.is_empty() {
        return Ok(PasteOutcome::NothingToPaste);
    }
    let backend = lock(&state.backend).clone().ok_or("The clipboard is not available")?;
    backend.write(&formats).map_err(|e| failed("copy the item", e))?;
    Ok(PasteOutcome::Copied)
}

/// Time for the popup to disappear and the old window to get the focus back.
const HIDE_SETTLE: Duration = Duration::from_millis(50);

/// After the popup is hidden: gives the focus back to the window the user came
/// from and presses Ctrl+V in it. Anything short of that leaves the Clip on
/// the clipboard for the user to paste (`Copied`); keys are never sent to a
/// window that could not be focused. Blocks (sleeps, X calls): run it off the
/// async runtime. No lock is held while it waits.
pub fn auto_paste_impl(state: &AppState, plain: bool) -> PasteOutcome {
    let enabled = lock(&state.settings).auto_paste;
    let paster = lock(&state.paster).clone();
    let focus = lock(&state.focus).clone();
    // `take`: a target is used once, never again by a later pick.
    let target = lock(&state.focused_before_popup).take();
    let (true, Some(paster), Some(focus), Some(target)) = (enabled, paster, focus, target) else {
        return PasteOutcome::Copied;
    };
    std::thread::sleep(HIDE_SETTLE);
    if let Err(e) = focus.restore(&target) {
        eprintln!("not pasting, the old window did not get the focus: {e}");
        return PasteOutcome::Copied;
    }
    match paster.paste(&target, plain) {
        Ok(()) => PasteOutcome::Pasted,
        Err(e) => {
            eprintln!("could not paste: {e}");
            PasteOutcome::Copied
        }
    }
}

#[tauri::command]
pub fn list_clips(state: State<'_, AppState>, query: Option<String>) -> Result<Vec<ClipSummary>, String> {
    list_impl(&state, query.as_deref())
}

#[tauri::command]
pub fn set_pinned(state: State<'_, AppState>, id: i64, pinned: bool) -> Result<(), String> {
    set_pinned_impl(&state, id, pinned)
}

#[tauri::command]
pub fn delete_clip(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    delete_impl(&state, id)
}

#[tauri::command]
pub fn clear_unpinned(state: State<'_, AppState>) -> Result<(), String> {
    clear_unpinned_impl(&state)
}

/// Async so a slow Backend write never blocks the main (UI) thread.
#[tauri::command]
pub async fn paste_clip(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    plain: bool,
) -> Result<PasteOutcome, String> {
    let outcome = paste_impl(&state, id, plain, now_unix_secs())?;
    if outcome == PasteOutcome::NothingToPaste {
        return Ok(outcome);
    }
    crate::popup::hide(&app);
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || auto_paste_impl(&handle.state::<AppState>(), plain))
        .await
        .map_err(|e| failed("paste the item", e))
}

#[tauri::command]
pub fn startup_report(state: State<'_, AppState>) -> StartupReport {
    state.report_for_ui()
}

#[tauri::command]
pub fn hide_popup(app: AppHandle) {
    crate::popup::hide(&app);
}

/// What the settings window should show now.
#[tauri::command]
pub fn settings_view(state: State<'_, AppState>) -> SettingsView {
    *lock(&state.settings_view)
}

/// The first-run dialog's answer. Yes returns the conflicts to tell the user about (what else lost Super+V), if any.
/// Async: it runs `gsettings`, which must not block the UI thread.
#[tauri::command]
pub async fn first_run_choice(app: AppHandle, use_super_v: bool) -> Result<Option<ForceReport>, String> {
    crate::shortcut::run_first_run_choice(&app, use_super_v)
}

/// Hides the settings window (the first-run dialog's OK, after it showed any conflict).
#[tauri::command]
pub fn hide_settings(app: AppHandle) {
    crate::shortcut::hide_settings(&app);
}

/// "Reset shortcuts to default".
#[tauri::command]
pub async fn reset_shortcuts(app: AppHandle) -> Result<(), String> {
    crate::shortcut::run_reset(&app)
}

/// The quit prompt's answer.
#[tauri::command]
pub async fn quit_choice(app: AppHandle, choice: QuitChoice) -> Result<(), String> {
    crate::shortcut::run_quit_choice(&app, choice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::clip::{ClipEvent, Format};
    use crate::core::settings::Settings;
    use crate::engine::Engine;
    use crate::platform::fake::{FakeBackend, FakeFocus, FakePaster};
    use crate::platform::FocusedWindow;
    use crate::store::{HistoryStore, StaticKey};
    use std::sync::{Arc, Mutex};

    fn text(s: &str) -> Format {
        Format { mime: "text/plain;charset=utf-8".into(), data: s.as_bytes().to_vec() }
    }
    fn html(s: &str) -> Format {
        Format { mime: "text/html".into(), data: s.as_bytes().to_vec() }
    }
    fn ev(formats: Vec<Format>) -> ClipEvent {
        ClipEvent { formats, source_app: None, secret: false }
    }

    fn state_with(engine: bool) -> (AppState, FakeBackend, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = HistoryStore::open(&dir.path().join("h.db"), &StaticKey([7; 32]), 0).unwrap();
        let fake = FakeBackend::new();
        let state = AppState {
            engine: Mutex::new(engine.then(|| Engine::new(store, Settings::default(), Box::new(|| {})))),
            backend: Mutex::new(Some(Arc::new(fake.clone()))),
            ..AppState::new()
        };
        (state, fake, dir)
    }

    fn add(state: &AppState, formats: Vec<Format>, now: i64) -> i64 {
        let mut g = state.engine.lock().unwrap();
        let e = g.as_mut().unwrap();
        e.handle_event(ev(formats), now).unwrap();
        e.store.list(None, 100).unwrap()[0].id
    }

    #[test]
    fn paste_clip_writes_formats_to_backend() {
        let (state, fake, _d) = state_with(true);
        let id = add(&state, vec![html("<b>hi</b>"), text("hi")], 1);
        assert_eq!(paste_impl(&state, id, false, 2).unwrap(), PasteOutcome::Copied);
        assert_eq!(fake.writes(), vec![vec![html("<b>hi</b>"), text("hi")]]);
    }

    #[test]
    fn paste_plain_writes_only_plain_text() {
        let (state, fake, _d) = state_with(true);
        let id = add(&state, vec![html("<b>hi</b>"), text("hi")], 1);
        assert_eq!(paste_impl(&state, id, true, 2).unwrap(), PasteOutcome::Copied);
        assert_eq!(fake.writes(), vec![vec![text("hi")]]);
    }

    #[test]
    fn paste_plain_without_text_writes_nothing() {
        let (state, fake, _d) = state_with(true);
        let png = Format { mime: "image/png".into(), data: vec![1, 2, 3] };
        let id = add(&state, vec![png], 1);
        assert_eq!(paste_impl(&state, id, true, 2).unwrap(), PasteOutcome::NothingToPaste);
        assert!(fake.writes().is_empty());
    }

    #[test]
    fn list_with_query_filters() {
        let (state, _f, _d) = state_with(true);
        add(&state, vec![text("apple pie")], 1);
        add(&state, vec![text("banana split")], 2);
        assert_eq!(list_impl(&state, None).unwrap().len(), 2);
        let hits = list_impl(&state, Some("banana")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].preview, "banana split");
    }

    #[test]
    fn list_includes_pinned_beyond_max_items() {
        let (state, _f, _d) = state_with(true);
        let pinned = add(&state, vec![text("keep")], 1);
        set_pinned_impl(&state, pinned, true).unwrap();
        state.engine.lock().unwrap().as_mut().unwrap().settings.limits.max_items = 1;
        add(&state, vec![text("a")], 2);
        add(&state, vec![text("b")], 3);
        let previews: Vec<_> = list_impl(&state, None).unwrap().into_iter().map(|c| c.preview).collect();
        assert_eq!(previews, ["keep", "b"]);
    }

    #[test]
    fn pin_delete_and_clear_change_the_list() {
        let (state, _f, _d) = state_with(true);
        let a = add(&state, vec![text("a")], 1);
        let b = add(&state, vec![text("b")], 2);
        let c = add(&state, vec![text("c")], 3);
        set_pinned_impl(&state, a, true).unwrap();
        delete_impl(&state, b).unwrap();
        clear_unpinned_impl(&state).unwrap();
        let rows = list_impl(&state, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, a);
        assert!(rows[0].pinned);
        let _ = c;
    }

    #[test]
    fn without_an_engine_list_is_empty_and_mutations_fail() {
        let (state, fake, _d) = state_with(false);
        assert_eq!(list_impl(&state, None).unwrap(), vec![]);
        let msg = "History is not available yet";
        assert_eq!(set_pinned_impl(&state, 1, true), Err(msg.to_string()));
        assert_eq!(delete_impl(&state, 1), Err(msg.to_string()));
        assert_eq!(clear_unpinned_impl(&state), Err(msg.to_string()));
        assert_eq!(paste_impl(&state, 1, false, 1), Err(msg.to_string()));
        assert!(fake.writes().is_empty());
    }

    fn with_paste(paster: FakePaster, focus: FakeFocus, auto_paste: bool) -> (AppState, FocusedWindow) {
        let (state, _f, _d) = state_with(false);
        let target = FocusedWindow(42);
        *state.paster.lock().unwrap() = Some(Arc::new(paster));
        *state.focus.lock().unwrap() = Some(Arc::new(focus));
        *state.focused_before_popup.lock().unwrap() = Some(target);
        state.settings.lock().unwrap().auto_paste = auto_paste;
        (state, target)
    }

    #[test]
    fn paste_clip_returns_pasted_when_auto_paste_succeeds() {
        let (paster, focus) = (FakePaster::default(), FakeFocus::default());
        let (state, target) = with_paste(paster.clone(), focus.clone(), true);
        assert_eq!(auto_paste_impl(&state, true), PasteOutcome::Pasted);
        assert_eq!(*focus.restored.lock().unwrap(), vec![target]);
        assert_eq!(*paster.pastes.lock().unwrap(), vec![(target, true)]);
    }

    #[test]
    fn paste_clip_returns_copied_when_focus_restore_fails() {
        let (paster, focus) = (FakePaster::default(), FakeFocus { fail_restore: true, ..FakeFocus::default() });
        let (state, _t) = with_paste(paster.clone(), focus, true);
        assert_eq!(auto_paste_impl(&state, false), PasteOutcome::Copied);
        assert!(paster.pastes.lock().unwrap().is_empty());
    }

    #[test]
    fn paste_clip_returns_copied_when_the_paste_itself_fails() {
        let paster = FakePaster { fail: true, ..FakePaster::default() };
        let (state, _t) = with_paste(paster, FakeFocus::default(), true);
        assert_eq!(auto_paste_impl(&state, false), PasteOutcome::Copied);
    }

    #[test]
    fn paste_clip_returns_copied_without_auto_paste_paster_or_target() {
        let (paster, focus) = (FakePaster::default(), FakeFocus::default());
        let (state, _t) = with_paste(paster.clone(), focus.clone(), false);
        assert_eq!(auto_paste_impl(&state, false), PasteOutcome::Copied);
        let (state, _t) = with_paste(paster.clone(), focus.clone(), true);
        *state.focused_before_popup.lock().unwrap() = None;
        assert_eq!(auto_paste_impl(&state, false), PasteOutcome::Copied);
        let (state, _t) = with_paste(paster.clone(), focus.clone(), true);
        *state.paster.lock().unwrap() = None;
        assert_eq!(auto_paste_impl(&state, false), PasteOutcome::Copied);
        assert!(paster.pastes.lock().unwrap().is_empty() && focus.restored.lock().unwrap().is_empty());
    }
}
