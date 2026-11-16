//! SQLite history storage and key management.
//!
//! The database is SQLCipher; the key comes from a `KeyProvider`. Public
//! methods return `Result<_, StoreError>`; the engine decides how to log them.

mod key;

pub use key::{FileKey, KeyInfo, KeyProvider, OsKeyring, StaticKey};

use std::fmt;
use std::ffi::OsString;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};

use crate::core::clip::{content_hash, kind_of, text_preview, total_size, ClipEvent, Format};
use crate::core::rules::{to_evict, Limits, RetentionItem};
use crate::models::{ClipKind, ClipSummary};

const SCHEMA: &str = include_str!("schema.sql");
const THUMB_MAX: u32 = 96;
const PREVIEW_CHARS: usize = 200;
const MIME_FILES: &str = "text/uri-list";
const MIME_IMAGE: &str = "image/png";

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// The key cannot be obtained safely right now (keyring outage, malformed
    /// value, or a fresh key where a database already exists). Never carries key material.
    KeyUnavailable(String),
    /// A ClipEvent the store cannot hold (no kind).
    InvalidClip(&'static str),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "database error: {e}"),
            Self::Io(e) => write!(f, "file error: {e}"),
            Self::KeyUnavailable(m) => write!(f, "key unavailable: {m}"),
            Self::InvalidClip(m) => write!(f, "invalid clip: {m}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// What `HistoryStore::open` had to do besides opening the file.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct OpenReport {
    /// Where an unreadable database was moved before a fresh one was created.
    pub recovered_broken: Option<PathBuf>,
    /// The key lives in a plain file because the OS keyring was unavailable.
    pub key_in_file: bool,
}

pub struct HistoryStore {
    conn: Connection,
}

fn kind_str(k: ClipKind) -> &'static str {
    match k {
        ClipKind::Text => "text",
        ClipKind::RichText => "richText",
        ClipKind::Image => "image",
        ClipKind::Files => "files",
    }
}

/// Persisted-enum fallback: an unknown stored kind reads as Text.
fn parse_kind(s: &str) -> ClipKind {
    match s {
        "richText" => ClipKind::RichText,
        "image" => ClipKind::Image,
        "files" => ClipKind::Files,
        _ => ClipKind::Text,
    }
}

fn rfc3339(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0).map(|d| d.to_rfc3339()).unwrap_or_default()
}

/// Opens `path` with `key` and checks the key is right.
fn open_keyed(path: &Path, key: &[u8; 32]) -> Result<Connection, StoreError> {
    let conn = Connection::open(path)?;
    conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(key)))?;
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))?;
    Ok(conn)
}

/// Only a wrong key / failed HMAC (`NotADatabase`) or a damaged file justifies
/// moving the database aside; busy, I/O and permission errors do not.
fn is_wrong_key_or_corrupt(e: &StoreError) -> bool {
    matches!(
        e,
        StoreError::Sqlite(rusqlite::Error::SqliteFailure(f, _))
            if matches!(f.code, rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt)
    )
}

/// Renames the database (and its journal/-wal/-shm) to `<name>.broken-<now>`,
/// adding `-2`, `-3`... rather than overwriting an earlier one.
fn move_aside(path: &Path, now: i64) -> Result<PathBuf, StoreError> {
    let name = path.file_name().unwrap_or_default().to_os_string();
    let with = |base: &OsString, suffix: &str| {
        let mut n = base.clone();
        n.push(suffix);
        path.with_file_name(n)
    };
    let mut tag = format!(".broken-{now}");
    let mut n = 1;
    while with(&name, &tag).exists() {
        n += 1;
        tag = format!(".broken-{now}-{n}");
    }
    let broken = with(&name, &tag);
    // Sidecars first: a failure must never leave the main file moved while its
    // hot journal stays behind.
    for side in ["-journal", "-wal", "-shm"] {
        let from = with(&name, side);
        if from.exists() {
            std::fs::rename(&from, with(&name, &format!("{tag}{side}")))?;
        }
    }
    std::fs::rename(path, &broken)?;
    Ok(broken)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hexval = (b[i] == b'%' && i + 2 < b.len())
            .then(|| std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()))
            .flatten();
        match hexval {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// File names from a `text/uri-list`, decoded, comma-joined, at most 200 chars.
fn files_preview(uri_list: &[u8]) -> String {
    let text = String::from_utf8_lossy(uri_list);
    let names: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let path = l.strip_prefix("file://").unwrap_or(l);
            let decoded = percent_decode(path);
            let trimmed = decoded.trim_end_matches('/');
            trimmed.rsplit('/').next().unwrap_or(trimmed).to_owned()
        })
        .collect();
    names.join(", ").chars().take(PREVIEW_CHARS).collect()
}

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
/// Skip thumbnails for images claiming more pixels than this.
const MAX_THUMB_PIXELS: u64 = 40_000_000;

/// `(width, height)` from a PNG's IHDR chunk (signature and chunk type checked).
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.get(..8)? != PNG_SIGNATURE || png.get(12..16)? != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(png.get(20..24)?.try_into().ok()?);
    Some((w, h))
}

fn preview_of(kind: ClipKind, formats: &[Format]) -> String {
    let data = |mime: &str| formats.iter().find(|f| f.mime == mime).map(|f| f.data.as_slice());
    match kind {
        ClipKind::Files => data(MIME_FILES).map(files_preview).unwrap_or_default(),
        ClipKind::Image => data(MIME_IMAGE)
            .and_then(png_size)
            .map(|(w, h)| format!("Image {w}\u{d7}{h}"))
            .unwrap_or_else(|| "Image".to_owned()),
        ClipKind::Text | ClipKind::RichText => text_preview(formats).unwrap_or_default(),
    }
}

/// A PNG of at most 96 px on the longest side; `None` if the image is
/// implausibly large or can't be decoded. Decoding is bounded because release
/// builds abort on allocation failure.
fn thumbnail_of(png: &[u8]) -> Option<Vec<u8>> {
    let (w, h) = png_size(png)?;
    if u64::from(w) * u64::from(h) > MAX_THUMB_PIXELS {
        return None;
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let img = if img.width() > THUMB_MAX || img.height() > THUMB_MAX {
        img.thumbnail(THUMB_MAX, THUMB_MAX)
    } else {
        img
    };
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// FTS5 MATCH expression: every word is a quoted prefix term, all must match.
fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

impl HistoryStore {
    /// Opens (creating if needed) the encrypted database at `path`. A file the
    /// key cannot open is moved to `<name>.broken-<now>` and a fresh one is made.
    pub fn open(
        path: &Path,
        keys: &dyn KeyProvider,
        now: i64,
    ) -> Result<(HistoryStore, OpenReport), StoreError> {
        if let Some(dir) = path.parent() {
            key::create_private_dir(dir)?;
        }
        // A key may be created only together with a brand-new database, so a
        // recovery (move aside) can only ever happen with a key that pre-existed.
        let info = keys.key(!path.exists())?;
        let mut report = OpenReport { recovered_broken: None, key_in_file: info.from_file };
        let conn = match open_keyed(path, &info.key) {
            Ok(c) => c,
            Err(e) if is_wrong_key_or_corrupt(&e) && path.exists() => {
                report.recovered_broken = Some(move_aside(path, now)?);
                open_keyed(path, &info.key)?
            }
            Err(e) => return Err(e),
        };
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok((HistoryStore { conn }, report))
    }

    pub fn exists(&self, id: i64) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM clips WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn find_by_hash(&self, h: [u8; 32]) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT id FROM clips WHERE hash = ?1", [h.as_slice()], |r| r.get(0))
            .optional()?)
    }

    /// Stores the Clip and returns its id. `e` must have a kind; Save rules
    /// skip events without one.
    pub fn insert(&mut self, e: &ClipEvent, now: i64) -> Result<i64, StoreError> {
        let kind = kind_of(&e.formats);
        debug_assert!(kind.is_some(), "a ClipEvent without a kind must not reach insert");
        let Some(kind) = kind else { return Err(StoreError::InvalidClip("no kind")) };
        let thumb = e
            .formats
            .iter()
            .find(|f| f.mime == MIME_IMAGE)
            .filter(|_| kind == ClipKind::Image)
            .and_then(|f| thumbnail_of(&f.data));
        let preview = preview_of(kind, &e.formats);
        let hash = content_hash(&e.formats);
        let size = i64::try_from(total_size(&e.formats)).unwrap_or(i64::MAX);
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<&Format> = e.formats.iter().filter(|f| seen.insert(f.mime.as_str())).collect();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO clips (kind, hash, preview, thumbnail, source_app, pinned, \
             created_at, last_used_at, size_bytes) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6, ?7)",
            params![kind_str(kind), hash.as_slice(), preview, thumb, e.source_app, now, size],
        )?;
        let id = tx.last_insert_rowid();
        for f in unique {
            tx.execute(
                "INSERT INTO formats (clip_id, mime, data) VALUES (?1, ?2, ?3)",
                params![id, f.mime, f.data],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn bump(&mut self, id: i64, now: i64) -> Result<(), StoreError> {
        self.conn.execute("UPDATE clips SET last_used_at = ?2 WHERE id = ?1", params![id, now])?;
        Ok(())
    }

    /// Pinned first, then most recently used. `query` matches the start of each word.
    pub fn list(&self, query: Option<&str>, limit: u32) -> Result<Vec<ClipSummary>, StoreError> {
        let fts = query.and_then(fts_query);
        let sql = format!(
            "SELECT id, kind, preview, thumbnail, pinned, created_at, source_app FROM clips {} \
             ORDER BY pinned DESC, last_used_at DESC, id DESC LIMIT ?2",
            if fts.is_some() {
                "WHERE id IN (SELECT rowid FROM clips_fts WHERE clips_fts MATCH ?1)"
            } else {
                "WHERE ?1 IS NULL"
            }
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![fts, limit], |r| {
            let thumb: Option<Vec<u8>> = r.get(3)?;
            Ok(ClipSummary {
                id: r.get(0)?,
                kind: parse_kind(&r.get::<_, String>(1)?),
                preview: r.get(2)?,
                thumbnail_png_base64: thumb
                    .map(|t| base64::engine::general_purpose::STANDARD.encode(t)),
                pinned: r.get(4)?,
                created_at: rfc3339(r.get(5)?),
                source_app: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// How many Clips are pinned.
    pub fn pinned_count(&self) -> Result<u32, StoreError> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM clips WHERE pinned = 1", [], |r| r.get(0))?)
    }

    pub fn formats(&self, id: i64) -> Result<Vec<Format>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT mime, data FROM formats WHERE clip_id = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map([id], |r| Ok(Format { mime: r.get(0)?, data: r.get(1)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_pinned(&mut self, id: i64, pinned: bool) -> Result<(), StoreError> {
        self.conn.execute("UPDATE clips SET pinned = ?2 WHERE id = ?1", params![id, pinned])?;
        Ok(())
    }

    pub fn delete(&mut self, id: i64) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM clips WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn clear_unpinned(&mut self) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM clips WHERE pinned = 0", [])?;
        Ok(())
    }

    /// Deletes what Retention says to evict; returns how many Clips went.
    pub fn apply_retention(&mut self, limits: &Limits, now: i64) -> Result<usize, StoreError> {
        let items: Vec<RetentionItem> = self
            .conn
            .prepare("SELECT id, pinned, last_used_at FROM clips")?
            .query_map([], |r| {
                Ok(RetentionItem { id: r.get(0)?, pinned: r.get(1)?, last_used_at: r.get(2)? })
            })?
            .collect::<Result<_, _>>()?;
        let doomed = to_evict(&items, limits, now);
        let tx = self.conn.transaction()?;
        for id in &doomed {
            tx.execute("DELETE FROM clips WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(doomed.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::clip::content_hash;
    use crate::models::ClipKind;
    use std::io::{Cursor, Read};

    const TEXT: &str = "text/plain;charset=utf-8";

    fn fmt(mime: &str, data: &[u8]) -> Format {
        Format { mime: mime.into(), data: data.to_vec() }
    }

    fn text(s: &str) -> ClipEvent {
        ClipEvent { formats: vec![fmt(TEXT, s.as_bytes())], source_app: Some("term".into()), secret: false }
    }

    fn open_in(dir: &Path, key: u8, now: i64) -> (HistoryStore, OpenReport) {
        HistoryStore::open(&dir.join("history.db"), &StaticKey([key; 32]), now).unwrap()
    }

    fn store() -> (tempfile::TempDir, HistoryStore) {
        let dir = tempfile::tempdir().unwrap();
        let (s, _) = open_in(dir.path(), 1, 0);
        (dir, s)
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::RgbaImage::new(w, h).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn insert_then_list_returns_summary() {
        let (_d, mut s) = store();
        let e = text("hello   world");
        let id = s.insert(&e, 1_700_000_000).unwrap();
        assert_eq!(s.find_by_hash(content_hash(&e.formats)).unwrap(), Some(id));
        let list = s.list(None, 10).unwrap();
        assert_eq!(list.len(), 1);
        let c = &list[0];
        assert_eq!((c.id, c.kind, c.preview.as_str()), (id, ClipKind::Text, "hello world"));
        assert_eq!(c.source_app.as_deref(), Some("term"));
        assert!(!c.pinned);
        assert_eq!(c.created_at, "2023-11-14T22:13:20+00:00");
        assert_eq!(c.thumbnail_png_base64, None);
        assert_eq!(s.formats(id).unwrap(), e.formats);
    }

    #[test]
    fn store_bump_moves_to_top() {
        let (_d, mut s) = store();
        let a = s.insert(&text("a"), 10).unwrap();
        let b = s.insert(&text("b"), 20).unwrap();
        assert_eq!(s.list(None, 10).unwrap().iter().map(|c| c.id).collect::<Vec<_>>(), [b, a]);
        s.bump(a, 30).unwrap();
        assert_eq!(s.list(None, 10).unwrap().iter().map(|c| c.id).collect::<Vec<_>>(), [a, b]);
    }

    #[test]
    fn pinned_listed_first() {
        let (_d, mut s) = store();
        let a = s.insert(&text("a"), 10).unwrap();
        let b = s.insert(&text("b"), 20).unwrap();
        s.set_pinned(a, true).unwrap();
        let list = s.list(None, 10).unwrap();
        assert_eq!(list.iter().map(|c| c.id).collect::<Vec<_>>(), [a, b]);
        assert!(list[0].pinned);
    }

    #[test]
    fn search_matches_word_prefix() {
        let (_d, mut s) = store();
        s.insert(&text("hello world"), 10).unwrap();
        assert_eq!(s.list(Some("wor"), 10).unwrap().len(), 1);
        assert_eq!(s.list(Some("hel wor"), 10).unwrap().len(), 1);
        assert_eq!(s.list(Some("xyz"), 10).unwrap().len(), 0);
        assert_eq!(s.list(Some("\"wor"), 10).unwrap().len(), 1);
        assert_eq!(s.list(Some("   "), 10).unwrap().len(), 1);
    }

    #[test]
    fn clear_unpinned_keeps_pinned() {
        let (_d, mut s) = store();
        let a = s.insert(&text("a"), 10).unwrap();
        s.insert(&text("b"), 20).unwrap();
        s.set_pinned(a, true).unwrap();
        s.clear_unpinned().unwrap();
        assert_eq!(s.list(None, 10).unwrap().iter().map(|c| c.id).collect::<Vec<_>>(), [a]);
    }

    #[test]
    fn delete_removes_clip_and_search_entry() {
        let (_d, mut s) = store();
        let a = s.insert(&text("needle"), 10).unwrap();
        s.delete(a).unwrap();
        assert!(s.list(None, 10).unwrap().is_empty());
        assert!(s.list(Some("needle"), 10).unwrap().is_empty());
        assert!(s.formats(a).unwrap().is_empty());
    }

    #[test]
    fn retention_deletes_formats_too() {
        let (_d, mut s) = store();
        let old = s.insert(&text("old"), 10).unwrap();
        let new = s.insert(&text("new"), 20).unwrap();
        let limits = Limits { max_items: 1, ..Limits::default() };
        assert_eq!(s.apply_retention(&limits, 100).unwrap(), 1);
        assert!(s.formats(old).unwrap().is_empty());
        assert_eq!(s.formats(new).unwrap().len(), 1);
        assert_eq!(s.list(None, 10).unwrap().len(), 1);
    }

    #[test]
    fn file_is_not_plain_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _) = open_in(dir.path(), 1, 0);
        s.insert(&text("secret"), 1).unwrap();
        drop(s);
        let mut head = [0u8; 16];
        std::fs::File::open(dir.path().join("history.db")).unwrap().read_exact(&mut head).unwrap();
        assert_ne!(&head, b"SQLite format 3\0");
    }

    #[test]
    fn reopen_with_same_key_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _) = open_in(dir.path(), 1, 0);
        s.insert(&text("kept"), 1).unwrap();
        drop(s);
        let (s, report) = open_in(dir.path(), 1, 5);
        assert_eq!(s.list(None, 10).unwrap().len(), 1);
        assert_eq!(report.recovered_broken, None);
    }

    #[test]
    fn wrong_key_moves_file_aside_and_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _) = open_in(dir.path(), 1, 0);
        s.insert(&text("a"), 1).unwrap();
        drop(s);
        let (s, report) = open_in(dir.path(), 2, 42);
        let broken = dir.path().join("history.db.broken-42");
        assert_eq!(report.recovered_broken, Some(broken.clone()));
        assert!(broken.exists());
        assert!(s.list(None, 10).unwrap().is_empty());
    }

    #[test]
    fn image_clip_gets_preview_and_thumbnail() {
        use base64::Engine;
        let (_d, mut s) = store();
        let e = ClipEvent { formats: vec![fmt("image/png", &png(200, 100))], source_app: None, secret: false };
        s.insert(&e, 1).unwrap();
        let c = &s.list(None, 10).unwrap()[0];
        assert_eq!((c.kind, c.preview.as_str()), (ClipKind::Image, "Image 200\u{d7}100"));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(c.thumbnail_png_base64.as_ref().unwrap())
            .unwrap();
        let t = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).unwrap();
        assert_eq!((t.width(), t.height()), (96, 48));
    }

    #[test]
    fn files_clip_preview_lists_decoded_basenames() {
        let (_d, mut s) = store();
        let uris = b"# comment\r\nfile:///home/u/My%20Doc.txt\r\nfile:///tmp/dir/b.png\r\n";
        let e = ClipEvent { formats: vec![fmt("text/uri-list", uris)], source_app: None, secret: false };
        s.insert(&e, 1).unwrap();
        let c = &s.list(None, 10).unwrap()[0];
        assert_eq!((c.kind, c.preview.as_str()), (ClipKind::Files, "My Doc.txt, b.png"));
    }

    #[cfg(unix)]
    #[test]
    fn file_key_fallback_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let provider = FileKey { path: dir.path().join("sub").join("history.key") };
        let first = provider.key(true).unwrap();
        assert!(first.from_file);
        let mode = std::fs::metadata(&provider.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(provider.key(true).unwrap().key, first.key);
    }

    struct BadKeys;

    impl KeyProvider for BadKeys {
        fn key(&self, _: bool) -> Result<KeyInfo, StoreError> {
            Err(StoreError::KeyUnavailable("keyring down".into()))
        }
    }

    fn db_with_data(dir: &Path) -> (PathBuf, Vec<u8>) {
        let (mut s, _) = open_in(dir, 1, 0);
        s.insert(&text("precious"), 1).unwrap();
        drop(s);
        let path = dir.join("history.db");
        let bytes = std::fs::read(&path).unwrap();
        (path, bytes)
    }

    fn untouched(dir: &Path, path: &Path, bytes: &[u8]) {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        let names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert!(names.iter().all(|n| !n.to_string_lossy().contains("broken")), "{names:?}");
    }

    #[test]
    fn failing_key_provider_leaves_existing_db_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let (path, bytes) = db_with_data(dir.path());
        let r = HistoryStore::open(&path, &BadKeys, 5);
        assert!(matches!(r, Err(StoreError::KeyUnavailable(_))));
        untouched(dir.path(), &path, &bytes);
    }

    struct NoKeyYet<'a>(&'a Path, bool);

    impl KeyProvider for NoKeyYet<'_> {
        /// Mimics OsKeyring with no key anywhere: creates a file key only when allowed.
        fn key(&self, allow_create: bool) -> Result<KeyInfo, StoreError> {
            FileKey { path: self.0.join("history.key") }.key(allow_create).map(|mut i| {
                i.from_file = self.1;
                i
            })
        }
    }

    #[test]
    fn existing_db_with_no_key_anywhere_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let (path, bytes) = db_with_data(dir.path());
        let r = HistoryStore::open(&path, &NoKeyYet(dir.path(), true), 5);
        assert!(matches!(r, Err(StoreError::KeyUnavailable(_))));
        assert!(!dir.path().join("history.key").exists());
        untouched(dir.path(), &path, &bytes);
    }

    #[test]
    fn no_db_creates_key_and_db_then_reopen_uses_same_key_without_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let keys = NoKeyYet(dir.path(), true);
        let (mut s, r1) = HistoryStore::open(&path, &keys, 1).unwrap();
        assert!(r1.key_in_file);
        s.insert(&text("x"), 1).unwrap();
        drop(s);
        let (s, r2) = HistoryStore::open(&path, &keys, 2).unwrap();
        assert_eq!(r2.recovered_broken, None);
        assert_eq!(s.list(None, 10).unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn non_key_open_error_does_not_move_db_aside() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (path, bytes) = db_with_data(dir.path());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&path).is_ok() {
            return; // running as root: permissions do not apply
        }
        let r = HistoryStore::open(&path, &StaticKey([1; 32]), 5);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(r.is_err());
        untouched(dir.path(), &path, &bytes);
    }

    #[test]
    fn broken_file_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = db_with_data(dir.path());
        std::fs::write(dir.path().join("history.db.broken-42"), b"earlier").unwrap();
        let (_s, report) = HistoryStore::open(&path, &StaticKey([2; 32]), 42).unwrap();
        assert_eq!(report.recovered_broken, Some(dir.path().join("history.db.broken-42-2")));
        assert_eq!(std::fs::read(dir.path().join("history.db.broken-42")).unwrap(), b"earlier");
    }

    #[test]
    fn huge_claimed_png_gets_no_thumbnail_but_insert_succeeds() {
        let (_d, mut s) = store();
        let mut p = png(1, 1);
        p[16..20].copy_from_slice(&60_000u32.to_be_bytes());
        p[20..24].copy_from_slice(&60_000u32.to_be_bytes());
        let e = ClipEvent { formats: vec![fmt("image/png", &p)], source_app: None, secret: false };
        s.insert(&e, 1).unwrap();
        let c = &s.list(None, 10).unwrap()[0];
        assert_eq!(c.thumbnail_png_base64, None);
        assert_eq!(c.preview, "Image 60000\u{d7}60000");
    }

    #[test]
    fn png_size_requires_signature_and_ihdr() {
        assert_eq!(png_size(&png(3, 5)), Some((3, 5)));
        let mut bad = png(3, 5);
        bad[1] = b'X';
        assert_eq!(png_size(&bad), None);
        let mut bad = png(3, 5);
        bad[12] = b'X';
        assert_eq!(png_size(&bad), None);
    }

    #[test]
    fn duplicate_mime_in_one_event_keeps_first_and_inserts() {
        let (_d, mut s) = store();
        let e = ClipEvent {
            formats: vec![fmt(TEXT, b"first"), fmt(TEXT, b"second")],
            source_app: None,
            secret: false,
        };
        let id = s.insert(&e, 1).unwrap();
        assert_eq!(s.formats(id).unwrap(), vec![fmt(TEXT, b"first")]);
        assert_eq!(s.find_by_hash(content_hash(&e.formats)).unwrap(), Some(id));
    }
}
