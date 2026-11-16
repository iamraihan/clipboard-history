# Clipboard History

[![CI](https://github.com/iamraihan/clipboard-history/actions/workflows/ci.yml/badge.svg)](https://github.com/iamraihan/clipboard-history/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A cross-platform clipboard history app built with Tauri 2. Ubuntu/Linux first; macOS and Windows to follow. Clipboard data stays on the machine.

## Linux MVP status

Works (automated tests pass): clipboard watching on GNOME X11 and on GNOME Wayland through XWayland, encrypted history (SQLite plus key in the keyring), popup with search, pin, delete and clear, Super+V shortcut (optionally taken from GNOME and restorable from the tray), paste-back on X11, copy-only on Wayland.

Pending: the manual desktop checklist on real GNOME X11 and Wayland sessions (`.claude/skills/desktop-test/SKILL.md`), the real-login confirmation in `docs/adr/0001-gnome-wayland-clipboard-via-xwayland.md`, and Plans 2 to 5 (settings UI, KDE/Sway, macOS, Windows, release). Run it with `pnpm tauri dev`.

## Development

Requires Rust, Node 22 and pnpm. On Ubuntu, install the Tauri build dependencies first (see `.github/workflows/ci.yml`).

```sh
pnpm install
pnpm tauri dev      # run the app
pnpm build          # type-check and build the frontend
cd src-tauri && cargo test
```

See `ARCHITECTURE.md` for the design and `CLAUDE.md` for contributor conventions.

## License

MIT, see [LICENSE](LICENSE).
