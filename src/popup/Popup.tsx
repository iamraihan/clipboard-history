import { listen } from "@tauri-apps/api/event";
import { ClipboardList, Search } from "lucide-react";
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
// Footer key legend. Pin and delete keys are named on the row buttons' tooltips.
const KEYS: readonly (readonly [string, string])[] = [
  ["Enter", "Paste"],
  ["Shift+Enter", "Plain text"],
  ["Esc", "Close"],
];
// Where the app cannot paste for you (Wayland, macOS without Accessibility),
// picking only copies; the user pastes with the OS's own keys.
function copyOnlyKeys(pasteKeys: string): readonly (readonly [string, string])[] {
  return [
    ["Enter", "Copy"],
    [pasteKeys, "Paste it"],
    ["Esc", "Close"],
  ];
}

function Kbd({ children }: { children: string }) {
  return (
    <kbd className="rounded border border-line bg-bg px-1 py-px font-sans text-[11px] leading-4 text-fg">{children}</kbd>
  );
}

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
    <div className="flex h-full flex-col overflow-hidden rounded-xl border border-line bg-bg">
      {notices.map((n) => (
        <p key={n.text} role={n.role} className="bg-warn-bg px-3 py-1.5 text-xs text-warn-fg">
          {n.text}
        </p>
      ))}
      <div className="relative border-b border-line p-2">
        <Search aria-hidden className="pointer-events-none absolute top-1/2 left-4.5 h-3.5 w-3.5 -translate-y-1/2 text-muted" />
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
          className={`h-8 w-full rounded-md border border-line bg-bg pl-8 text-fg outline-none placeholder:text-muted focus:border-accent ${report?.shortcut ? "pr-[72px]" : "pr-2.5"}`}
        />
        {report?.shortcut ? (
          <span
            title={`${report.shortcut} opens this window from any app`}
            className="absolute top-1/2 right-[18px] -translate-y-1/2 opacity-80"
          >
            <Kbd>{report.shortcut}</Kbd>
          </span>
        ) : null}
      </div>
      <div
        id="clip-list"
        role="listbox"
        aria-label="Clipboard history"
        className="min-h-0 flex-1 overflow-y-auto py-1"
      >
        {empty ? (
          <div className="flex flex-col items-center px-6 pt-14 text-center">
            {query.trim() === "" ? (
              <>
                <ClipboardList aria-hidden className="mb-3 h-7 w-7 text-muted" strokeWidth={1.5} />
                <p className="font-medium text-fg">Nothing copied yet</p>
                <p className="mt-1 text-xs text-muted">
                  {report?.shortcut
                    ? `Copy something, then press ${report.shortcut} to open this list.`
                    : "Copy something and it will appear here."}
                </p>
              </>
            ) : (
              <p className="text-muted">No matches for “{query.trim()}”</p>
            )}
          </div>
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
      <footer className="flex h-8 items-center gap-2 border-t border-line pr-1.5 pl-3 text-xs text-muted">
        <span className="flex min-w-0 flex-1 items-center gap-3 truncate" aria-live="polite">
          {hint ??
            (report && !report.autoPasteAvailable ? copyOnlyKeys(report.manualPasteKeys) : KEYS).map(([key, action]) => (
              <span key={key} className="flex shrink-0 items-center gap-1">
                <Kbd>{key}</Kbd>
                {action}
              </span>
            ))}
        </span>
        <button
          type="button"
          tabIndex={-1}
          onClick={() => {
            clearUnpinned();
            inputRef.current?.focus();
          }}
          title="Clear unpinned items (Ctrl+Shift+Del)"
          className={`shrink-0 rounded px-1.5 py-0.5 ${confirmClear ? "bg-warn-bg text-warn-fg" : "hover:bg-hover hover:text-fg"}`}
        >
          <span aria-live="polite">{confirmClear ? "Click again to clear unpinned" : "Clear"}</span>
        </button>
      </footer>
    </div>
  );
}
