//! macOS clipboard Backend.
//!
//! macOS sends no change notification, so one thread polls
//! `NSPasteboard.changeCount` every 500 ms and reads every Format at once:
//! a pasteboard item is only valid until the owner changes. A copy whose
//! types carry a secret marker (nspasteboard.org) is reported as a secret
//! ClipEvent without reading any data.
//!
//! Our own write is recognised by its change count, before the marker check
//! (it carries `TransientType`), as X11 skips its own ownership. Writes and
//! poll reads hold the same lock, so a poll never reads a half-written item.

use crate::core::clip::{ClipEvent, Format};
use crate::core::pasteboard::{
    classify_change, file_urls, is_marked_secret, source_app, uri_list, PasteboardChange, FILE_URL_UTI,
    FORMAT_UTIS, SOURCE_MARKER, TRANSIENT_MARKER, URI_LIST_MIME,
};
use crate::platform::{BackendError, Capabilities, ClipboardBackend};
use crate::state::lock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardAccessBehavior, NSPasteboardItem, NSPasteboardWriting, NSWorkspace};
use objc2_foundation::{NSArray, NSData, NSOperatingSystemVersion, NSProcessInfo, NSString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const POLL_EVERY: Duration = Duration::from_millis(500);
const DENIED: &str =
    "Clipboard access is denied in System Settings \u{2192} Privacy & Security \u{2192} Paste from Other Apps";

/// Which pasteboard to watch. `Retained<NSPasteboard>` is not `Send`, so
/// each thread looks it up by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteboardName {
    General,
    /// A private pasteboard (tests): never asks for privacy consent.
    Named(String),
}

impl PasteboardName {
    fn open(&self) -> Retained<NSPasteboard> {
        match self {
            Self::General => NSPasteboard::generalPasteboard(),
            Self::Named(n) => NSPasteboard::pasteboardWithName(&ns(n)),
        }
    }
}

pub struct MacBackend {
    name: PasteboardName,
    /// Change count right after our last write; held while writing and while
    /// a poll reads.
    own: Arc<Mutex<Option<isize>>>,
    stop: Arc<AtomicBool>,
}

impl MacBackend {
    pub fn new(name: PasteboardName) -> Self {
        Self { name, own: Arc::default(), stop: Arc::default() }
    }
}

impl Drop for MacBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A pasteboard type (UTI) or string value as an NSString.
fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

/// The user set this app to "Always Deny" (macOS 15.4+; `accessBehavior`
/// does not exist before).
fn access_denied(pb: &NSPasteboard) -> bool {
    let v15_4 = NSOperatingSystemVersion { majorVersion: 15, minorVersion: 4, patchVersion: 0 };
    NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(v15_4)
        && pb.accessBehavior() == NSPasteboardAccessBehavior::AlwaysDeny
}

impl ClipboardBackend for MacBackend {
    fn watch(&self, tx: Sender<ClipEvent>) -> Result<(), BackendError> {
        if self.name == PasteboardName::General && access_denied(&self.name.open()) {
            return Err(BackendError(DENIED.into()));
        }
        let (name, own, stop) = (self.name.clone(), self.own.clone(), self.stop.clone());
        std::thread::Builder::new()
            .name("macos-pasteboard".into())
            .spawn(move || poll(&name, &own, &stop, &tx))
            .map(|_| ())
            .map_err(|e| BackendError(format!("cannot start the pasteboard watcher: {e}")))
    }

    fn write(&self, formats: &[Format]) -> Result<(), BackendError> {
        let pb = self.name.open();
        let mut own = lock(&self.own);
        pb.clearContents();
        let items = items_for(formats);
        let objects: Vec<&ProtocolObject<dyn NSPasteboardWriting>> =
            items.iter().map(|i| ProtocolObject::from_ref(&**i)).collect();
        let written = pb.writeObjects(&NSArray::from_slice(&objects));
        // The count after the whole write, whether or not writeObjects moved it.
        *own = Some(pb.changeCount());
        if written {
            Ok(())
        } else {
            Err(BackendError("the pasteboard refused the write".into()))
        }
    }

    fn capabilities(&self) -> Capabilities {
        // Auto-paste exists; whether it works right now (Accessibility) is
        // `MacPaster::ready`, which can change while the app runs.
        Capabilities { source_app: true, auto_paste: true, files: true, backend_name: "macos".into() }
    }
}

/// Reads once at start (copies made while nobody watched), then on every
/// change that is not ours. Stops when the Backend is dropped or the
/// receiver is gone.
fn poll(name: &PasteboardName, own: &Mutex<Option<isize>>, stop: &AtomicBool, tx: &Sender<ClipEvent>) {
    let pb = name.open();
    let mut last_seen: Option<isize> = None;
    while !stop.load(Ordering::Relaxed) {
        let guard = lock(own);
        let count = pb.changeCount();
        let change = match last_seen {
            Some(last) => classify_change(last, count, *guard),
            None if *guard == Some(count) => PasteboardChange::Own,
            None => PasteboardChange::Foreign,
        };
        last_seen = Some(count);
        let event = if change == PasteboardChange::Foreign { read(&pb) } else { None };
        drop(guard);
        if let Some(e) = event {
            if tx.send(e).is_err() {
                return;
            }
        }
        std::thread::sleep(POLL_EVERY);
    }
}

/// The current copy as a ClipEvent; None when it has no Format we keep.
fn read(pb: &NSPasteboard) -> Option<ClipEvent> {
    let types: Vec<String> = pb.types().map(|a| a.iter().map(|t| t.to_string()).collect()).unwrap_or_default();
    if is_marked_secret(&types) {
        return Some(ClipEvent { formats: vec![], source_app: None, secret: true });
    }
    let mut formats = Vec::new();
    for (mime, ty) in FORMAT_UTIS {
        if !types.iter().any(|t| t == ty) {
            continue;
        }
        if let Some(data) = pb.dataForType(&ns(ty)) {
            formats.push(Format { mime: mime.into(), data: data.to_vec() });
        }
    }
    let urls: Vec<String> = pb
        .pasteboardItems()
        .map(|items| items.iter().filter_map(|i| i.stringForType(&ns(FILE_URL_UTI))).map(|s| s.to_string()).collect())
        .unwrap_or_default();
    if let Some(list) = uri_list(&urls) {
        formats.push(Format { mime: URI_LIST_MIME.into(), data: list.into_bytes() });
    }
    if formats.is_empty() {
        eprintln!("macos: the pasteboard has no Format we keep");
        return None;
    }
    let marker = pb.stringForType(&ns(SOURCE_MARKER)).map(|s| s.to_string());
    let front = NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|a| a.bundleIdentifier())
        .map(|s| s.to_string());
    Some(ClipEvent { formats, source_app: source_app(marker.as_deref(), front.as_deref()), secret: false })
}

/// Pasteboard items for a Clip: one per file URL (the other Formats on the
/// first), or one item with every Format. The first carries our markers.
fn items_for(formats: &[Format]) -> Vec<Retained<NSPasteboardItem>> {
    let urls = formats
        .iter()
        .find(|f| f.mime == URI_LIST_MIME)
        .map(|f| file_urls(&String::from_utf8_lossy(&f.data)))
        .unwrap_or_default();
    let items: Vec<Retained<NSPasteboardItem>> = (0..urls.len().max(1)).map(|_| NSPasteboardItem::new()).collect();
    for (item, url) in items.iter().zip(&urls) {
        item.setString_forType(&ns(url), &ns(FILE_URL_UTI));
    }
    let first = &items[0];
    for f in formats {
        if let Some((_, ty)) = FORMAT_UTIS.iter().find(|(mime, _)| *mime == f.mime) {
            first.setData_forType(&NSData::with_bytes(&f.data), &ns(ty));
        }
    }
    first.setData_forType(&NSData::with_bytes(&[]), &ns(TRANSIENT_MARKER));
    first.setString_forType(&ns(""), &ns(SOURCE_MARKER));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    const WAIT: Duration = Duration::from_secs(2);

    /// A private pasteboard and a Backend on it: never the user's clipboard,
    /// never a privacy prompt.
    fn private() -> (Retained<NSPasteboard>, MacBackend) {
        let pb = NSPasteboard::pasteboardWithUniqueName();
        let backend = MacBackend::new(PasteboardName::Named(pb.name().to_string()));
        (pb, backend)
    }

    fn put(pb: &NSPasteboard, entries: &[(&str, &[u8])]) {
        pb.clearContents();
        for (ty, data) in entries {
            assert!(pb.setData_forType(Some(&NSData::with_bytes(data)), &ns(ty)), "{ty}");
        }
    }

    fn text(s: &str) -> Format {
        Format { mime: "text/plain;charset=utf-8".into(), data: s.as_bytes().to_vec() }
    }

    #[test]
    fn foreign_text_and_html_become_one_clip_event() {
        let (pb, backend) = private();
        put(&pb, &[("public.utf8-plain-text", b"hi"), ("public.html", b"<b>hi</b>")]);
        let (tx, rx) = channel();
        backend.watch(tx).unwrap();
        let e = rx.recv_timeout(WAIT).expect("a ClipEvent for the copy");
        assert!(!e.secret);
        assert_eq!(
            e.formats,
            vec![text("hi"), Format { mime: "text/html".into(), data: b"<b>hi</b>".to_vec() }]
        );
    }

    #[test]
    fn concealed_item_emits_secret_event_without_formats() {
        let (pb, backend) = private();
        put(&pb, &[("public.utf8-plain-text", b"hunter2"), ("org.nspasteboard.ConcealedType", b"")]);
        let (tx, rx) = channel();
        backend.watch(tx).unwrap();
        let e = rx.recv_timeout(WAIT).expect("a secret ClipEvent");
        assert!(e.secret);
        assert!(e.formats.is_empty());
        assert_eq!(e.source_app, None);
    }

    #[test]
    fn write_is_not_read_back_as_a_clip() {
        let (pb, backend) = private();
        let (tx, rx) = channel();
        backend.watch(tx).unwrap();
        backend.write(&[text("x")]).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(1500)).is_err(), "our own write must not come back");
        let types: Vec<String> = pb.types().unwrap().iter().map(|t| t.to_string()).collect();
        assert!(types.iter().any(|t| t == TRANSIENT_MARKER), "{types:?}");
        assert_eq!(pb.stringForType(&ns("public.utf8-plain-text")).unwrap().to_string(), "x");
    }

    #[test]
    fn files_are_written_one_pasteboard_item_each() {
        let (pb, backend) = private();
        let list = Format { mime: URI_LIST_MIME.into(), data: b"file:///tmp/a\r\nfile:///tmp/b".to_vec() };
        backend.write(&[list]).unwrap();
        let items = pb.pasteboardItems().unwrap();
        let urls: Vec<String> =
            items.iter().filter_map(|i| i.stringForType(&ns(FILE_URL_UTI))).map(|s| s.to_string()).collect();
        assert_eq!(urls, ["file:///tmp/a", "file:///tmp/b"]);
    }
}
