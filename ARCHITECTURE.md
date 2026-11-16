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
| `core` | Clip model, rules (duplicates, skip, retention, echo), search, settings. Pure, no side effects. |
| `store` | SQLite history storage and encryption key handling. |
| `platform` | Backend traits and per-OS implementations; Linux-only code under `platform/linux/` behind `#[cfg(target_os = "linux")]`. |
| `engine` | Receives ClipEvents, applies `core` decisions, runs side effects (store, clipboard, windows, tray). |
| `commands` | Tauri commands the frontend calls. |
| `tray` | Tray icon and menu. |
| `popup` | Popup window lifecycle: show, position, hide on blur. |

Frontend (`src/`): one page serves both windows; `App.tsx` picks the view from the window label (`popup` or `settings`). Both windows are created hidden at startup.

Decisions that look odd are recorded in `docs/adr/`.
