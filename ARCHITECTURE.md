# Architecture

One Tauri 2 process. Rust owns all clipboard history logic; the React/TS UI renders what Rust sends over IPC.

```
┌──────────────── Tauri 2 app (one process) ──────────────┐
│  React/TS UI: popup window + settings window            │
│        ▲ IPC; Rust types exported with ts-rs            │
│  Rust core: history store, rules, search, settings      │
│        ▲ ClipEvent                                      │
│  Platform layer: ClipboardBackend, Paster,              │
│                  ShortcutRegistrar, FocusTracker        │
└─────────────────────────────────────────────────────────┘
      ▲ session D-Bus (GNOME Wayland only, only if the spike fails)
 [GNOME Shell companion extension]
```

Same conventions as `prayer-times`: Rust is the single source of truth for IPC types (`ts-rs`, `#[serde(rename_all = "camelCase")]`, enums for closed sets); decision logic lives in pure functions that take inputs as parameters; side effects stay in a thin engine layer.

## Module map (`src-tauri/src/`)

| Module | Role |
|---|---|
| `core` | Clip model, rules (duplicates, skip, retention, echo), search, settings, update tray line. Pure, no side effects. |
| `store` | SQLite history storage and encryption key handling. |
| `platform` | Backend traits and per-OS implementations; Linux-only code under `platform/linux/` behind `#[cfg(target_os = "linux")]`. |
| `engine` | Receives ClipEvents, applies `core` decisions, runs side effects (store, clipboard, windows, tray). |
| `commands` | Tauri commands the frontend calls. |
| `tray` | Tray icon and menu. |
| `updater` | Update check against GitHub Releases (`latest.json`) on launch, daily and from the tray's "Check for updates", the app's only network access; offers the update as a tray line. Release builds only. See `RELEASING.md`. |
| `popup` | Popup window lifecycle: show, position, take focus, hide on blur. |
| `autostart` | Start at login (`tauri-plugin-autostart`): on by default once, toggled from the tray; the login entry passes `--autostart`, so the app starts in the tray. |

Frontend (`src/`): one page serves both windows; `App.tsx` picks the view from the window label (`popup` or `settings`). Both windows are created hidden at startup.

Decisions that look odd are recorded in `docs/adr/`.
