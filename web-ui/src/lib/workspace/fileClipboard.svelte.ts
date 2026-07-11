/**
 * The in-app file clipboard: a single copied/cut file or directory, held per
 * window (which is per-daemon — every /fs path is absolute on this window's
 * daemon, and a remote daemon is a different window entirely, so a cross-daemon
 * paste is impossible by construction). Copy/cut record here; paste calls the
 * server-side /fs/copy or /fs/move (bytes never round-trip through the browser
 * for a same-daemon op). Cut clears itself once the move lands.
 *
 * Runes discipline: mutate the field only through this module's functions.
 */

import { basename, dirname, joinPath } from "../previews/files";
import { reportUploadError, trackFileOp } from "../net/uploads";
import { fsCopyOp, fsMoveOp } from "./fsEvents";

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
}

export function cutFile(path: string, kind: "dir" | "file"): void {
  clip = { path, kind, mode: "cut" };
}

export function clearClip(): void {
  clip = null;
}

/** True when `dest` is `src` itself or lies inside it (can't paste a dir into
 *  its own subtree). */
function within(dest: string, src: string): boolean {
  return dest === src || dest.startsWith(`${src}/`);
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
  const dest = joinPath(destDir, basename(c.path));
  if (c.kind === "dir" && within(destDir, c.path)) {
    reportUploadError("can't paste a folder into itself");
    return;
  }
  if (c.mode === "cut") {
    if (dirname(c.path) === destDir) return; // moving into the same folder is a no-op
    const moved = await trackFileOp(`Moving ${basename(c.path)}…`, () => fsMoveOp(c.path, dest));
    if (moved !== null) clearClip();
  } else {
    // Copy keeps the clipboard so the same source can be pasted repeatedly.
    await trackFileOp(`Copying ${basename(c.path)}…`, () => fsCopyOp(c.path, dest, "unique"));
  }
}
