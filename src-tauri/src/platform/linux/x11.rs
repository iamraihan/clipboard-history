//! X11 Backend; also the GNOME Wayland Backend through XWayland (ADR 0001).
//!
//! One dedicated thread owns one X connection and runs both the watcher
//! (XFixes on CLIPBOARD) and the selection server for our own writes.
//!
//! `write()` reaches that thread through a command channel. Because the
//! thread blocks in `wait_for_event`, `write()` then wakes it with a
//! ClientMessage sent to our hidden window on the same (thread-safe)
//! connection, and waits for the thread's answer. The thread drains the
//! channel before each event. The wake also interrupts a read in progress
//! (that copy is dropped, no ClipEvent), so a slow owner cannot delay a
//! write. Each write carries a deadline; one whose caller already gave up is
//! never applied.
//!
//! The thread stops when the X11Backend is dropped, or when a ClipEvent send
//! fails because the receiver is gone. std's mpsc cannot tell that the
//! receiver is gone without sending, so while the display is down (backoff)
//! the thread notices it only at the first send after reconnecting. If the
//! connection dies (Xwayland killed or restarted), it reconnects with
//! backoff, re-selects XFixes and reads the current CLIPBOARD again; what we
//! were serving is not re-owned.

use super::x11_transfer::{Fetch, Lost, Offer, Session};
use crate::core::clip::{ClipEvent, Format};
use crate::core::rules::Limits;
use crate::platform::{BackendError, Capabilities, ClipboardBackend};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, ClientMessageEvent, ConnectionExt as _, EventMask, Timestamp, Window,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::{CURRENT_TIME, NONE};

const TEXT_MIME: &str = "text/plain;charset=utf-8";
/// Slack over the max Clip size for one target; the total is checked in core.
const TARGET_MARGIN: usize = 64 * 1024;
const BACKOFF_START: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
/// How long `write` waits for the thread; reads in progress are interrupted,
/// so this only covers getting a server timestamp and taking ownership.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

enum Command {
    /// Formats, the caller's deadline, where to answer.
    Write(Vec<Format>, Instant, Sender<Result<(), BackendError>>),
    Shutdown,
}

/// Connection, window and wake atom of the live session, for `wake()`.
type Waker = Arc<Mutex<Option<(Arc<RustConnection>, Window, Atom)>>>;

pub struct X11Backend {
    display: Option<String>,
    caps: Capabilities,
    /// The connected session until `watch` hands it to the thread.
    session: Mutex<Option<Session>>,
    commands: Mutex<Option<Sender<Command>>>,
    waker: Waker,
}

impl X11Backend {
    /// Connects to `display` (None: `$DISPLAY`).
    pub fn connect(display: Option<&str>) -> Result<Self, BackendError> {
        let session = Session::open(display).map_err(BackendError)?;
        let session_type = std::env::var("XDG_SESSION_TYPE").ok();
        Ok(Self {
            display: display.map(str::to_owned),
            caps: capabilities_for(session_type.as_deref()),
            session: Mutex::new(Some(session)),
            commands: Mutex::new(None),
            waker: Arc::default(),
        })
    }

    fn wake(&self) {
        let guard = self.waker.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((conn, win, atom)) = guard.as_ref() {
            let ev = ClientMessageEvent::new(32, *win, *atom, [0u32; 5]);
            // Best effort: if the connection is dead the thread is reconnecting
            // and drains the channel itself.
            let _ = conn.send_event(false, *win, EventMask::NO_EVENT, ev);
            let _ = conn.flush();
        }
    }

    fn send(&self, cmd: Command) -> Result<(), BackendError> {
        let guard = self.commands.lock().unwrap_or_else(|p| p.into_inner());
        let tx = guard
            .as_ref()
            .ok_or_else(|| BackendError("X11 backend is not watching".into()))?;
        tx.send(cmd)
            .map_err(|_| BackendError("X11 backend thread has stopped".into()))
    }
}

impl Drop for X11Backend {
    fn drop(&mut self) {
        if self.send(Command::Shutdown).is_ok() {
            self.wake();
        }
    }
}

/// Capabilities as a pure function of `XDG_SESSION_TYPE`. Under XWayland the
/// CLIPBOARD owner is Mutter's own window, so the source app is unknown.
pub fn capabilities_for(session_type: Option<&str>) -> Capabilities {
    let xwayland = session_type == Some("wayland");
    Capabilities {
        source_app: !xwayland,
        auto_paste: !xwayland,
        files: true,
        backend_name: if xwayland { "xwayland" } else { "x11" }.into(),
    }
}

impl ClipboardBackend for X11Backend {
    fn watch(&self, tx: Sender<ClipEvent>) -> Result<(), BackendError> {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .ok_or_else(|| BackendError("X11 backend is already watching".into()))?;
        let (cmd_tx, cmd_rx) = channel();
        *self.commands.lock().unwrap_or_else(|p| p.into_inner()) = Some(cmd_tx);
        let worker = Worker {
            display: self.display.clone(),
            source_app: self.caps.source_app,
            tx,
            commands: cmd_rx,
            waker: self.waker.clone(),
        };
        std::thread::Builder::new()
            .name("x11-clipboard".into())
            .spawn(move || worker.run(session))
            .map_err(|e| BackendError(format!("cannot start X11 thread: {e}")))?;
        Ok(())
    }

    fn write(&self, formats: &[Format]) -> Result<(), BackendError> {
        let (reply_tx, reply_rx) = channel();
        let deadline = Instant::now() + WRITE_TIMEOUT;
        self.send(Command::Write(formats.to_vec(), deadline, reply_tx))?;
        self.wake();
        reply_rx
            .recv_timeout(WRITE_TIMEOUT)
            .unwrap_or_else(|_| Err(BackendError("X11 backend did not take the write".into())))
    }

    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }
}

fn max_target_bytes() -> usize {
    usize::try_from(Limits::default().max_item_bytes).unwrap_or(usize::MAX) + TARGET_MARGIN
}

struct Worker {
    display: Option<String>,
    source_app: bool,
    tx: Sender<ClipEvent>,
    commands: Receiver<Command>,
    waker: Waker,
}

/// Why `pump` returned without losing the connection.
struct Stop;

impl Worker {
    fn run(self, mut session: Session) {
        loop {
            self.set_waker(Some((
                session.conn.clone(),
                session.win,
                session.atoms.WAKE,
            )));
            let result = self.pump(&mut session);
            // Drop the waker's handle too, so the connection closes with us.
            self.set_waker(None);
            match result {
                Ok(Stop) => return,
                Err(Lost(why)) => eprintln!("x11: connection lost ({why}); reconnecting"),
            }
            match self.reconnect() {
                Some(s) => session = s,
                None => return,
            }
        }
    }

    fn set_waker(&self, w: Option<(Arc<RustConnection>, Window, Atom)>) {
        *self.waker.lock().unwrap_or_else(|p| p.into_inner()) = w;
    }

    /// Retries with backoff (0.5 s doubling to 10 s). None means shut down.
    fn reconnect(&self) -> Option<Session> {
        let mut backoff = BACKOFF_START;
        loop {
            match self.commands.recv_timeout(backoff) {
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => return None,
                Ok(Command::Write(_, _, reply)) => {
                    let _ = reply.send(Err(BackendError("X display is disconnected".into())));
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            match Session::open(self.display.as_deref()) {
                Ok(s) => return Some(s),
                Err(e) => eprintln!("x11: reconnect failed: {e}"),
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    fn pump(&self, s: &mut Session) -> Result<Stop, Lost> {
        // Copies made while nobody watched produce no XFixes event.
        let owner = s.selection_owner()?;
        if owner != NONE && owner != s.win && !self.emit(s, owner, CURRENT_TIME)? {
            return Ok(Stop);
        }
        loop {
            loop {
                match self.commands.try_recv() {
                    Ok(Command::Write(_, deadline, _)) if Instant::now() >= deadline => {
                        // The caller already returned an error; never apply it late.
                        eprintln!("x11: dropping a write whose caller gave up");
                    }
                    Ok(Command::Write(formats, _, reply)) => {
                        let offers = offers_for(s, &formats)?;
                        let _ = reply.send(s.own(offers)?.map_err(BackendError));
                    }
                    Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => return Ok(Stop),
                    Err(TryRecvError::Empty) => break,
                }
            }
            let ev = match s.pending.pop_front() {
                Some(ev) => ev,
                None => s.conn.wait_for_event()?,
            };
            let clip = s.atoms.CLIPBOARD;
            match ev {
                Event::XfixesSelectionNotify(e) if e.selection == clip => {
                    // Our own write (no Echo from the Backend), or nobody owns it.
                    if e.owner == NONE || e.owner == s.win {
                        continue;
                    }
                    // A newer change is already queued; read that one instead.
                    let newer = s.pending.iter().any(
                        |p| matches!(p, Event::XfixesSelectionNotify(n) if n.selection == clip),
                    );
                    if !newer && !self.emit(s, e.owner, e.selection_timestamp)? {
                        return Ok(Stop);
                    }
                }
                Event::SelectionRequest(r) => s.on_selection_request(&r)?,
                Event::SelectionClear(c) if c.selection == clip && c.owner == s.win => {
                    s.on_selection_clear()?
                }
                Event::PropertyNotify(p) => s.on_property_notify(&p)?,
                // Errors from transfers (e.g. a requestor window that vanished)
                // and the wake ClientMessage need nothing here.
                _ => {}
            }
        }
    }

    /// Reads the change and sends it. False when the receiver is gone.
    fn emit(&self, s: &mut Session, owner: Window, time: Timestamp) -> Result<bool, Lost> {
        // We took CLIPBOARD back since this change (a write): nothing to read.
        if s.selection_owner()? == s.win {
            return Ok(true);
        }
        let Some(mut event) = read_clipboard(s, owner, time)? else {
            return Ok(true);
        };
        if self.source_app {
            event.source_app = source_app_of(s, owner)?;
        }
        Ok(self.tx.send(event).is_ok())
    }
}

/// Reads the Formats we keep from `owner`. None: owner gone, timed out or
/// nothing usable (logged).
fn read_clipboard(
    s: &mut Session,
    owner: Window,
    time: Timestamp,
) -> Result<Option<ClipEvent>, Lost> {
    let a = s.atoms;
    let limit = max_target_bytes();
    let targets: Vec<Atom> = match s.fetch(a.TARGETS, time, limit)? {
        Fetch::Data(d) => d
            // 32-bit atoms; `chunks` + `try_from` instead of `as_chunks`
            // (Rust 1.88+); a trailing partial chunk is dropped.
            .chunks(4)
            .filter_map(|c| <[u8; 4]>::try_from(c).ok())
            .map(u32::from_ne_bytes)
            .collect(),
        other => {
            eprintln!("x11: TARGETS read failed: {other:?}");
            return Ok(None);
        }
    };
    if targets.contains(&a.PASSWORD_HINT) {
        match s.fetch(a.PASSWORD_HINT, time, 64)? {
            Fetch::Data(v) if v.trim_ascii() == b"secret" => {
                return Ok(Some(ClipEvent {
                    formats: vec![],
                    source_app: None,
                    secret: true,
                }));
            }
            Fetch::OwnerChanged | Fetch::Interrupted | Fetch::TimedOut => return Ok(None),
            _ => {}
        }
    }
    let text = [a.TEXT_PLAIN_UTF8, a.UTF8_STRING]
        .into_iter()
        .find(|t| targets.contains(t));
    let wanted = [
        (text, TEXT_MIME),
        (Some(a.TEXT_HTML), "text/html"),
        (Some(a.IMAGE_PNG), "image/png"),
        (Some(a.URI_LIST), "text/uri-list"),
    ];
    let mut formats = Vec::new();
    for (target, mime) in wanted {
        let Some(target) = target.filter(|t| targets.contains(t)) else {
            continue;
        };
        match s.fetch(target, time, limit)? {
            Fetch::Data(data) => formats.push(Format {
                mime: mime.into(),
                data,
            }),
            Fetch::Refused if s.selection_owner()? != owner => {
                eprintln!("x11: CLIPBOARD owner went away mid-read; no ClipEvent");
                return Ok(None);
            }
            Fetch::Refused => eprintln!("x11: owner refused {mime}"),
            Fetch::TooLarge(n) => {
                eprintln!("x11: skipping {mime}: {n} bytes is over the {limit}-byte limit")
            }
            Fetch::OwnerChanged | Fetch::Interrupted | Fetch::TimedOut => {
                eprintln!("x11: CLIPBOARD read of {mime} interrupted; no ClipEvent");
                return Ok(None);
            }
        }
    }
    if formats.is_empty() {
        eprintln!("x11: CLIPBOARD has no Format we keep");
        return Ok(None);
    }
    Ok(Some(ClipEvent {
        formats,
        source_app: None,
        secret: false,
    }))
}

/// `_NET_WM_PID` on the owner window, then `/proc/<pid>/comm`.
fn source_app_of(s: &Session, owner: Window) -> Result<Option<String>, Lost> {
    let Some(pid) = s.cardinal(owner, s.atoms._NET_WM_PID)? else {
        return Ok(None);
    };
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok();
    Ok(comm
        .map(|c| c.trim_end().to_owned())
        .filter(|c| !c.is_empty()))
}

/// The targets we serve for `formats`; plain text also as UTF8_STRING,
/// STRING (Latin-1) and TEXT.
fn offers_for(s: &Session, formats: &[Format]) -> Result<Vec<Offer>, Lost> {
    let a = s.atoms;
    let mut offers = Vec::new();
    for f in formats {
        let data: Arc<[u8]> = f.data.clone().into();
        if f.mime == TEXT_MIME {
            let latin1: Vec<u8> = String::from_utf8_lossy(&f.data)
                .chars()
                .map(|c| u8::try_from(c).unwrap_or(b'?'))
                .collect();
            for (target, type_) in [
                (a.UTF8_STRING, a.UTF8_STRING),
                (a.TEXT_PLAIN_UTF8, a.TEXT_PLAIN_UTF8),
                (a.TEXT, a.UTF8_STRING),
            ] {
                offers.push(Offer {
                    target,
                    type_,
                    data: data.clone(),
                });
            }
            let string = Atom::from(x11rb::protocol::xproto::AtomEnum::STRING);
            offers.push(Offer {
                target: string,
                type_: string,
                data: latin1.into(),
            });
        } else if let Some(atom) = s.intern(&f.mime)? {
            offers.push(Offer {
                target: atom,
                type_: atom,
                data,
            });
        }
    }
    Ok(offers)
}

#[cfg(test)]
mod tests {
    use super::super::x11_test_support::{read_as_client, Mode, Owner, PrivateX};
    use super::*;
    use std::sync::mpsc::{channel, Receiver};
    use std::time::Duration;

    const TEXT: &str = "text/plain;charset=utf-8";
    const WAIT: Duration = Duration::from_secs(5);

    fn watching(x: &PrivateX) -> (X11Backend, Receiver<ClipEvent>) {
        let b = X11Backend::connect(Some(&x.display)).expect("connect");
        let (tx, rx) = channel();
        b.watch(tx).expect("watch");
        (b, rx)
    }

    fn fmt(mime: &str, data: &[u8]) -> Format {
        Format {
            mime: mime.into(),
            data: data.to_vec(),
        }
    }

    fn next(rx: &Receiver<ClipEvent>) -> ClipEvent {
        rx.recv_timeout(WAIT).expect("a ClipEvent")
    }

    #[test]
    fn capabilities_follow_session_type() {
        let x11 = capabilities_for(Some("x11"));
        assert_eq!(
            x11,
            Capabilities {
                source_app: true,
                auto_paste: true,
                files: true,
                backend_name: "x11".into()
            }
        );
        assert_eq!(capabilities_for(None), x11);
        let xw = capabilities_for(Some("wayland"));
        assert_eq!(
            xw,
            Capabilities {
                source_app: false,
                auto_paste: false,
                files: true,
                backend_name: "xwayland".into()
            }
        );
    }

    #[test]
    fn detects_text_copy() {
        let x = PrivateX::start();
        let (_b, rx) = watching(&x);
        let _o = Owner::copy(&x.display, &[("UTF8_STRING", b"hello")], Mode::Serve);
        let e = next(&rx);
        assert_eq!(e.formats, vec![fmt(TEXT, b"hello")]);
        assert!(!e.secret);
        let comm = std::fs::read_to_string("/proc/self/comm").unwrap();
        assert_eq!(e.source_app.as_deref(), Some(comm.trim_end()));
    }

    #[test]
    fn reads_existing_clipboard_when_watch_starts() {
        let x = PrivateX::start();
        let _o = Owner::copy(&x.display, &[("UTF8_STRING", b"before")], Mode::Serve);
        let (_b, rx) = watching(&x);
        assert_eq!(next(&rx).formats, vec![fmt(TEXT, b"before")]);
    }

    #[test]
    fn reads_html_and_plain_together() {
        let x = PrivateX::start();
        let (_b, rx) = watching(&x);
        let offers: &[(&str, &[u8])] = &[("text/html", b"<b>hi</b>"), ("UTF8_STRING", b"hi")];
        let _o = Owner::copy(&x.display, offers, Mode::Serve);
        let mut got = next(&rx).formats;
        got.sort_by(|a, b| a.mime.cmp(&b.mime));
        assert_eq!(got, vec![fmt("text/html", b"<b>hi</b>"), fmt(TEXT, b"hi")]);
    }

    fn fake_png(len: usize) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend((0..len - 8).map(|i| (i % 251) as u8));
        v
    }

    #[test]
    fn reads_png_larger_than_max_request_via_incr() {
        let x = PrivateX::start();
        let (_b, rx) = watching(&x);
        let png = fake_png(2 * 1024 * 1024);
        let before = super::super::x11_transfer::incr_reads();
        let _o = Owner::copy(&x.display, &[("image/png", &png)], Mode::Serve);
        let e = next(&rx);
        assert_eq!(e.formats, vec![fmt("image/png", &png)]);
        assert!(
            super::super::x11_transfer::incr_reads() > before,
            "transfer did not use INCR"
        );
    }

    #[test]
    fn oversized_target_is_dropped_but_rest_is_kept() {
        let x = PrivateX::start();
        let (_b, rx) = watching(&x);
        let png = fake_png(11 * 1024 * 1024);
        let offers: &[(&str, &[u8])] = &[("image/png", &png), ("UTF8_STRING", b"caption")];
        let _o = Owner::copy(&x.display, offers, Mode::Serve);
        assert_eq!(next(&rx).formats, vec![fmt(TEXT, b"caption")]);
    }

    #[test]
    fn password_hint_sets_secret_flag() {
        let x = PrivateX::start();
        let (_b, rx) = watching(&x);
        let offers: &[(&str, &[u8])] = &[
            ("UTF8_STRING", b"hunter2"),
            ("x-kde-passwordManagerHint", b"secret"),
        ];
        let _o = Owner::copy(&x.display, offers, Mode::Serve);
        let e = next(&rx);
        assert!(e.secret);
        assert!(e.formats.is_empty(), "no data is fetched for a secret");
    }

    #[test]
    fn owner_vanishing_mid_read_yields_no_event() {
        let x = PrivateX::start();
        let (_b, rx) = watching(&x);
        Owner::copy(
            &x.display,
            &[("UTF8_STRING", b"gone")],
            Mode::VanishAfterTargets,
        )
        .join();
        let _o = Owner::copy(&x.display, &[("UTF8_STRING", b"after")], Mode::Serve);
        assert_eq!(
            next(&rx).formats,
            vec![fmt(TEXT, b"after")],
            "the vanished copy must not emit"
        );
    }

    #[test]
    fn own_write_is_served_to_other_clients() {
        let x = PrivateX::start();
        let (b, _rx) = watching(&x);
        b.write(&[fmt(TEXT, b"hello")]).expect("write");
        assert_eq!(
            read_as_client(&x.display, "UTF8_STRING"),
            Some((b"hello".to_vec(), false))
        );
        assert_eq!(
            read_as_client(&x.display, "STRING"),
            Some((b"hello".to_vec(), false))
        );
        assert_eq!(
            read_as_client(&x.display, TEXT),
            Some((b"hello".to_vec(), false))
        );
        assert_eq!(read_as_client(&x.display, "image/png"), None);
    }

    #[test]
    fn write_during_slow_read_completes_quickly() {
        let x = PrivateX::start();
        let (b, rx) = watching(&x);
        let slow = Owner::copy(&x.display, &[("UTF8_STRING", b"slow")], Mode::NeverAnswer);
        slow.wait_stalled(); // the Backend is now inside a 5 s read
        let started = std::time::Instant::now();
        b.write(&[fmt(TEXT, b"mine")]).expect("write");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "write took {:?}",
            started.elapsed()
        );
        assert_eq!(
            read_as_client(&x.display, "UTF8_STRING"),
            Some((b"mine".to_vec(), false))
        );
        assert!(rx.try_recv().is_err(), "the interrupted read emits nothing");
    }

    #[test]
    fn own_large_write_is_served_via_incr() {
        let x = PrivateX::start();
        let (b, _rx) = watching(&x);
        let png = fake_png(2 * 1024 * 1024);
        b.write(&[fmt("image/png", &png)]).expect("write");
        assert_eq!(read_as_client(&x.display, "image/png"), Some((png, true)));
    }

    #[test]
    fn own_write_does_not_emit_event() {
        let x = PrivateX::start();
        let (b, rx) = watching(&x);
        b.write(&[fmt(TEXT, b"mine")]).expect("write");
        let _o = Owner::copy(&x.display, &[("UTF8_STRING", b"theirs")], Mode::Serve);
        assert_eq!(next(&rx).formats, vec![fmt(TEXT, b"theirs")]);
    }

    #[test]
    fn reconnects_after_x_server_restart() {
        let x = PrivateX::start();
        let (_b, rx) = watching(&x);
        let number = x.number;
        drop(x);
        let x = PrivateX::start_on(number);
        let _o = Owner::copy(&x.display, &[("UTF8_STRING", b"back")], Mode::Serve);
        assert_eq!(next(&rx).formats, vec![fmt(TEXT, b"back")]);
    }
}
