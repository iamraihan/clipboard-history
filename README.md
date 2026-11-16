# Clipboard History

[![CI](https://github.com/iamraihan/clipboard-history/actions/workflows/ci.yml/badge.svg)](https://github.com/iamraihan/clipboard-history/actions/workflows/ci.yml)
[![Release](https://github.com/iamraihan/clipboard-history/actions/workflows/release.yml/badge.svg)](https://github.com/iamraihan/clipboard-history/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Downloads](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fiamraihan%2Fclipboard-history%2Fbadges%2Fdownloads.json)](https://github.com/iamraihan/clipboard-history/releases)

A cross-platform clipboard history app built with Tauri 2. Ubuntu/Linux first; macOS and Windows to follow. Clipboard data stays on the machine.

## Install

Grab the latest installer from the [Releases page](https://github.com/iamraihan/clipboard-history/releases/latest):

| OS           | File to download |
| ------------ | ---------------- |
| Ubuntu/Linux | the `.AppImage` (supports in-app updates), or `.deb` / `.rpm` |
| Windows      | `Clipboard History_x.y.z_x64-setup.exe` or the `.msi` (Backend not ready yet) |
| macOS        | `_aarch64.dmg` (Apple Silicon) or `_x64.dmg` (Intel) (Backend not ready yet) |

## Updates

The app checks GitHub Releases for a newer version on launch and once a day; that check is its only network access. When an update exists, the tray menu shows **Update to vX.Y.Z and restart**. On macOS it opens the release page instead.

## Linux MVP status

Works (automated tests pass): clipboard watching on GNOME X11 and on GNOME Wayland through XWayland, encrypted history (SQLite plus key in the keyring), popup with search, pin, delete and clear, Super+V shortcut (optionally taken from GNOME and restorable from the tray), paste-back on X11, copy-only on Wayland.

Pending: the manual desktop checklist on real GNOME X11 and Wayland sessions (`.claude/skills/desktop-test/SKILL.md`), the real-login confirmation in `docs/adr/0001-gnome-wayland-clipboard-via-xwayland.md`, and the next plans (settings UI, KDE/Sway, macOS, Windows). Run it with `pnpm tauri dev`.

## Development

Requires Rust, Node 22 and pnpm. On Ubuntu, install the Tauri build dependencies first (see `.github/workflows/ci.yml`).

```sh
pnpm install
pnpm tauri dev      # run the app
pnpm build          # type-check and build the frontend
cd src-tauri && cargo test
```

See `ARCHITECTURE.md` for the design, `CONTRIBUTING.md` and `CLAUDE.md` for contributor conventions, and `RELEASING.md` for how releases are published.

## License

MIT, see [LICENSE](LICENSE).
