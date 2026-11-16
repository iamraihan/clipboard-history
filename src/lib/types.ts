/**
 * Frontend view of the IPC types. The shapes themselves are generated from
 * the Rust types into `./bindings/` by `cargo test` (ts-rs); change the Rust
 * type, rerun `cargo test`, and these follow.
 */
export type { ClipKind } from "./bindings/ClipKind";
export type { ClipSummary } from "./bindings/ClipSummary";
export type { Capabilities } from "./bindings/Capabilities";
export type { Limits } from "./bindings/Limits";
export type { Settings } from "./bindings/Settings";
export type { SavedBinding } from "./bindings/SavedBinding";
export type { StartupReport } from "./bindings/StartupReport";
export type { PasteOutcome } from "./bindings/PasteOutcome";
export type { ForceReport } from "./bindings/ForceReport";
export type { QuitChoice } from "./bindings/QuitChoice";
export type { SettingsView } from "./bindings/SettingsView";
