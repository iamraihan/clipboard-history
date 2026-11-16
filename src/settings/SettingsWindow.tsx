import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { ipc } from "../lib/ipc";
import type { SettingsView } from "../lib/types";
import FirstRun from "./FirstRun";
import QuitPrompt from "./QuitPrompt";

// The backend decides which view the window shows (first run, quit prompt)
// and tells us with an event; on mount we ask in case we missed it.
export default function SettingsWindow() {
  const [view, setView] = useState<SettingsView>("home");

  useEffect(() => {
    let alive = true;
    const unlisten = listen<SettingsView>("settings-view-changed", (e) => setView(e.payload));
    ipc.settingsView().then(
      (v) => alive && setView(v),
      () => undefined,
    );
    return () => {
      alive = false;
      void unlisten.then((f) => f());
    };
  }, []);

  switch (view) {
    case "firstRun":
      return <FirstRun />;
    case "quitPrompt":
      return <QuitPrompt />;
    case "home":
      return <main className="p-4">Settings placeholder</main>;
  }
}
