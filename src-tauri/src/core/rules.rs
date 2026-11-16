//! Save rules and Retention: pure decisions over a ClipEvent and the store state.

use serde::{Deserialize, Serialize};
#[cfg(test)]
use ts_rs::TS;

pub use crate::core::clip::ClipEvent;
use crate::core::clip::{kind_of, total_size};

const SECS_PER_DAY: i64 = 86_400;

/// Retention limits and the per-Clip size cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
#[cfg_attr(test, derive(TS), ts(export))]
pub struct Limits {
    pub max_items: u32,
    pub max_age_days: Option<u32>,
    #[cfg_attr(test, ts(type = "number"))]
    pub max_item_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_items: 100, max_age_days: None, max_item_bytes: 10 * 1024 * 1024 }
    }
}

/// How long after the app's own clipboard write an Echo can still arrive.
pub const ECHO_WINDOW_SECS: i64 = 2;

/// The app's own last clipboard write, remembered for Echo detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnWrite {
    pub hash: [u8; 32],
    pub clip_id: i64,
    /// Unix seconds of the write.
    pub at: i64,
}

/// The Clip an event is the Echo of, if it matches the last own write and is
/// still inside the Echo window.
pub fn echo_target(own: Option<&OwnWrite>, event_hash: [u8; 32], now: i64) -> Option<i64> {
    let o = own?;
    (o.hash == event_hash && now - o.at <= ECHO_WINDOW_SECS).then_some(o.clip_id)
}

/// Everything `decide` needs besides the ClipEvent itself.
pub struct SaveContext<'a> {
    pub limits: &'a Limits,
    pub paused: bool,
    pub excluded_apps: &'a [String],
    /// Id of a Clip that already has this event's content hash.
    pub existing: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Secret,
    Paused,
    ExcludedApp,
    Empty,
    Oversize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Insert,
    Bump(i64),
    Skip(SkipReason),
}

/// Decide what a ClipEvent becomes. An Echo is not special-cased: it hashes
/// to the Clip the app just wrote, so `existing` is set and it bumps.
pub fn decide(event: &ClipEvent, ctx: &SaveContext) -> Decision {
    if ctx.paused {
        return Decision::Skip(SkipReason::Paused);
    }
    if event.secret {
        return Decision::Skip(SkipReason::Secret);
    }
    if let Some(app) = &event.source_app {
        if ctx.excluded_apps.iter().any(|x| x.eq_ignore_ascii_case(app)) {
            return Decision::Skip(SkipReason::ExcludedApp);
        }
    }
    if kind_of(&event.formats).is_none() {
        return Decision::Skip(SkipReason::Empty);
    }
    if total_size(&event.formats) > ctx.limits.max_item_bytes {
        return Decision::Skip(SkipReason::Oversize);
    }
    match ctx.existing {
        Some(id) => Decision::Bump(id),
        None => Decision::Insert,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionItem {
    pub id: i64,
    pub pinned: bool,
    /// Unix seconds.
    pub last_used_at: i64,
}

/// Ids Retention removes: unpinned items older than `max_age_days`, plus the
/// least recently used unpinned items beyond `max_items`. Pinned are never returned.
pub fn to_evict(items: &[RetentionItem], limits: &Limits, now: i64) -> Vec<i64> {
    let mut unpinned: Vec<&RetentionItem> = items.iter().filter(|i| !i.pinned).collect();
    unpinned.sort_by(|a, b| b.last_used_at.cmp(&a.last_used_at).then(b.id.cmp(&a.id)));
    let cutoff = limits.max_age_days.map(|d| now - i64::from(d) * SECS_PER_DAY);
    unpinned
        .iter()
        .enumerate()
        .filter(|(rank, i)| {
            *rank >= limits.max_items as usize || cutoff.is_some_and(|c| i.last_used_at < c)
        })
        .map(|(_, i)| i.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::clip::{content_hash, Format};

    const MB10: usize = 10 * 1024 * 1024;

    fn text(s: &[u8]) -> ClipEvent {
        ClipEvent {
            formats: vec![Format { mime: "text/plain;charset=utf-8".into(), data: s.to_vec() }],
            source_app: None,
            secret: false,
        }
    }

    fn ctx<'a>(limits: &'a Limits, excluded: &'a [String]) -> SaveContext<'a> {
        SaveContext { limits, paused: false, excluded_apps: excluded, existing: None }
    }

    #[test]
    fn paused_skips_before_anything_else() {
        let l = Limits::default();
        let mut e = text(b"");
        e.secret = true;
        let mut c = ctx(&l, &[]);
        c.paused = true;
        assert_eq!(decide(&e, &c), Decision::Skip(SkipReason::Paused));
    }

    #[test]
    fn secret_is_skipped() {
        let l = Limits::default();
        let mut e = text(b"pw");
        e.secret = true;
        assert_eq!(decide(&e, &ctx(&l, &[])), Decision::Skip(SkipReason::Secret));
    }

    #[test]
    fn excluded_app_matches_case_insensitively() {
        let l = Limits::default();
        let ex = vec!["keepassxc".to_string()];
        let mut e = text(b"x");
        e.source_app = Some("KeePassXC".into());
        assert_eq!(decide(&e, &ctx(&l, &ex)), Decision::Skip(SkipReason::ExcludedApp));
    }

    #[test]
    fn empty_formats_are_skipped() {
        let l = Limits::default();
        let e = ClipEvent { formats: vec![], source_app: None, secret: false };
        assert_eq!(decide(&e, &ctx(&l, &[])), Decision::Skip(SkipReason::Empty));
    }

    #[test]
    fn oversize_is_skipped() {
        let l = Limits::default();
        let over = text(&vec![b'a'; MB10 + 1]);
        assert_eq!(decide(&over, &ctx(&l, &[])), Decision::Skip(SkipReason::Oversize));
        let exact = text(&vec![b'a'; MB10]);
        assert_eq!(decide(&exact, &ctx(&l, &[])), Decision::Insert);
    }

    #[test]
    fn existing_hash_bumps() {
        let l = Limits::default();
        let mut c = ctx(&l, &[]);
        c.existing = Some(7);
        assert_eq!(decide(&text(b"x"), &c), Decision::Bump(7));
    }

    fn own(hash: [u8; 32]) -> OwnWrite {
        OwnWrite { hash, clip_id: 5, at: 100 }
    }

    #[test]
    fn echo_target_matches_within_window() {
        let h = content_hash(&text(b"x").formats);
        assert_eq!(echo_target(Some(&own(h)), h, 101), Some(5));
    }

    #[test]
    fn echo_target_expires_after_window() {
        let h = content_hash(&text(b"x").formats);
        assert_eq!(echo_target(Some(&own(h)), h, 103), None);
    }

    #[test]
    fn echo_target_ignores_other_hash() {
        let h = content_hash(&text(b"x").formats);
        let o = content_hash(&text(b"y").formats);
        assert_eq!(echo_target(Some(&own(h)), o, 100), None);
        assert_eq!(echo_target(None, h, 100), None);
    }

    fn item(id: i64, pinned: bool, last_used_at: i64) -> RetentionItem {
        RetentionItem { id, pinned, last_used_at }
    }

    #[test]
    fn evicts_oldest_unpinned_beyond_max_items() {
        let items: Vec<_> = (1..=102).map(|i| item(i, false, 1000 + i)).collect();
        let mut out = to_evict(&items, &Limits::default(), 2000);
        out.sort();
        assert_eq!(out, vec![1, 2]);
    }

    #[test]
    fn pinned_never_evicted_and_not_counted() {
        let mut items: Vec<_> = (1..=100).map(|i| item(i, false, 1000 + i)).collect();
        items.extend((101..=105).map(|i| item(i, true, 1)));
        assert!(to_evict(&items, &Limits::default(), 2000).is_empty());
    }

    #[test]
    fn evicts_by_age_when_set() {
        let day = 86_400;
        let now = 100 * day;
        let limits = Limits { max_age_days: Some(30), ..Limits::default() };
        let items = vec![item(1, false, now - 31 * day), item(2, false, now - 29 * day), item(3, true, now - 99 * day)];
        assert_eq!(to_evict(&items, &limits, now), vec![1]);
    }
}
