/**
 * The in-app file clipboard: a single copied/cut file or directory, held per
 * window (which is per-daemon — every /fs path is absolute on this window's
 * daemon, and a remote daemon is a different window entirely, so a cross-daemon
 * paste is impossible by construction). Copy/cut record here; paste calls the
 * server-side /fs/copy or /fs/move (bytes never round-trip through the browser
 * for a same-daemon op). Cut clears itself once the move lands.
 *
 * A copy also goes to the OS clipboard where it has a form there (the file
 * itself on a local native window, else an image's picture), so it pastes
 * outside the workbench too. A cut stays in-app: nothing has moved yet.
 *
 * Runes discipline: mutate the field only through this module's functions.
 */

import { copyFileToOs } from "../shared/clipboard";
import { transferInto } from "./fileTransfer";

export interface FileClip {
  path: string;
  kind: "dir" | "file";
  mode: "copy" | "cut";
}

let clip = $state<FileClip | null>(null);

/** The current clipboard entry, or null. */
export function fileClip(): FileClip | null {
  return clip;
}

/** True when `path` is the pending CUT source (row renders dimmed). */
export function isCutPending(path: string): boolean {
  return clip !== null && clip.mode === "cut" && clip.path === path;
}

export function copyFile(path: string, kind: "dir" | "file"): void {
  clip = { path, kind, mode: "copy" };
  void copyFileToOs(path, kind);
}

export function cutFile(path: string, kind: "dir" | "file"): void {
  clip = { path, kind, mode: "cut" };
}

export function clearClip(): void {
  clip = null;
}

/**
 * Paste the clipboard entry INTO `destDir` (an absolute directory path). Copy
 * runs a server-side /fs/copy with macOS "name copy" collision handling; cut
 * runs /fs/move and clears the clipboard on success. No-ops a cut into the
 * same parent. Guards against pasting a directory into itself/its descendant.
 * All chrome (progress chip, errors) rides the shared upload job store.
 */
export async function pasteInto(destDir: string): Promise<void> {
  const c = clip;
  if (c === null) return;
  const result = await transferInto(c, destDir, c.mode === "cut" ? "move" : "copy");
  // A slow paste must not clear a newer copy/cut made while it was in flight.
  if (result !== null && c.mode === "cut" && clip === c) clearClip();
}
