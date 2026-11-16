import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import { errorText, ipc } from "../lib/ipc";
import type { ClipSummary, StartupReport } from "../lib/types";
import { ClipRow } from "./ClipRow";
import { nextSelection } from "./selection";
import { useKeyboard } from "./useKeyboard";

const SEARCH_DEBOUNCE_MS = 80;
const MODIFIER_KEYS = new Set(["Control", "Shift", "Alt", "Meta"]);
const CLEAR_CONFIRM_MS = 3000;
const NO_PLAIN_TEXT = "This item has no plain text";
const KEYS_HINT = "Enter paste · Shift+Enter plain · Ctrl+P pin · Del delete · Esc close";
// Where the app cannot paste for you (Wayland), picking only copies.
const COPY_ONLY_HINT = "Pick an item, then press Ctrl+V · Ctrl+P pin · Del delete · Esc close";

interface Banner {
  text: string;
  role: "alert" | "status";
}

// storeError already carries its own prefix ("History cannot be opened: ...").
function banners(r: StartupReport | null): Banner[] {
  if (!r) return [];
  const out: Banner[] = [];
  if (r.backendError) out.push({ text: `Clipboard watching is off: ${r.backendError}`, role: "alert" });
  if (r.storeError) out.push({ text: r.storeError, role: "alert" });
  if (r.keyUnavailable) out.push({ text: "History locked: waiting for the keyring…", role: "status" });
  if (r.recoveredBroken)
    out.push({ text: "Your previous history couldn't be opened and was set aside; a new history was started.", role: "status" });
  if (r.keyInFile) out.push({ text: "History is protected by file permissions only (no keyring found).", role: "status" });
  return out;
}

export default function Popup() {
  const [query, setQuery] = useState("");
  const [clips, setClips] = useState<ClipSummary[]>([]);
  const [selected, setSelected] = useState(0);
  const [report, setReport] = useState<StartupReport | null>(null);
  const [hint, setHint] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const clearTimer = useRef<number | undefined>(undefined);
  const selectedIdRef = useRef<number | undefined>(undefined);
  const selectedRef = useRef(0);
  const queryRef = useRef(query);
  const requestRef = useRef(0);
  // A reset (new query, popup opened) stays owed until a response is applied,
  // even if a later non-reset refresh supersedes the request that asked for it.
  const pendingResetRef = useRef(false);
  queryRef.current = query;

  const disarmClear = useCallback(() => {
    window.clearTimeout(clearTimer.current);
    setConfirmClear(false);
  }, []);

  const refresh = useCallback(async (resetSelection: boolean) => {
    if (resetSelection) pendingResetRef.current = true;
    const mine = ++requestRef.current;
    try {
      const q = queryRef.current.trim();
      const rows = await ipc.listClips(q === "" ? null : q);
      if (mine !== requestRef.current) return;
      const next = nextSelection(
        selectedIdRef.current,
        selectedRef.current,
        rows.map((r) => r.id),
        pendingResetRef.current,
      );
      pendingResetRef.current = false;
      setClips(rows);
      setSelected(next);
    } catch (e) {
      if (mine === requestRef.current) setHint(errorText(e));
    }
  }, []);

  useEffect(() => {
    selectedRef.current = selected;
    selectedIdRef.current = clips[selected]?.id;
  }, [clips, selected]);

  const refreshReport = useCallback(() => {
    ipc.startupReport().then(setReport, () => undefined);
  }, []);

  // Search, debounced; the first run loads the list.
  useEffect(() => {
    const t = window.setTimeout(() => void refresh(true), SEARCH_DEBOUNCE_MS);
    return () => window.clearTimeout(t);
  }, [query, refresh]);

  useEffect(() => {
    refreshReport();
    const subs = [
      listen("history-changed", () => {
        void refresh(false);
        refreshReport();
      }),
      listen("popup-opened", () => {
        setQuery("");
        queryRef.current = "";
        setHint(null);
        disarmClear();
        void refresh(true);
        refreshReport();
        inputRef.current?.focus();
      }),
    ];
    return () => {
      for (const s of subs) void s.then((off) => off());
    };
  }, [refresh, refreshReport, disarmClear]);

  useEffect(() => {
    window.addEventListener("blur", disarmClear);
    return () => {
      window.removeEventListener("blur", disarmClear);
      window.clearTimeout(clearTimer.current);
    };
  }, [disarmClear]);

  useEffect(() => {
    const id = clips[selected]?.id;
    if (id !== undefined) document.getElementById(`clip-${id}`)?.scrollIntoView({ block: "nearest" });
  }, [clips, selected]);

  const run = (p: Promise<unknown>): void => {
    p.catch((e: unknown) => setHint(errorText(e)));
  };

  const paste = (clip: ClipSummary | undefined, plain: boolean): void => {
    if (!clip) return;
    run(
      ipc.pasteClip(clip.id, plain).then((outcome) => {
        if (outcome === "nothingToPaste") setHint(NO_PLAIN_TEXT);
      }),
    );
  };

  const clearUnpinned = (): void => {
    if (confirmClear) {
      disarmClear();
      run(ipc.clearUnpinned());
      return;
    }
    setConfirmClear(true);
    window.clearTimeout(clearTimer.current);
    clearTimer.current = window.setTimeout(() => setConfirmClear(false), CLEAR_CONFIRM_MS);
  };

  const current = clips[selected];
  const onKeyDown = useKeyboard({
    move: (d) => {
      setHint(null);
      setSelected((s) => (clips.length === 0 ? 0 : Math.min(clips.length - 1, Math.max(0, s + d))));
    },
    paste: (plain) => paste(current, plain),
    togglePin: () => current && run(ipc.setPinned(current.id, !current.pinned)),
    remove: () => current && run(ipc.deleteClip(current.id)),
    clearUnpinned,
    close: () => run(ipc.hidePopup()),
  });
  const onInputKeyDown = (e: React.KeyboardEvent<HTMLInputElement>): void => {
    const isClearKey = e.key === "Delete" && e.ctrlKey && e.shiftKey;
    if (isClearKey && e.repeat) {
      // Holding the shortcut must not arm and then clear.
      e.preventDefault();
      return;
    }
    // Any real key other than the clear shortcut cancels a pending clear.
    if (!isClearKey && !MODIFIER_KEYS.has(e.key)) disarmClear();
    onKeyDown(e);
  };

  const notices = banners(report);
  const empty = clips.length === 0;

  return (
    <div className="flex h-full flex-col">
      {notices.map((n) => (
        <p key={n.text} role={n.role} className="bg-warn-bg px-3 py-1.5 text-xs text-warn-fg">
          {n.text}
        </p>
      ))}
      <div className="border-b border-line p-2">
        <input
          ref={inputRef}
          autoFocus
          type="text"
          role="combobox"
          aria-expanded={!empty}
          aria-controls="clip-list"
          aria-activedescendant={current ? `clip-${current.id}` : undefined}
          aria-label="Search clipboard history"
          placeholder="Search clipboard history"
          autoComplete="off"
          spellCheck={false}
          value={query}
          onChange={(e) => {
            setHint(null);
            setQuery(e.target.value);
          }}
          onKeyDown={onInputKeyDown}
          className="h-8 w-full rounded-md border border-line bg-bg px-2.5 text-fg outline-none placeholder:text-muted focus:border-accent"
        />
      </div>
      <div
        id="clip-list"
        role="listbox"
        aria-label="Clipboard history"
        className="min-h-0 flex-1 overflow-y-auto py-1"
      >
        {empty ? (
          <p className="px-3 py-8 text-center text-muted">
            {query.trim() === "" ? "Nothing copied yet" : "No matches"}
          </p>
        ) : (
          clips.map((c, i) => (
            <ClipRow
              key={c.id}
              clip={c}
              selected={i === selected}
              onHover={() => setSelected(i)}
              onPick={() => paste(c, false)}
              onTogglePin={() => run(ipc.setPinned(c.id, !c.pinned))}
              onDelete={() => run(ipc.deleteClip(c.id))}
            />
          ))
        )}
      </div>
      <footer className="flex items-center gap-2 border-t border-line px-3 py-1.5 text-xs text-muted">
        <span className="min-w-0 flex-1 truncate" aria-live="polite">
          {hint ?? (report && !report.autoPasteAvailable ? COPY_ONLY_HINT : KEYS_HINT)}
        </span>
        <button
          type="button"
          tabIndex={-1}
          onClick={() => {
            clearUnpinned();
            inputRef.current?.focus();
          }}
          className={`shrink-0 rounded px-1.5 py-0.5 hover:bg-hover hover:text-fg ${confirmClear ? "text-fg" : ""}`}
        >
          <span aria-live="polite">{confirmClear ? "Click again to clear unpinned" : "Clear"}</span>
        </button>
      </footer>
    </div>
  );
}
