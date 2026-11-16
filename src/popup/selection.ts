/**
 * Which row is selected after the list changed. A reset (new query, popup
 * opened) selects the first row; otherwise the same clip stays selected, or
 * the row now at the old position when it is gone.
 */
export function nextSelection(
  selectedId: number | undefined,
  oldIndex: number,
  newIds: readonly number[],
  reset: boolean,
): number {
  if (reset || newIds.length === 0) return 0;
  if (selectedId !== undefined) {
    const same = newIds.indexOf(selectedId);
    if (same >= 0) return same;
  }
  return Math.min(Math.max(oldIndex, 0), newIds.length - 1);
}
