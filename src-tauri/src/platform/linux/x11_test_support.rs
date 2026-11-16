//! Helpers for the X11 tests: a private Xvfb per test, and an independent
//! "other app" (owner and reader) written straight against x11rb, so a
//! mistake in the Backend's transfer code cannot hide in the test client.

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

/// The test owner serves data above this size with INCR, like GTK and Qt
/// do for anything bigger than a server without BIG-REQUESTS accepts.
const OWNER_INCR_CHUNK: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);

/// An Xvfb of the test's own, so parallel tests never share a CLIPBOARD.
pub struct PrivateX {
    child: Child,
    _stdout: BufReader<ChildStdout>,
    pub number: u32,
    pub display: String,
}

impl PrivateX {
    pub fn start() -> Self {
        Self::spawn(None)
    }

    /// Starts Xvfb on a given display number (to restart a killed one).
    pub fn start_on(number: u32) -> Self {
        Self::spawn(Some(number))
    }

    fn spawn(number: Option<u32>) -> Self {
        let mut cmd = Command::new("Xvfb");
        if let Some(n) = number {
            cmd.arg(format!(":{n}"));
        }
        // -displayfd 1: Xvfb prints its display number on stdout once ready.
        cmd.args([
            "-displayfd",
            "1",
            "-nolisten",
            "tcp",
            "-screen",
            "0",
            "64x64x24",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        let mut child = cmd.spawn().expect("run Xvfb (install the xvfb package)");
        let mut out = BufReader::new(child.stdout.take().expect("Xvfb stdout"));
        let mut line = String::new();
        out.read_line(&mut line).expect("read Xvfb display number");
        let number: u32 = line.trim().parse().expect("Xvfb display number");
        Self {
            child,
            _stdout: out,
            number,
            display: format!(":{number}"),
        }
    }
}

impl Drop for PrivateX {
    fn drop(&mut self) {
        // SIGTERM (not Child::kill's SIGKILL) so Xvfb removes its lock file
        // and socket, and the display number can be reused.
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let _ = self.child.wait();
    }
}

pub enum Mode {
    Serve,
    /// Answer TARGETS, then disconnect without serving anything else.
    VanishAfterTargets,
    /// Answer TARGETS, then leave every other request unanswered (a slow app).
    NeverAnswer,
}

/// Another app that owns CLIPBOARD and serves `offers` (target name, data).
pub struct Owner {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    stalled: Receiver<()>,
}

impl Owner {
    /// Returns once the owner holds CLIPBOARD.
    pub fn copy(display: &str, offers: &[(&str, &[u8])], mode: Mode) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let offers: Vec<(String, Vec<u8>)> = offers
            .iter()
            .map(|(t, d)| (t.to_string(), d.to_vec()))
            .collect();
        let (ready_tx, ready_rx) = channel();
        let (stalled_tx, stalled) = channel();
        let (display, flag) = (display.to_string(), stop.clone());
        let handle = thread::spawn(move || {
            let ready = || {
                let _ = ready_tx.send(());
            };
            let stall = || {
                let _ = stalled_tx.send(());
            };
            owner_thread(&display, &offers, &mode, &flag, &ready, &stall)
        });
        ready_rx
            .recv_timeout(TIMEOUT)
            .expect("owner took CLIPBOARD");
        Self {
            stop,
            handle: Some(handle),
            stalled,
        }
    }

    /// With `Mode::NeverAnswer`: waits until a request is being left unanswered.
    pub fn wait_stalled(&self) {
        self.stalled
            .recv_timeout(TIMEOUT)
            .expect("a request left unanswered");
    }

    /// Waits until the owner thread has ended by itself.
    pub fn join(mut self) {
        if let Some(h) = self.handle.take() {
            h.join().expect("owner thread");
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn atom(c: &RustConnection, name: &str) -> Atom {
    c.intern_atom(false, name.as_bytes())
        .unwrap()
        .reply()
        .unwrap()
        .atom
}

fn window(c: &RustConnection, screen: usize) -> Window {
    let win = c.generate_id().unwrap();
    let aux = CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE);
    let root = c.setup().roots[screen].root;
    c.create_window(
        COPY_DEPTH_FROM_PARENT,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &aux,
    )
    .unwrap();
    win
}

fn notify(c: &RustConnection, r: &SelectionRequestEvent, property: Atom) {
    let ev = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: r.time,
        requestor: r.requestor,
        selection: r.selection,
        target: r.target,
        property,
    };
    c.send_event(false, r.requestor, EventMask::NO_EVENT, ev)
        .unwrap();
    c.flush().unwrap();
}

fn owner_thread(
    display: &str,
    offers: &[(String, Vec<u8>)],
    mode: &Mode,
    stop: &AtomicBool,
    ready: &dyn Fn(),
    stalled: &dyn Fn(),
) {
    let (c, screen) = x11rb::connect(Some(display)).unwrap();
    let win = window(&c, screen);
    let pid = atom(&c, "_NET_WM_PID");
    c.change_property32(
        PropMode::REPLACE,
        win,
        pid,
        AtomEnum::CARDINAL,
        &[std::process::id()],
    )
    .unwrap();
    let (clip, targets, incr) = (atom(&c, "CLIPBOARD"), atom(&c, "TARGETS"), atom(&c, "INCR"));
    let offers: Vec<(Atom, &[u8])> = offers
        .iter()
        .map(|(t, d)| (atom(&c, t), d.as_slice()))
        .collect();
    c.set_selection_owner(win, clip, CURRENT_TIME).unwrap();
    c.flush().unwrap();
    ready();
    // (requestor, property, type, data, offset)
    let mut jobs: Vec<(Window, Atom, Atom, &[u8], usize)> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        let ev = match c.poll_for_event() {
            Ok(Some(ev)) => ev,
            Ok(None) => {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(_) => return,
        };
        match ev {
            Event::SelectionRequest(r) => {
                let prop = if r.property == NONE {
                    r.target
                } else {
                    r.property
                };
                if matches!(mode, Mode::NeverAnswer) && r.target != targets {
                    stalled();
                    continue;
                }
                let offer = offers.iter().find(|(a, _)| *a == r.target);
                let ok = if r.target == targets {
                    let mut list: Vec<Atom> = offers.iter().map(|(a, _)| *a).collect();
                    list.push(targets);
                    c.change_property32(
                        PropMode::REPLACE,
                        r.requestor,
                        prop,
                        AtomEnum::ATOM,
                        &list,
                    )
                    .unwrap();
                    true
                } else if let Some(&(target, data)) = offer {
                    if data.len() > OWNER_INCR_CHUNK {
                        let aux =
                            ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE);
                        c.change_window_attributes(r.requestor, &aux).unwrap();
                        c.change_property32(
                            PropMode::REPLACE,
                            r.requestor,
                            prop,
                            incr,
                            &[data.len() as u32],
                        )
                        .unwrap();
                        jobs.push((r.requestor, prop, target, data, 0));
                    } else {
                        c.change_property8(PropMode::REPLACE, r.requestor, prop, target, data)
                            .unwrap();
                    }
                    true
                } else {
                    false
                };
                notify(&c, &r, if ok { prop } else { NONE });
                if matches!(mode, Mode::VanishAfterTargets) && r.target == targets {
                    return; // drops the connection
                }
            }
            Event::PropertyNotify(p) if p.state == Property::DELETE => {
                if let Some(i) = jobs.iter().position(|j| j.0 == p.window && j.1 == p.atom) {
                    let (req, prop, ty, data, off) = jobs[i];
                    let end = (off + OWNER_INCR_CHUNK).min(data.len());
                    c.change_property8(PropMode::REPLACE, req, prop, ty, &data[off..end])
                        .unwrap();
                    c.flush().unwrap();
                    if off == data.len() {
                        jobs.remove(i); // that was the zero-length end marker
                    } else {
                        jobs[i].4 = end;
                    }
                }
            }
            Event::SelectionClear(_) => return,
            _ => {}
        }
    }
}

/// Reads CLIPBOARD as another app would: `(data, used_incr)`, None if refused.
pub fn read_as_client(display: &str, target: &str) -> Option<(Vec<u8>, bool)> {
    let (c, screen) = x11rb::connect(Some(display)).unwrap();
    let win = window(&c, screen);
    let (clip, target, prop) = (
        atom(&c, "CLIPBOARD"),
        atom(&c, target),
        atom(&c, "TEST_PROP"),
    );
    let incr = atom(&c, "INCR");
    c.convert_selection(win, clip, target, prop, CURRENT_TIME)
        .unwrap();
    c.flush().unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let next = || loop {
        if let Some(ev) = c.poll_for_event().unwrap() {
            return ev;
        }
        assert!(Instant::now() < deadline, "read_as_client timed out");
        thread::sleep(Duration::from_millis(1));
    };
    let property = loop {
        if let Event::SelectionNotify(n) = next() {
            break n.property;
        }
    };
    if property == NONE {
        return None;
    }
    let get = || {
        c.get_property(true, win, prop, AtomEnum::ANY, 0, u32::MAX / 4)
            .unwrap()
            .reply()
            .unwrap()
    };
    let first = get();
    if first.type_ != incr {
        return Some((first.value, false));
    }
    let mut data = Vec::new();
    loop {
        match next() {
            Event::PropertyNotify(p) if p.atom == prop && p.state == Property::NEW_VALUE => {
                let chunk = get();
                if chunk.value.is_empty() {
                    return Some((data, true));
                }
                data.extend_from_slice(&chunk.value);
            }
            _ => {}
        }
    }
}
