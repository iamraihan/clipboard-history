import { useState } from "react";
import { errorText, ipc } from "../lib/ipc";
import type { QuitChoice } from "../lib/types";
import { Button, Dialog } from "./Dialog";

export default function QuitPrompt() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const answer = async (choice: QuitChoice): Promise<void> => {
    setBusy(true);
    setError(null);
    try {
      await ipc.quitChoice(choice);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      title="Give Super+V back to GNOME before quitting?"
      actions={
        <>
          <Button disabled={busy} onClick={() => void answer("cancel")}>
            Cancel
          </Button>
          <Button disabled={busy} onClick={() => void answer("keepAndQuit")}>
            Keep and quit
          </Button>
          <Button primary autoFocus disabled={busy} onClick={() => void answer("giveBackAndQuit")}>
            Give back and quit
          </Button>
        </>
      }
    >
      <p className="mt-2 text-muted">Giving it back restores the shortcut GNOME had before.</p>
      {error && (
        <p role="alert" className="mt-3 rounded-md bg-warn-bg px-3 py-1.5 text-xs text-warn-fg">
          {error}
        </p>
      )}
    </Dialog>
  );
}
