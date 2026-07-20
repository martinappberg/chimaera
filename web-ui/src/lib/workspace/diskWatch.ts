/**
 * Client half of the mounted-path disk monitor.
 *
 * File views retain exact file paths; listing surfaces publish the directories
 * they currently render. App forwards the two bounded-by-server snapshots on
 * `/ws/events`, and publishes the daemon's path-only invalidations back here.
 * This is deliberately separate from `fsEvents`: a disk observation has no
 * rename provenance, while an in-app mutation does and can safely rewrite tabs.
 */

import { writable } from "svelte/store";

export interface DiskChange {
  seq: number;
  /** Existing files whose metadata changed. */
  files: string[];
  /** Watched files that disappeared. */
  removed: string[];
  /** Visible directories whose membership may have changed. */
  dirs: string[];
  /** Watched directory roots that disappeared or stopped being directories. */
  removedDirs: string[];
}

export const diskWatchFiles = writable<string[]>([]);
export const diskWatchDirs = writable<string[]>([]);
export const lastDiskChange = writable<DiskChange | null>(null);

const fileRefs = new Map<string, number>();
const dirOwners = new Map<object, string[]>();
const WATCH_COUNT_CAP = 64;
const WATCH_PATH_BYTES = 4096;
const WATCH_BYTES_PER_KIND = 30 * 1024;
const encoder = new TextEncoder();
let publishedFiles: string[] = [];
let publishedDirs: string[] = [];
let seq = 0;

function same(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((value, i) => value === b[i]);
}

function unique(paths: Iterable<string>): string[] {
  return [...new Set([...paths].filter((path) => path.length > 0))];
}

/** Mirror the daemon caps before serializing, keeping the watch frame below
 *  its 128 KiB transport ceiling even with adversarially long file names. */
function bounded(paths: Iterable<string>): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  let bytes = 0;
  for (const path of paths) {
    const pathBytes = encoder.encode(path).byteLength;
    // JSON escaping can be much larger than the path itself (newlines,
    // quotes, backslashes), so charge the serialized representation against
    // the client transport budget while enforcing the daemon's raw-byte cap.
    const wireBytes = encoder.encode(JSON.stringify(path)).byteLength;
    if (
      out.length >= WATCH_COUNT_CAP ||
      path.length === 0 ||
      pathBytes > WATCH_PATH_BYTES ||
      seen.has(path) ||
      bytes + wireBytes > WATCH_BYTES_PER_KIND
    )
      continue;
    seen.add(path);
    bytes += wireBytes;
    out.push(path);
  }
  return out;
}

function publishFiles(): void {
  const next = bounded(fileRefs.keys());
  if (same(next, publishedFiles)) return;
  publishedFiles = next;
  diskWatchFiles.set(next);
}

function publishDirs(): void {
  function* ownedPaths(): Iterable<string> {
    for (const paths of dirOwners.values()) yield* paths;
  }
  const next = bounded(ownedPaths());
  if (same(next, publishedDirs)) return;
  publishedDirs = next;
  diskWatchDirs.set(next);
}

/** Pair with `releaseDiskFile`; multiple mounted views share one registration. */
export function retainDiskFile(path: string): void {
  const refs = (fileRefs.get(path) ?? 0) + 1;
  fileRefs.set(path, refs);
  if (refs === 1) publishFiles();
}

export function releaseDiskFile(path: string): void {
  const refs = fileRefs.get(path);
  if (refs === undefined) return;
  if (refs <= 1) {
    fileRefs.delete(path);
    publishFiles();
  } else {
    fileRefs.set(path, refs - 1);
  }
}

/** Replace one listing surface's visible directory set. */
export function setDiskDirs(owner: object, paths: Iterable<string>): void {
  const next = unique(paths);
  const previous = dirOwners.get(owner) ?? [];
  if (same(previous, next)) return;
  dirOwners.set(owner, next);
  publishDirs();
}

export function clearDiskDirs(owner: object): void {
  if (!dirOwners.delete(owner)) return;
  publishDirs();
}

/** Snapshot used when App creates the events socket after views already mount. */
export function currentDiskWatches(): { files: string[]; dirs: string[] } {
  return { files: publishedFiles, dirs: publishedDirs };
}

/** Publish one daemon fs frame to every interested surface. */
export function notifyDiskChange(change: Omit<DiskChange, "seq">): void {
  const files = unique(change.files);
  const removed = unique(change.removed);
  const dirs = unique(change.dirs);
  const removedDirs = unique(change.removedDirs);
  if (files.length + removed.length + dirs.length + removedDirs.length === 0) return;
  seq += 1;
  lastDiskChange.set({ seq, files, removed, dirs, removedDirs });
}
