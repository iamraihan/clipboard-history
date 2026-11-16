# On GNOME Wayland the app watches the clipboard through XWayland, not through a Shell extension

Status: Accepted, pending real-login confirmation. Tested **headless** on GNOME 46 only; see "Still to confirm".

Mutter has no `ext-data-control-v1` or `wlr-data-control`, so a clipboard manager on GNOME Wayland needs either a Shell extension or an X11 client that Mutter mirrors selections to. Mutter claims every Wayland selection on its own Xwayland window, so a windowless X11 client should see an XFixes owner change for each copy made in a native Wayland app. This ADR records the test of that, and the decision it drives (`GNOME_BACKEND = XWayland`, read by Tasks 7 and 8).

## Test

The maintainer's login is X11, so the test ran in a private headless GNOME Shell 46.0 / mutter 46.2 on Ubuntu 24.04, with its own D-Bus session and its own Wayland display, so the real session was untouched:

`dbus-run-session -- gnome-shell --headless --wayland --virtual-monitor 1280x720 --wayland-display wayland-spike`

The probe is a windowless `x11rb` 0.14.0 client (an unmapped input-only window). It calls `xfixes_select_selection_input` on `CLIPBOARD` with `SET_SELECTION_OWNER`. On each event it prints the timestamp, `TARGETS`, and the byte length of `UTF8_STRING`, `text/html`, `image/png` and `text/uri-list`. Copies came from native Wayland `wl-copy` (wl-clipboard 2.2.1) on `WAYLAND_DISPLAY=wayland-spike`. Source: see the appendix.

Two things the headless setup needed:

- **No keyboard.** `wl-copy` first failed with "This seat has no keyboard". A virtual keyboard created through `org.gnome.Mutter.RemoteDesktop` on the private bus fixed that (test-only; a real login has a keyboard). After it, Mutter accepted `wl-copy` selections, so focus did not block the test.
- **No systemd user manager.** Mutter's X11 setup calls `org.freedesktop.systemd1` `StartUnit('gnome-session-x11-services-ready.target')`. Without an answer, mutter logged "Failed to initialize X11 display" and the X11 side hung. A throwaway stand-in service on the private bus answered it (test harness, not kept). A real login has gnome-session and systemd, so this is a test artefact only.

## Results

| Row | Copy source | Event received | Target seen over XWayland | Result |
|---|---|---|---|---|
| Text | `wl-copy` | yes | `UTF8_STRING` 11 bytes (also STRING, TEXT, text/plain) | pass (synthetic wl-copy source) |
| HTML | `wl-copy --type text/html` | yes | `text/html` 10 bytes (wl-copy also adds `UTF8_STRING`) | pass (synthetic wl-copy source) |
| Image | `wl-copy --type image/png` (73-byte PNG) | yes | `image/png` 73 bytes (`TARGETS` is only `image/png`) | pass (synthetic wl-copy source) |
| Files | `wl-copy --type text/uri-list` | yes | `text/uri-list` 22 bytes (also `UTF8_STRING`) | pass (synthetic wl-copy source) |
| Large text | `wl-copy`, 3 MB | yes | `UTF8_STRING` 3000000 bytes, no INCR | pass (synthetic wl-copy source) |
| Xwayland not running at first | probe connected while no Xwayland process existed | yes | the connection started Xwayland on demand, then the next copy arrived (14 bytes) | pass (synthetic wl-copy source) |

Other observations:

- Before the first X client connects, Mutter only holds the `/tmp/.X11-unix/X1` socket; there is no Xwayland process. The probe's own connect starts it.
- A copy made while Xwayland was stopped and the probe was not running produced no event once the probe attached. The app must read the current clipboard once at startup.
- When Xwayland was killed under a running probe, the probe's connection died (`UnexpectedEof`). The app must reconnect, and reconnecting restarts Xwayland.
- In the headless run, X clients needed `XAUTHORITY=/run/user/1000/.mutter-Xwaylandauth.*`. A real session exports it.

## Considered Options

- **GNOME Shell extension (listening to Mutter's `MetaSelection` `owner-changed` from inside gnome-shell).** Not chosen for now: it is a second artifact to ship, review on extensions.gnome.org and keep compatible with each GNOME release, and the XWayland route needs none of that.
- **RemoteDesktop portal clipboard.** Rejected: it needs a consent dialog and probably a persistent remote-control indicator, which is a poor fit for an always-on app.
- **XWayland XFixes listener (chosen).** Works without an extension, with every tested format.

## Consequences

- Tasks 7 and 8 implement the X11 backend and also use it on GNOME Wayland. Task 8 does not need to return `Unsupported` for GNOME Wayland, and Plan 2 gains no extension task.
- The app needs Xwayland and a reachable `DISPLAY`. It must not pick a Wayland data-control backend on GNOME.
- From research, not measured here ([research doc, section 1.2](../research/clipboard-history-platform-apis.md), the "built-in clipboard manager" bullet): when the CLIPBOARD owner exits, Mutter re-serves only one saved MIME type (best image, else text; caps 4 MB text, 200 MB image), so rich formats such as HTML may be lost after the source app quits. The app should therefore read the formats it wants at copy time.
- The X11 backend must still implement INCR for other sources, even though the 3 MB text copy here did not trigger it.
- If the real-login check fails on any row, change this ADR to `Extension` and add the extension task before Plan 3.

## Still to confirm

Not tested: native GUI apps and a real Ubuntu Wayland login. Do this once in a real "Ubuntu" (Wayland) login.

1. `echo $XDG_SESSION_TYPE` must print `wayland`.
2. Copy the appendix files into `~/xwayland-probe/` (`Cargo.toml` and `src/main.rs`). The first `cargo run` downloads crates (needs network).
3. Close all X11 apps, then run `pgrep -a Xwayland`. On Mutter 46 it normally prints nothing yet. Start the probe with `cd ~/xwayland-probe && cargo run`; its own connection starts Xwayland, and `pgrep -a Xwayland` now lists it. Then make the first copy below. This is the on-demand check.
4. In Firefox open `about:support` and confirm Window Protocol = `wayland` (if it says `x11`, relaunch with `MOZ_ENABLE_WAYLAND=1`); an X11 Firefox would not test the bridge.
5. Copy from each app and expect an `owner-change` block with these targets:

| App and action | Expected |
|---|---|
| GNOME Text Editor, copy text | `UTF8_STRING` with a length |
| Firefox, copy a formatted paragraph | `text/html` (and `UTF8_STRING`) |
| Image Viewer, "Copy Image" | `image/png` |
| Files, copy a file | `text/uri-list` (or `x-special/gnome-copied-files`) |

If every row arrives, the ADR stands. If any is missing, change the decision to `Extension` and add the extension task before Plan 3.

## Appendix: probe source

`~/xwayland-probe/Cargo.toml`

```toml
[package]
name = "xwayland-probe"
version = "0.0.0"
edition = "2021"

[dependencies]
x11rb = { version = "=0.14.0", features = ["xfixes"] }
```

`~/xwayland-probe/src/main.rs`

```rust
use std::collections::VecDeque;
use std::io::Write;
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::COPY_DEPTH_FROM_PARENT;

fn atom(c: &impl Connection, n: &str) -> Atom {
    c.intern_atom(false, n.as_bytes()).unwrap().reply().unwrap().atom
}

fn main() {
    let (c, scr) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[scr].root;
    let win = c.generate_id().unwrap();
    c.create_window(COPY_DEPTH_FROM_PARENT, win, root, 0, 0, 1, 1, 0,
        WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new()).unwrap(); // never mapped
    c.xfixes_query_version(5, 0).unwrap().reply().unwrap();
    let clip = atom(&c, "CLIPBOARD");
    c.xfixes_select_selection_input(win, clip, SelectionEventMask::SET_SELECTION_OWNER).unwrap();
    let prop = atom(&c, "PROBE_PROP");
    let incr = atom(&c, "INCR");
    let targets = atom(&c, "TARGETS");
    c.flush().unwrap();
    println!("listening on CLIPBOARD (DISPLAY={:?})", std::env::var("DISPLAY"));
    std::io::stdout().flush().unwrap();
    let mut pending: VecDeque<Event> = VecDeque::new();
    loop {
        let ev = match pending.pop_front() { Some(e) => e, None => c.wait_for_event().unwrap() };
        let Event::XfixesSelectionNotify(e) = ev else { continue };
        if e.selection != clip { continue; }
        println!("--- owner-change ts={} owner={}", e.timestamp, e.owner);
        if e.owner == x11rb::NONE { println!("  (owner none)"); continue; }
        let mut ask = |t: Atom| -> String {
            c.convert_selection(win, clip, t, prop, e.timestamp).unwrap();
            c.flush().unwrap();
            loop {
                match c.wait_for_event().unwrap() {
                    Event::SelectionNotify(n) if n.requestor == win => {
                        if n.property == x11rb::NONE { return "refused".into(); }
                        let r = c.get_property(true, win, prop, AtomEnum::ANY, 0, 0x1000000)
                            .unwrap().reply().unwrap();
                        if r.type_ == incr { return "INCR (not read)".into(); }
                        if t == targets {
                            let v: Vec<String> = r.value32().into_iter().flatten().map(|a| {
                                String::from_utf8(c.get_atom_name(a).unwrap().reply().unwrap().name).unwrap()
                            }).collect();
                            return v.join(" ");
                        }
                        return format!("{} bytes", r.value.len());
                    }
                    other => pending.push_back(other),
                }
            }
        };
        println!("  TARGETS: {}", ask(targets));
        for n in ["UTF8_STRING", "text/html", "image/png", "text/uri-list"] {
            let a = atom(&c, n);
            println!("  {n}: {}", ask(a));
        }
        std::io::stdout().flush().unwrap();
    }
}
```
