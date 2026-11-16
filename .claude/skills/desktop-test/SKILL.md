---
name: desktop-test
description: Use when running, testing or verifying the app on a desktop, or preparing a release; includes the manual checklist for GNOME X11 and Wayland.
---

# Desktop test

Related: [ADR 0001](../../../docs/adr/0001-gnome-wayland-clipboard-via-xwayland.md), skill `platform-clipboard`.

## Run
- App: `pnpm install --frozen-lockfile && pnpm tauri dev` (no env override). Second launch with `--toggle` toggles the popup.
- Automated: `cd src-tauri && cargo test --locked`, `cargo clippy --all-targets -- -D warnings`; `pnpm build`; from repo root `cargo deny --manifest-path src-tauri/Cargo.toml check` and `pnpm audit --prod`. X11 tests start their own Xvfb (needs the `xvfb` package); CI uses `xvfb-run -a`. Tests never touch your real gsettings or display.
- Switch GNOME session: log out, click the gear on the login screen, choose "Ubuntu" (Wayland) or "Ubuntu on Xorg", log in. Check `echo $XDG_SESSION_TYPE`.
- Data dir: `~/.local/share/com.clipboardhistory.app/` (Tauri `app_data_dir`): `history.db`, `settings.json`, `history.key` (only if no keyring).

## Before any shortcut test: record GNOME state
```
gsettings get org.gnome.shell.keybindings toggle-message-tray     # expect ['<Super>v', '<Super>m']
gsettings get org.gnome.settings-daemon.plugins.media-keys custom-keybindings
```
Manual restore if anything goes wrong:
```
gsettings set org.gnome.shell.keybindings toggle-message-tray "['<Super>v', '<Super>m']"
gsettings reset-recursively org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/clipboard-history/
gsettings set org.gnome.settings-daemon.plugins.media-keys custom-keybindings "<value recorded above>"   # or gsettings reset ... custom-keybindings if it was @as []
```

## Manual checklist (run on GNOME X11, then on GNOME Wayland)
Copy and history
1. Copy text, rich HTML (Firefox paragraph), an image (Image Viewer "Copy Image"), a file (Files): each appears with the right icon/preview.
2. Copy the same text again: no new row, the existing one moves to the top.
3. Pin an item, use footer "Clear" (click twice within 3 s) or Ctrl+Shift+Delete twice: the pin survives; the label resets after 3 s; holding the keys arms once but does not clear; Esc/blur/reopen disarms.
4. Copy a password in KeePassXC: skipped.
5. Copy an 11 MB image: skipped.
6. Quit and restart: history is still there. `sqlite3 ~/.local/share/com.clipboardhistory.app/history.db .tables` fails (file is not a database).
Popup
7. Delete/pin a middle row keeps the selection near it; a new copy while open keeps the selected clip; typing resets to row 0; Down/Enter works after clicking Clear.
Shortcut (record gsettings state first)
8. First run shows "Use Super+V for clipboard history?". Click "Use Super+V". Then `toggle-message-tray` prints `['<Super>m']`; `settings.json` has `forceSuperV: true`, `firstRunDone: true`, `savedBindings` with the originals; the `.../clipboard-history/` custom keybinding `command` ends with `--toggle` and `binding` is `'<Super>v'`.
9. Super+V opens the popup; again closes it.
10. Paste. X11: focus gedit, Super+V, Enter inserts the item; with rich HTML, Shift+Enter inserts plain text; with auto_paste off it is only copied. In gnome-terminal (X11 session) Enter pastes with Ctrl+Shift+V. Wayland: Enter copies and closes the popup, the footer says "Pick an item, then press Ctrl+V", Ctrl+V pastes.
11. Tray "Reset shortcuts to default": `toggle-message-tray` is `['<Super>v', '<Super>m']` again, the custom list no longer has `clipboard-history/`.
11b. After "Not now" the tray status says "Shortcut not set: choose “Use Super+V…”" and the menu has "Use Super+V…", which reopens the first-run dialog. If another binding also had Super+V, the dialog then says "Super+V was also removed from: …" with OK.
12. To retry first run: quit, set `firstRunDone` to false in `settings.json`, restart. With Force on, tray "Quit" asks "Give Super+V back to GNOME before quitting?" (three buttons).
12b. Start at login (release build: the `.deb` or AppImage). First start creates `~/.config/autostart/Clipboard History.desktop` (exact name from `ls ~/.config/autostart`) with `Exec=... --autostart`; the tray shows "Start at login" checked and `settings.json` has `autostartDefaultApplied: true`. Log out and in: the app is in the tray, no popup. Uncheck it: the entry is gone and stays gone after a restart. `pnpm tauri dev` never creates the entry by itself.
13. Wayland only: ADR 0001 "Still to confirm" (real login, native apps, on-demand Xwayland).

## Reset everything
Tray "Reset shortcuts to default" first, quit the app, then:
```
rm -rf ~/.local/share/com.clipboardhistory.app
secret-tool clear service com.clipboardhistory.app username history-key
```
(attributes are `service` and `username`, as set by keyring 3.6.3's secret-service store). Deleting the data dir without the keyring entry is fine; deleting the entry while keeping the DB makes the DB unreadable.
