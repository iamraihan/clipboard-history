---
name: platform-clipboard
description: Use when editing anything under src-tauri/src/platform/, or changing how clipboard data is read, written, pasted or how shortcuts are registered, on any OS.
---

# Platform clipboard rules

Read first: `ARCHITECTURE.md`, `GLOSSARY.md`, [ADR 0001](../../../docs/adr/0001-gnome-wayland-clipboard-via-xwayland.md). Research doc: [platform APIs](../../../docs/research/clipboard-history-platform-apis.md) (`R` below). Spec: `docs/superpowers/specs/2026-11-12-clipboard-history-design.md` section 4.6.

Decisions stay in `src-tauri/src/core/` (pure, take `now`); backends only do I/O. Linux code lives in `platform/linux/` behind `#[cfg(target_os = "linux")]`. Backends are tested against `platform/fake.rs`; no `unsafe` (`unsafe_code = "forbid"`).

## Linux (implemented)

Files: `linux/mod.rs` (`choose`), `x11.rs` (watch/write), `x11_transfer.rs` (selection I/O, INCR), `x11_paste.rs` (XTest paste, focus), `pointer.rs`, `gnome_keys.rs` (GNOME shortcuts).

**Backend choice**
- X11 session: XFixes `SelectionNotify` on CLIPBOARD (R 1.1). PRIMARY is not recorded.
- GNOME Wayland: Mutter has no `ext-data-control-v1`/`wlr-data-control`, so use the same X11 backend through XWayland (ADR 0001). Needs `DISPLAY`. Never pick a Wayland data-control backend on GNOME. Read the current clipboard once at startup and reconnect (with backoff) when Xwayland dies; reconnecting restarts it.
- KDE/Sway/Hyprland: `Unsupported` until a later plan (data-control, R 1.2).
- Env var names are exact: `XDG_SESSION_TYPE`, `WAYLAND_DISPLAY`, `DISPLAY`, `XDG_CURRENT_DESKTOP`. `clipboard-master` is not used because it checks the misspelled `WAYLAND_DISPALY` (spec 4.6). `choose_from_env` has a test that pins the names.

**Reading** (R 1.1: the owner's data dies with its process, so read at once on each owner change)
- Large selections arrive by ICCCM INCR; implement it for both reading and serving. Chunk size is `min(maximum_request_bytes, 65535 * 4) - overhead` (the core request size cap). Tests: `reads_png_larger_than_max_request_via_incr`, `own_large_write_is_served_via_incr`.
- Per-target size cap: `Limits::default().max_item_bytes` (10 MB) plus a margin. Over the cap, stop reading and abandon the INCR transfer cleanly (`abandon_incr`). R 1.1 says few apps honor `_NET_MAX_SELECTION_SIZE`, so enforce your own cap.
- Secrets: target `x-kde-passwordManagerHint` equal to `secret` marks the ClipEvent secret and its formats are dropped (R 3, R 7; Klipper/GPaste/arboard convention). Test: `password_hint_sets_secret_flag`.
- Stored formats: `text/plain;charset=utf-8` (else `UTF8_STRING`), `text/html`, `image/png`, `text/uri-list` (R 3). Mutter re-serves only one saved type after the owner exits, so read every wanted format at copy time (ADR 0001).
- A paste-time write must interrupt an in-flight read (the read returns `Interrupted`, no event). Do not let a slow source block our own write.

**Echo (own writes)**
- Our own write produces an Echo that must only bump the existing Clip. `core::rules::echo_target(own, hash, now)` matches by content hash within `ECHO_WINDOW_SECS = 2`, and the record is one-shot (taken on first match). Do not suppress by "ignore events for N ms".

**Pasting** (R 2: XTest fake key events)
- Never inject keys unless the active window (`_NET_ACTIVE_WINDOW`, else input focus) equals the paste target. Check right before the key events, after keycode lookups, not earlier. Use a recorded target once (`take()`).
- Always release modifiers, also on error paths. Wayland (GNOME): no injection; copy only, and the UI says "press Ctrl+V".
- Terminals need Ctrl+Shift+V: `X11Paster` reads the target's `WM_CLASS` (instance and class) and `paste_chord_for(class)` picks the chord (known terminal list, case-insensitive: gnome-terminal-server, kgx, org.gnome.console, konsole, xterm, uxterm, alacritty, kitty, wezterm, tilix, terminator, xfce4-terminal, urxvt, foot, ptyxis). Ctrl+Shift+V presses Control_L, Shift_L, v and releases in reverse; the active-window check stays right before the first key event. Add new terminals to that list, with a test.

**GNOME shortcuts** (`gnome_keys.rs`, `shortcut.rs`)
- Force Super+V removes Super+V from `org.gnome.shell.keybindings` and the `wm`/`mutter` keybinding schemas and adds our custom keybinding under `.../custom-keybindings/clipboard-history/` that runs `<exe> --toggle`. Reset restores the exact originals.
- Save the originals to disk (settings `saved_bindings`) before the first write; if persisting fails, write nothing.
- A failed `gsettings get` is an error, never an empty list: never write back a list built from a failed read.
- One shortcut operation at a time: hold `AppState.shortcut_op` (`with_settings_state`) for first run, reset, quit and startup.
- Restore leftovers at startup when originals are saved but Force is off (`StartupPlan::Restore`).
- macOS gets no registrar until Plan 3 (`kind_for_os`): Cmd+V is never taken. "Not now" in the first-run dialog is recoverable via the tray item "Use Super+V…" (`offers_use_super_v`). The first-run command returns only conflicts worth telling (`conflict_notice`); the dialog closes the window via `hide_settings`.
- Wayland GNOME 48+ could use the GlobalShortcuts portal (R 2); GNOME 46/47 use the custom keybinding plus single-instance `--toggle`.
- Tests never write real gsettings: use `GSETTINGS_BACKEND=memory` (`SystemGSettings::with_env`) or the fake. Tests never inject keys into a real display; X11 tests use their own Xvfb. Real-session checks go in the `desktop-test` skill.

**Keyring and database** (`store/key.rs`, `store/mod.rs`)
- A key is only created together with a brand-new DB (`allow_create`). An existing DB with no key is an error, never "make a new key".
- Recovery (moving the DB aside as `history.db.broken-*`) only on `NotADatabase`/`DatabaseCorrupt`. Other errors (locked keyring, IO) must not touch the file.
- Fallback key file `history.key` is written 0600 in a 0700 dir only when no Secret Service exists and creation is allowed.

## macOS (Plan 3, not implemented yet)

Follow R 1.3 and 3: poll `NSPasteboard.changeCount` every 500 ms on a background thread; skip `org.nspasteboard.TransientType`, `ConcealedType`, `AutoGeneratedType`; `org.nspasteboard.source` is display only; frontmost app at change time is the source (approximation); paste via `CGEvent` needs Accessibility; macOS 15.4+ pasteboard privacy alerts (read `accessBehavior`, use `detectedPatterns` which does not read). Default shortcut Ctrl+Cmd+V.

## Windows (Plan 4, not implemented yet)

Follow R 1.4 and 3: hidden message-only window with `AddClipboardFormatListener` (never poll `GetClipboardSequenceNumber`); skip on `ExcludeClipboardContentFromMonitorProcessing`, `CanIncludeInClipboardHistory == 0` (and Ditto's legacy `Clipboard Viewer Ignore`); read only formats we store (delayed rendering); `HTML Format` offsets, `CF_DIB` to PNG; `SendInput` is subject to UIPI; Win+V is OS-reserved (Force takes it).
