import type { KeyboardEvent } from "react";

export interface KeyActions {
  move: (delta: 1 | -1) => void;
  paste: (plain: boolean) => void;
  togglePin: () => void;
  remove: () => void;
  clearUnpinned: () => void;
  close: () => void;
}

/**
 * Key handler for the search input (it keeps focus the whole time).
 * Up/Down move, Enter pastes, Shift+Enter pastes plain text, Ctrl/Cmd+P pins,
 * Delete deletes (only when it would not edit the query), Ctrl+Shift+Delete
 * clears unpinned clips (the caller confirms with a second press), Esc closes.
 */
export function useKeyboard(actions: KeyActions) {
  return (e: KeyboardEvent<HTMLInputElement>): void => {
    if (e.nativeEvent.isComposing) return;
    const input = e.currentTarget;
    const handled = (): void => e.preventDefault();
    switch (e.key) {
      case "ArrowDown":
        handled();
        actions.move(1);
        break;
      case "ArrowUp":
        handled();
        actions.move(-1);
        break;
      case "Enter":
        handled();
        actions.paste(e.shiftKey);
        break;
      case "Escape":
        handled();
        actions.close();
        break;
      case "p":
      case "P":
        if (e.ctrlKey || e.metaKey) {
          handled();
          actions.togglePin();
        }
        break;
      case "Delete":
        if (e.ctrlKey && e.shiftKey) {
          handled();
          actions.clearUnpinned();
        } else if (input.value === "" || input.selectionStart === input.value.length) {
          handled();
          actions.remove();
        }
        break;
      default:
        break;
    }
  };
}
