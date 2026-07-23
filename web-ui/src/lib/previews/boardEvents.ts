/**
 * The `{"type":"board"}` /ws/events frame, fanned out to the pane layer: a
 * board mutation somewhere — another window's /board/edit, a CLI append, a
 * journal comment — bumped a workspace's board epoch. Invalidate-and-pull:
 * the frame carries workspace ids only, never payload, so consumers re-probe
 * and refetch. The daemon dedupes frames per socket (only a moved epoch map
 * is ever sent), so arrival itself is the signal.
 */

import { writable } from "svelte/store";
import { revalidateBoardPaths } from "./fileStore.svelte";

/**
 * Monotonic nudge count. BoardView keys its pin-overlay refetch on it: a
 * journal append moves no file mtime, so the fileStore's revalidation signal
 * alone cannot carry a pin dropped from another window or the CLI.
 */
export const boardNudge = writable(0);

/** Handle a board epochs frame (wired by App into the events socket). */
export function onBoardNudge(_epochs: Record<string, number>): void {
  // The epoch map's workspace ids are not resolvable to board paths
  // client-side (a pane knows its path, not its workspace binding), so the
  // nudge is broadcast: every mounted board re-probes, cheaply.
  revalidateBoardPaths();
  boardNudge.update((n) => n + 1);
}
