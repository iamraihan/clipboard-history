# Clipboard History

Tauri 2 desktop app: a Rust backend watches the clipboard and owns history, rules and settings; a React/TS frontend renders the popup and settings windows. Ubuntu ships first; macOS and Windows follow.

Before changing the engine or a platform backend, read `ARCHITECTURE.md` and `GLOSSARY.md`, and every ADR in `docs/adr/`. Use the glossary terms (Clip, Format, ClipEvent, Backend, Capabilities, Echo...) in code, comments and docs. An ADR explains a decision that looks odd; read the relevant one before "fixing" it.

## Git

The maintainer commits and pushes. Leave changes uncommitted and unstaged; report what changed and propose a commit message.

## Type safety

Rust is the single source of truth for every shape that crosses IPC.

- **Changing an IPC type:** edit the Rust struct or enum (it derives `ts_rs::TS` with `#[ts(export)]`), then run `cargo test` in `src-tauri/`. That regenerates `src/lib/bindings/`; the maintainer commits those files with the change. CI fails when they are stale. `src/lib/bindings/` is read-only output.
- **New IPC type:** derive `Serialize`/`Deserialize` + `TS`, add `#[serde(rename_all = "camelCase")]` and `#[ts(export)]`, and re-export it from `src/lib/types.ts` under its frontend name.
- **New or changed command:** add it to the `Commands` table in `src/lib/ipc.ts`. The argument object's keys must match the Rust parameter names in camelCase; no tool checks that, so verify it by hand.
- **A closed set of values** (a clip kind, a backend, a format) is an enum on both sides, never a free `String`/`u8`.
- **Persisted types** (settings, anything on disk) must load files written by older versions: give new fields a default (`#[serde(default)]`) and make enum deserializers fall back to a default instead of failing.
- **TypeScript runs with the strictest flags** (`noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`, ...). Handle the `undefined` they surface; make optional props `prop?: T | undefined`.

## Testing

- Decisions (duplicates, skip rules, retention, echo detection) live in `src-tauri/src/core/` as pure functions that take their inputs, including `now`, as parameters, unit-tested in the same file. `engine` only applies decisions and runs side effects (storage, clipboard, windows, tray).
- Every bug fix starts with a failing test that reproduces it.
- Platform code is tested against a fake Backend. Tests that need a real compositor or desktop session are `#[ignore = "display"]`. The X11 Backend tests start their own Xvfb (install the `xvfb` package) and run in plain `cargo test`. CI runs the suite under `xvfb-run` on Linux.
- Before finishing: `cargo test` (in `src-tauri/`) and `pnpm build` (runs `tsc`) both pass, and `src/lib/bindings/` is regenerated.

## Platform code

Linux-only code lives under `src-tauri/src/platform/linux/` behind `#[cfg(target_os = "linux")]`. CI builds Ubuntu, Windows and macOS, so everything outside that directory compiles on all three.

## Dependencies

Dependencies are an attack surface; each one needs a reason.

- Add a crate or package only when the plan names it, from its official repository. Prefer the standard library or a few lines of our own code over a small crate.
- Pick the latest version at least 2 weeks old. Install with `pnpm install --frozen-lockfile`; commit `Cargo.lock` and `pnpm-lock.yaml`.
- pnpm install scripts stay blocked; list a package in `onlyBuiltDependencies` only when it needs one.
- `cargo deny check` and `pnpm audit --prod` pass; CI enforces both.
- No network access except the update check to GitHub Releases. Clipboard data never leaves the machine.
