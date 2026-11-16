//! Selection transfers on the X11 Backend's one connection: reading another
//! owner's CLIPBOARD and serving our own, both with INCR for large data.
//!
//! Everything here runs on the Backend thread. Waiting for a transfer polls
//! the connection (2 ms steps, 5 s deadline); events that are not part of the
//! transfer are queued in `pending` for the main loop.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::errors::{ConnectionError, ReplyError};
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

/// Per-transfer deadline (brief: 5 s; owner gone or timeout means no event).
pub(super) const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_STEP: Duration = Duration::from_millis(2);
/// Serve with INCR above this, even when BIG-REQUESTS would allow more: it
/// keeps each ChangeProperty within the core-protocol request size
/// (65535 × 4 bytes). ICCCM allows INCR for any size.
const INCR_CHUNK_CAP: usize = 65535 * 4;
/// Room for the ChangeProperty request header inside one request.
const REQUEST_OVERHEAD: usize = 64;
/// An INCR send with no progress for this long is dropped.
const INCR_SEND_STALE: Duration = Duration::from_secs(10);

x11rb::atom_manager! {
    pub(super) Atoms: AtomsCookie {
        CLIPBOARD,
        TARGETS,
        TIMESTAMP,
        INCR,
        UTF8_STRING,
        TEXT,
        TEXT_PLAIN_UTF8: b"text/plain;charset=utf-8",
        TEXT_HTML: b"text/html",
        IMAGE_PNG: b"image/png",
        URI_LIST: b"text/uri-list",
        PASSWORD_HINT: b"x-kde-passwordManagerHint",
        _NET_WM_PID,
        XFER: b"_CLIPHIST_XFER",
        TIME_PROP: b"_CLIPHIST_TIME",
        WAKE: b"_CLIPHIST_WAKE",
    }
}

/// The X connection is gone; the Backend thread reconnects.
#[derive(Debug)]
pub(super) struct Lost(pub String);

impl From<ConnectionError> for Lost {
    fn from(e: ConnectionError) -> Self {
        Lost(e.to_string())
    }
}

/// A reply, `None` for an X protocol error (e.g. the window is gone).
fn reply<R>(r: Result<R, ReplyError>) -> Result<Option<R>, Lost> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(ReplyError::X11Error(_)) => Ok(None),
        Err(ReplyError::ConnectionError(e)) => Err(e.into()),
    }
}

/// Outcome of reading one target.
#[derive(Debug)]
pub(super) enum Fetch {
    Data(Vec<u8>),
    /// The owner said no (or is gone; the caller checks which).
    Refused,
    /// Bigger than the limit; the transfer was abandoned. Carries the size seen.
    TooLarge(usize),
    /// CLIPBOARD changed hands (or its owner quit) during the transfer.
    OwnerChanged,
    /// A `write()` woke the thread; the read is dropped so the write runs now.
    Interrupted,
    TimedOut,
}

enum Wait<T> {
    Got(T),
    OwnerChanged,
    /// A `write()` woke the thread; the read is dropped so the write runs now.
    Interrupted,
    TimedOut,
}

/// One target we serve while we own CLIPBOARD.
pub(super) struct Offer {
    pub target: Atom,
    pub type_: Atom,
    pub data: Arc<[u8]>,
}

struct Served {
    time: Timestamp,
    offers: Vec<Offer>,
}

struct IncrSend {
    requestor: Window,
    property: Atom,
    type_: Atom,
    data: Arc<[u8]>,
    offset: usize,
    last: Instant,
}

/// One X connection: hidden window, atoms, transfer state.
pub(super) struct Session {
    pub conn: Arc<RustConnection>,
    pub win: Window,
    pub atoms: Atoms,
    /// Events read while waiting for a transfer, for the main loop.
    pub pending: VecDeque<Event>,
    /// Property we read selections into; replaced after an abandoned INCR read
    /// so a stalled owner's late chunks cannot land in the next transfer.
    prop: Atom,
    prop_generation: u32,
    served: Option<Served>,
    sends: Vec<IncrSend>,
    chunk: usize,
}

#[cfg(test)]
static INCR_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Completed INCR reads in this process (tests assert a transfer used INCR).
#[cfg(test)]
pub(super) fn incr_reads() -> usize {
    INCR_READS.load(std::sync::atomic::Ordering::SeqCst)
}

impl Session {
    /// Connects, creates the hidden window and selects XFixes on CLIPBOARD.
    pub fn open(display: Option<&str>) -> Result<Self, String> {
        let (conn, screen) =
            x11rb::connect(display).map_err(|e| format!("cannot connect to X: {e}"))?;
        let err = |e: &dyn std::fmt::Display| format!("X setup failed: {e}");
        let root = conn
            .setup()
            .roots
            .get(screen)
            .ok_or("X screen missing")?
            .root;
        let win = conn.generate_id().map_err(|e| err(&e))?;
        let aux = CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE);
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            win,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &aux,
        )
        .map_err(|e| err(&e))?;
        let atoms = Atoms::new(&conn)
            .map_err(|e| err(&e))?
            .reply()
            .map_err(|e| err(&e))?;
        conn.xfixes_query_version(5, 0)
            .map_err(|e| err(&e))?
            .reply()
            .map_err(|e| format!("XFixes unavailable: {e}"))?;
        let mask = SelectionEventMask::SET_SELECTION_OWNER
            | SelectionEventMask::SELECTION_WINDOW_DESTROY
            | SelectionEventMask::SELECTION_CLIENT_CLOSE;
        conn.xfixes_select_selection_input(win, atoms.CLIPBOARD, mask)
            .map_err(|e| err(&e))?;
        let chunk = conn.maximum_request_bytes().min(INCR_CHUNK_CAP) - REQUEST_OVERHEAD;
        conn.flush().map_err(|e| err(&e))?;
        Ok(Self {
            conn: Arc::new(conn),
            win,
            prop: atoms.XFER,
            atoms,
            pending: VecDeque::new(),
            prop_generation: 0,
            served: None,
            sends: Vec::new(),
            chunk,
        })
    }

    pub fn selection_owner(&self) -> Result<Window, Lost> {
        let r = reply(self.conn.get_selection_owner(self.atoms.CLIPBOARD)?.reply())?;
        Ok(r.map_or(NONE, |r| r.owner))
    }

    /// Polls until `pick` matches an event or the deadline passes. Other events
    /// are queued. With `abort`, a CLIPBOARD XFixes event ends the wait (it is
    /// queued too, so the main loop still sees it), and so does the wake
    /// ClientMessage that `write()` sends (the main loop then drains commands).
    fn wait<T>(
        &mut self,
        deadline: Instant,
        abort: bool,
        mut pick: impl FnMut(&Event) -> Option<T>,
    ) -> Result<Wait<T>, Lost> {
        self.conn.flush()?;
        loop {
            match self.conn.poll_for_event()? {
                Some(ev) => {
                    if let Some(t) = pick(&ev) {
                        return Ok(Wait::Got(t));
                    }
                    if let Event::ClientMessage(m) = &ev {
                        if m.window == self.win && m.type_ == self.atoms.WAKE {
                            if abort {
                                return Ok(Wait::Interrupted);
                            }
                            continue; // the main loop drains commands anyway
                        }
                    }
                    let changed = matches!(&ev, Event::XfixesSelectionNotify(e) if e.selection == self.atoms.CLIPBOARD);
                    self.pending.push_back(ev);
                    if changed && abort {
                        return Ok(Wait::OwnerChanged);
                    }
                }
                None if Instant::now() >= deadline => return Ok(Wait::TimedOut),
                None => std::thread::sleep(POLL_STEP),
            }
        }
    }

    /// Reads CLIPBOARD as `target`, giving up above `limit` bytes.
    pub fn fetch(&mut self, target: Atom, time: Timestamp, limit: usize) -> Result<Fetch, Lost> {
        let (win, clip, prop) = (self.win, self.atoms.CLIPBOARD, self.prop);
        self.conn.delete_property(win, prop)?;
        self.conn.convert_selection(win, clip, target, prop, time)?;
        let deadline = Instant::now() + TRANSFER_TIMEOUT;
        let property = match self.wait(deadline, true, |e| match e {
            // `property` filters out a late answer to an earlier, timed-out
            // request (after an abandoned INCR read it named another property).
            Event::SelectionNotify(n)
                if n.requestor == win
                    && n.selection == clip
                    && n.target == target
                    && (n.property == prop || n.property == NONE) =>
            {
                Some(n.property)
            }
            _ => None,
        })? {
            Wait::Got(p) => p,
            Wait::OwnerChanged => return Ok(Fetch::OwnerChanged),
            Wait::Interrupted => return Ok(Fetch::Interrupted),
            Wait::TimedOut => return Ok(Fetch::TimedOut),
        };
        if property == NONE {
            return Ok(Fetch::Refused);
        }
        // Zero-length read: the type, and the size in bytes_after.
        let Some(head) = reply(
            self.conn
                .get_property(false, win, property, AtomEnum::ANY, 0, 0)?
                .reply(),
        )?
        else {
            return Ok(Fetch::Refused);
        };
        if head.type_ == self.atoms.INCR {
            return self.fetch_incr(property, deadline, limit);
        }
        let size = head.bytes_after as usize;
        if size > limit {
            self.conn.delete_property(win, property)?;
            return Ok(Fetch::TooLarge(size));
        }
        let words = size.div_ceil(4) as u32;
        match reply(
            self.conn
                .get_property(true, win, property, AtomEnum::ANY, 0, words)?
                .reply(),
        )? {
            Some(r) => Ok(Fetch::Data(r.value)),
            None => Ok(Fetch::Refused),
        }
    }

    /// ICCCM INCR: deleting the INCR property starts the transfer; each chunk
    /// arrives as a new property value; a zero-length value ends it.
    fn fetch_incr(
        &mut self,
        property: Atom,
        deadline: Instant,
        limit: usize,
    ) -> Result<Fetch, Lost> {
        let win = self.win;
        let Some(r) = reply(
            self.conn
                .get_property(false, win, property, AtomEnum::ANY, 0, 1)?
                .reply(),
        )?
        else {
            return Ok(Fetch::Refused);
        };
        let lower_bound = r.value32().and_then(|mut v| v.next()).unwrap_or(0) as usize;
        if lower_bound > limit {
            self.abandon_incr()?;
            return Ok(Fetch::TooLarge(lower_bound));
        }
        let mut data = Vec::with_capacity(lower_bound);
        self.conn.delete_property(win, property)?;
        loop {
            let new_value = |e: &Event| match e {
                Event::PropertyNotify(p)
                    if p.window == win && p.atom == property && p.state == Property::NEW_VALUE =>
                {
                    Some(())
                }
                _ => None,
            };
            match self.wait(deadline, true, new_value)? {
                Wait::Got(()) => {}
                Wait::OwnerChanged => {
                    self.abandon_incr()?;
                    return Ok(Fetch::OwnerChanged);
                }
                Wait::Interrupted => {
                    self.abandon_incr()?;
                    return Ok(Fetch::Interrupted);
                }
                Wait::TimedOut => {
                    self.abandon_incr()?;
                    return Ok(Fetch::TimedOut);
                }
            }
            let words = (limit + 1).div_ceil(4) as u32;
            let Some(chunk) = reply(
                self.conn
                    .get_property(true, win, property, AtomEnum::ANY, 0, words)?
                    .reply(),
            )?
            else {
                self.abandon_incr()?;
                return Ok(Fetch::Refused);
            };
            if chunk.value.is_empty() {
                #[cfg(test)]
                INCR_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Ok(Fetch::Data(data));
            }
            data.extend_from_slice(&chunk.value);
            if data.len() > limit {
                self.abandon_incr()?;
                return Ok(Fetch::TooLarge(data.len()));
            }
        }
    }

    /// Moves reads to a fresh property; the abandoned owner may keep writing
    /// to the old one. Only happens on oversize/aborted INCR reads.
    fn abandon_incr(&mut self) -> Result<(), Lost> {
        self.prop_generation += 1;
        let name = format!("_CLIPHIST_XFER_{}", self.prop_generation);
        if let Some(r) = reply(self.conn.intern_atom(false, name.as_bytes())?.reply())? {
            self.prop = r.atom;
        }
        Ok(())
    }

    /// Reads a 32-bit property of another window (e.g. `_NET_WM_PID`).
    pub fn cardinal(&self, window: Window, property: Atom) -> Result<Option<u32>, Lost> {
        let r = reply(
            self.conn
                .get_property(false, window, property, AtomEnum::CARDINAL, 0, 1)?
                .reply(),
        )?;
        Ok(r.and_then(|r| r.value32().and_then(|mut v| v.next())))
    }

    pub fn intern(&self, name: &str) -> Result<Option<Atom>, Lost> {
        Ok(reply(self.conn.intern_atom(false, name.as_bytes())?.reply())?.map(|r| r.atom))
    }

    /// A server timestamp, from a zero-length property append (ICCCM §2.1:
    /// never own a selection with CurrentTime).
    fn server_time(&mut self) -> Result<Option<Timestamp>, Lost> {
        let (win, prop) = (self.win, self.atoms.TIME_PROP);
        self.conn
            .change_property8(PropMode::APPEND, win, prop, AtomEnum::STRING, &[])?;
        let deadline = Instant::now() + TRANSFER_TIMEOUT;
        match self.wait(deadline, false, |e| match e {
            Event::PropertyNotify(p) if p.window == win && p.atom == prop => Some(p.time),
            _ => None,
        })? {
            Wait::Got(t) => Ok(Some(t)),
            Wait::OwnerChanged | Wait::Interrupted | Wait::TimedOut => Ok(None),
        }
    }

    /// Becomes the CLIPBOARD owner serving `offers`. The inner error is for
    /// the caller of `write`; `Lost` means the connection died.
    pub fn own(&mut self, offers: Vec<Offer>) -> Result<Result<(), String>, Lost> {
        let Some(time) = self.server_time()? else {
            return Ok(Err("no server timestamp".into()));
        };
        self.conn
            .set_selection_owner(self.win, self.atoms.CLIPBOARD, time)?;
        if self.selection_owner()? != self.win {
            return Ok(Err("could not become the CLIPBOARD owner".into()));
        }
        self.served = Some(Served { time, offers });
        Ok(Ok(()))
    }

    /// SelectionClear: drops our offers, unless the clear is stale (left over
    /// from an earlier ownership; we own CLIPBOARD again since).
    pub fn on_selection_clear(&mut self) -> Result<(), Lost> {
        if self.selection_owner()? != self.win {
            self.served = None;
        }
        Ok(())
    }

    pub fn on_selection_request(&mut self, r: &SelectionRequestEvent) -> Result<(), Lost> {
        // ICCCM: obsolete clients send property None; use the target name.
        let property = if r.property == NONE {
            r.target
        } else {
            r.property
        };
        let ok = self.answer(r, property)?;
        let ev = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: r.time,
            requestor: r.requestor,
            selection: r.selection,
            target: r.target,
            property: if ok { property } else { NONE },
        };
        self.conn
            .send_event(false, r.requestor, EventMask::NO_EVENT, ev)?;
        self.conn.flush()?;
        Ok(())
    }

    fn answer(&mut self, r: &SelectionRequestEvent, property: Atom) -> Result<bool, Lost> {
        let a = &self.atoms;
        let Some(served) = &self.served else {
            return Ok(false);
        };
        if r.selection != a.CLIPBOARD
            || r.owner != self.win
            || (r.time != CURRENT_TIME && r.time < served.time)
        {
            return Ok(false);
        }
        let c = &self.conn;
        if r.target == a.TARGETS {
            let mut list = vec![a.TARGETS, a.TIMESTAMP];
            list.extend(served.offers.iter().map(|o| o.target));
            c.change_property32(
                PropMode::REPLACE,
                r.requestor,
                property,
                AtomEnum::ATOM,
                &list,
            )?;
            return Ok(true);
        }
        if r.target == a.TIMESTAMP {
            c.change_property32(
                PropMode::REPLACE,
                r.requestor,
                property,
                AtomEnum::INTEGER,
                &[served.time],
            )?;
            return Ok(true);
        }
        let Some(offer) = served.offers.iter().find(|o| o.target == r.target) else {
            return Ok(false);
        };
        if offer.data.len() <= self.chunk {
            c.change_property8(
                PropMode::REPLACE,
                r.requestor,
                property,
                offer.type_,
                &offer.data,
            )?;
            return Ok(true);
        }
        // INCR: announce the size, then send a chunk each time the requestor
        // deletes the property (see on_property_notify).
        let len = u32::try_from(offer.data.len()).unwrap_or(u32::MAX);
        let aux = ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE);
        c.change_window_attributes(r.requestor, &aux)?;
        c.change_property32(PropMode::REPLACE, r.requestor, property, a.INCR, &[len])?;
        let send = IncrSend {
            requestor: r.requestor,
            property,
            type_: offer.type_,
            data: offer.data.clone(),
            offset: 0,
            last: Instant::now(),
        };
        self.sends
            .retain(|s| !(s.requestor == r.requestor && s.property == property));
        self.sends.push(send);
        Ok(true)
    }

    pub fn on_property_notify(&mut self, p: &PropertyNotifyEvent) -> Result<(), Lost> {
        self.sends.retain(|s| s.last.elapsed() < INCR_SEND_STALE);
        if p.state != Property::DELETE {
            return Ok(());
        }
        let Some(i) = self
            .sends
            .iter()
            .position(|s| s.requestor == p.window && s.property == p.atom)
        else {
            return Ok(());
        };
        let s = &mut self.sends[i];
        let end = (s.offset + self.chunk).min(s.data.len());
        self.conn.change_property8(
            PropMode::REPLACE,
            s.requestor,
            s.property,
            s.type_,
            &s.data[s.offset..end],
        )?;
        self.conn.flush()?;
        if s.offset == s.data.len() {
            self.sends.remove(i); // that was the zero-length end marker
        } else {
            s.offset = end;
            s.last = Instant::now();
        }
        Ok(())
    }
}
