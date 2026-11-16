//! Thin engine: applies core decisions and runs side effects.

use crate::core::clip::{content_hash, ClipEvent, Format};
use crate::core::rules::{decide, echo_target, Decision, OwnWrite, SaveContext, SkipReason};
use crate::core::settings::Settings;
use crate::models::ClipSummary;
use crate::store::{HistoryStore, StoreError};

const MIME_TEXT: &str = "text/plain;charset=utf-8";

pub struct Engine {
    pub store: HistoryStore,
    pub settings: Settings,
    /// The app's last clipboard write; one-shot, see `handle_event`.
    own_write: Option<OwnWrite>,
    /// Runs under the engine lock: emit an event only, never re-enter the engine.
    on_change: Box<dyn Fn() + Send>,
}

const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Engine>();
};

impl Engine {
    pub fn new(store: HistoryStore, settings: Settings, on_change: Box<dyn Fn() + Send>) -> Self {
        Self { store, settings, own_write: None, on_change }
    }

    /// Saves, bumps or skips a ClipEvent. The remembered own write is consumed
    /// by whatever event comes next, matching or not. `on_change` runs after
    /// every committed Insert/Bump, so an `Err` (from retention) can follow a
    /// saved event.
    pub fn handle_event(&mut self, e: ClipEvent, now: i64) -> Result<Decision, StoreError> {
        let hash = content_hash(&e.formats);
        let own = self.own_write.take();
        let echo_id = match echo_target(own.as_ref(), hash, now) {
            Some(id) if self.store.exists(id)? => Some(id),
            _ => None,
        };
        let decision = match echo_id {
            // prepare_paste already bumped it.
            Some(_) if self.settings.paused => return Ok(Decision::Skip(SkipReason::Paused)),
            Some(id) => {
                self.store.bump(id, now)?;
                Decision::Bump(id)
            }
            None => {
                let ctx = SaveContext {
                    limits: &self.settings.limits,
                    paused: self.settings.paused,
                    excluded_apps: &self.settings.excluded_apps,
                    existing: self.store.find_by_hash(hash)?,
                };
                match decide(&e, &ctx) {
                    Decision::Skip(r) => return Ok(Decision::Skip(r)),
                    Decision::Insert => {
                        self.store.insert(&e, now)?;
                        Decision::Insert
                    }
                    Decision::Bump(id) => {
                        self.store.bump(id, now)?;
                        Decision::Bump(id)
                    }
                }
            }
        };
        let retention = self.store.apply_retention(&self.settings.limits, now);
        (self.on_change)();
        retention?;
        Ok(decision)
    }

    /// Popup list: pinned first, then newest. Shows `max_items` unpinned Clips
    /// plus every pinned one.
    pub fn list(&self, query: Option<&str>) -> Result<Vec<ClipSummary>, StoreError> {
        let limit = self.settings.limits.max_items.saturating_add(self.store.pinned_count()?);
        self.store.list(query, limit)
    }

    pub fn set_pinned(&mut self, id: i64, pinned: bool) -> Result<(), StoreError> {
        self.store.set_pinned(id, pinned)?;
        (self.on_change)();
        Ok(())
    }

    pub fn delete(&mut self, id: i64) -> Result<(), StoreError> {
        self.store.delete(id)?;
        (self.on_change)();
        Ok(())
    }

    pub fn clear_unpinned(&mut self) -> Result<(), StoreError> {
        self.store.clear_unpinned()?;
        (self.on_change)();
        Ok(())
    }

    /// Formats to write to the clipboard for Clip `id`; bumps it and remembers
    /// the write so its Echo is recognised. Plain with no plain text gives an
    /// empty Vec (nothing to paste) and changes nothing.
    pub fn prepare_paste(&mut self, id: i64, plain: bool, now: i64) -> Result<Vec<Format>, StoreError> {
        let mut formats = self.store.formats(id)?;
        if plain {
            formats.retain(|f| f.mime == MIME_TEXT);
        }
        if formats.is_empty() {
            return Ok(formats);
        }
        self.store.bump(id, now)?;
        self.own_write = Some(OwnWrite { hash: content_hash(&formats), clip_id: id, at: now });
        (self.on_change)();
        Ok(formats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rules::{Limits, SkipReason};
    use crate::store::StaticKey;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn text(s: &str) -> Format {
        Format { mime: MIME_TEXT.into(), data: s.as_bytes().to_vec() }
    }

    fn ev(formats: Vec<Format>) -> ClipEvent {
        ClipEvent { formats, source_app: None, secret: false }
    }

    fn engine(settings: Settings) -> (Engine, Arc<AtomicUsize>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = HistoryStore::open(&dir.path().join("h.db"), &StaticKey([7; 32]), 0).unwrap();
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let e = Engine::new(store, settings, Box::new(move || { n2.fetch_add(1, Ordering::SeqCst); }));
        (e, n, dir)
    }

    #[test]
    fn event_is_saved_and_listener_notified() {
        let (mut e, n, _d) = engine(Settings::default());
        assert_eq!(e.handle_event(ev(vec![text("hi")]), 1).unwrap(), Decision::Insert);
        assert_eq!(e.store.list(None, 10).unwrap().len(), 1);
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn skip_does_not_notify() {
        let (mut e, n, _d) = engine(Settings { paused: true, ..Settings::default() });
        assert_eq!(e.handle_event(ev(vec![text("hi")]), 1).unwrap(), Decision::Skip(SkipReason::Paused));
        assert_eq!(e.store.list(None, 10).unwrap().len(), 0);
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn prepare_paste_then_echo_event_does_not_duplicate() {
        let (mut e, _n, _d) = engine(Settings::default());
        e.handle_event(ev(vec![text("hi")]), 1).unwrap();
        let id = e.store.list(None, 10).unwrap()[0].id;
        let formats = e.prepare_paste(id, false, 2).unwrap();
        assert_eq!(e.handle_event(ev(formats), 3).unwrap(), Decision::Bump(id));
        assert_eq!(e.store.list(None, 10).unwrap().len(), 1);
    }

    #[test]
    fn plain_paste_echo_bumps_original_not_new_clip() {
        let (mut e, _n, _d) = engine(Settings::default());
        let html = Format { mime: "text/html".into(), data: b"<b>hi</b>".to_vec() };
        e.handle_event(ev(vec![html, text("hi")]), 1).unwrap();
        let id = e.store.list(None, 10).unwrap()[0].id;
        let plain = e.prepare_paste(id, true, 2).unwrap();
        assert_eq!(plain, vec![text("hi")]);
        assert_eq!(e.handle_event(ev(plain), 3).unwrap(), Decision::Bump(id));
        assert_eq!(e.store.list(None, 10).unwrap().len(), 1);
    }

    #[test]
    fn plain_paste_returns_only_plain_text() {
        let (mut e, _n, _d) = engine(Settings::default());
        let html = Format { mime: "text/html".into(), data: b"<b>hi</b>".to_vec() };
        e.handle_event(ev(vec![html.clone(), text("hi")]), 1).unwrap();
        let id = e.store.list(None, 10).unwrap()[0].id;
        assert_eq!(e.prepare_paste(id, true, 2).unwrap(), vec![text("hi")]);
        assert_eq!(e.prepare_paste(id, false, 3).unwrap().len(), 2);
    }

    #[test]
    fn plain_paste_without_plain_text_is_empty_and_inert() {
        let (mut e, n, _d) = engine(Settings::default());
        let png = Format { mime: "image/png".into(), data: vec![1, 2, 3] };
        e.handle_event(ev(vec![png]), 1).unwrap();
        let id = e.store.list(None, 10).unwrap()[0].id;
        assert!(e.prepare_paste(id, true, 2).unwrap().is_empty());
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert!(e.own_write.is_none());
    }

    #[test]
    fn stale_echo_does_not_capture_later_plain_copy() {
        let (mut e, _n, _d) = engine(Settings::default());
        let html = Format { mime: "text/html".into(), data: b"<b>hi</b>".to_vec() };
        e.handle_event(ev(vec![html, text("hi")]), 1).unwrap();
        let id = e.store.list(None, 10).unwrap()[0].id;
        let plain = e.prepare_paste(id, true, 2).unwrap();
        assert_eq!(e.handle_event(ev(plain), 12).unwrap(), Decision::Insert);
        assert_eq!(e.store.list(None, 10).unwrap().len(), 2);
    }

    #[test]
    fn echo_after_pasted_clip_deleted_saves_copy() {
        let (mut e, _n, _d) = engine(Settings::default());
        e.handle_event(ev(vec![text("hi")]), 1).unwrap();
        let id = e.store.list(None, 10).unwrap()[0].id;
        let f = e.prepare_paste(id, false, 2).unwrap();
        e.store.delete(id).unwrap();
        assert_eq!(e.handle_event(ev(f), 3).unwrap(), Decision::Insert);
        assert_eq!(e.store.list(None, 10).unwrap().len(), 1);
    }

    #[test]
    fn echo_while_paused_is_consumed() {
        let (mut e, _n, _d) = engine(Settings::default());
        let html = Format { mime: "text/html".into(), data: b"<b>hi</b>".to_vec() };
        e.handle_event(ev(vec![html, text("hi")]), 1).unwrap();
        let id = e.store.list(None, 10).unwrap()[0].id;
        let plain = e.prepare_paste(id, true, 2).unwrap();
        e.settings.paused = true;
        assert_eq!(e.handle_event(ev(plain.clone()), 3).unwrap(), Decision::Skip(SkipReason::Paused));
        e.settings.paused = false;
        assert_eq!(e.handle_event(ev(plain), 13).unwrap(), Decision::Insert);
    }

    #[test]
    fn retention_runs_after_insert() {
        let limits = Limits { max_items: 2, ..Limits::default() };
        let (mut e, _n, _d) = engine(Settings { limits, ..Settings::default() });
        for (i, s) in ["a", "b", "c"].iter().enumerate() {
            e.handle_event(ev(vec![text(s)]), i as i64 + 1).unwrap();
        }
        assert_eq!(e.store.list(None, 10).unwrap().len(), 2);
    }
}
