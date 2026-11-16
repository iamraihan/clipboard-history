/**
 * Typed wrappers around the Tauri IPC commands in `src-tauri/src/commands.rs`.
 *
 * `Commands` is the one table of command name -> argument object -> result.
 * Argument keys must match the Rust parameter names in camelCase; no tool
 * checks that, so verify by hand when a command changes. Payload and result
 * types come from the generated bindings.
 */
import { invoke } from "@tauri-apps/api/core";
import type { ClipSummary, ForceReport, PasteOutcome, QuitChoice, SettingsView, StartupReport } from "./types";

type NoArgs = Record<string, never>;

interface Commands {
  list_clips: [{ query: string | null }, ClipSummary[]];
  set_pinned: [{ id: number; pinned: boolean }, void];
  delete_clip: [{ id: number }, void];
  clear_unpinned: [NoArgs, void];
  paste_clip: [{ id: number; plain: boolean }, PasteOutcome];
  startup_report: [NoArgs, StartupReport];
  hide_popup: [NoArgs, void];
  settings_view: [NoArgs, SettingsView];
  /** Yes returns what else lost Super+V (readable names; GNOME's notification list is left out), or null when there is nothing to tell; No returns null. */
  first_run_choice: [{ useSuperV: boolean }, ForceReport | null];
  hide_settings: [NoArgs, void];
  reset_shortcuts: [NoArgs, void];
  quit_choice: [{ choice: QuitChoice }, void];
}

function call<K extends keyof Commands>(cmd: K, args: Commands[K][0]): Promise<Commands[K][1]> {
  return invoke<Commands[K][1]>(cmd, args);
}

export const ipc = {
  listClips: (query: string | null) => call("list_clips", { query }),
  setPinned: (id: number, pinned: boolean) => call("set_pinned", { id, pinned }),
  deleteClip: (id: number) => call("delete_clip", { id }),
  clearUnpinned: () => call("clear_unpinned", {}),
  pasteClip: (id: number, plain: boolean) => call("paste_clip", { id, plain }),
  startupReport: () => call("startup_report", {}),
  hidePopup: () => call("hide_popup", {}),
  settingsView: () => call("settings_view", {}),
  firstRunChoice: (useSuperV: boolean) => call("first_run_choice", { useSuperV }),
  hideSettings: () => call("hide_settings", {}),
  resetShortcuts: () => call("reset_shortcuts", {}),
  quitChoice: (choice: QuitChoice) => call("quit_choice", { choice }),
};

/** Tauri rejects with the Rust `String` error. */
export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : "Something went wrong";
}
