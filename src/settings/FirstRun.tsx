import { useState } from "react";
import { errorText, ipc } from "../lib/ipc";
import type { ForceReport } from "../lib/types";
import { Button, Dialog } from "./Dialog";

export default function FirstRun() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // What else lost Super+V; the window stays open until the user says OK.
  const [conflicts, setConflicts] = useState<ForceReport | null>(null);

  const close = async (): Promise<void> => {
    try {
      await ipc.hideSettings();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const answer = async (useSuperV: boolean): Promise<void> => {
    setBusy(true);
    setError(null);
    try {
      const notice = await ipc.firstRunChoice(useSuperV);
      if (notice && notice.freedFrom.length > 0) {
        setConflicts(notice);
      } else {
        await ipc.hideSettings();
      }
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  if (conflicts) {
    return (
      <Dialog
        title="Super+V is set"
        actions={
          <Button primary autoFocus disabled={busy} onClick={() => void close()}>
            OK
          </Button>
        }
      >
        <p role="status" className="mt-2 text-muted">
          Super+V was also removed from: {conflicts.freedFrom.join(", ")}. Reset shortcuts to default gives them back.
        </p>
        {error && (
          <p role="alert" className="mt-3 rounded-md bg-warn-bg px-3 py-1.5 text-xs text-warn-fg">
            {error}
          </p>
        )}
      </Dialog>
    );
  }

  return (
    <Dialog
      title="Use Super+V for clipboard history?"
      actions={
        <>
          <Button disabled={busy} onClick={() => void answer(false)}>
            Not now
          </Button>
          <Button primary autoFocus disabled={busy} onClick={() => void answer(true)}>
            Use Super+V
          </Button>
        </>
      }
    >
      <p className="mt-2 text-muted">GNOME&apos;s notification list will stay on Super+M.</p>
      {error && (
        <p role="alert" className="mt-3 rounded-md bg-warn-bg px-3 py-1.5 text-xs text-warn-fg">
          {error}
        </p>
      )}
    </Dialog>
  );
}
