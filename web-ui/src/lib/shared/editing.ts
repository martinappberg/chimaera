/**
 * Shared dirty-state for lightweight single-file editing. The buffer store
 * (`previews/buffers.svelte.ts`) owns every editor buffer — mounted or not —
 * and mirrors which paths hold unsaved edits here; the pane tab shows a dot in
 * its glyph slot, pane keep-alive never evicts them, and App installs the
 * beforeunload guard and the reload gate whenever any file is dirty. Keeping
 * this out of the layout tree (and free of CodeMirror, which loads lazily)
 * means App and the panes can act on unsaved state before any editor loaded.
 */
import { writable } from "svelte/store";

/** Paths with unsaved edits in this window (mounted or not). */
export const dirtyFiles = writable<Set<string>>(new Set());

export function setDirty(path: string, dirty: boolean): void {
  dirtyFiles.update((s) => {
    if (dirty === s.has(path)) return s;
    const next = new Set(s);
    if (dirty) next.add(path);
    else next.delete(path);
    return next;
  });
}

/** Drop a path from the dirty set (tab closed / file gone). */
export function forgetDirty(path: string): void {
  setDirty(path, false);
}

/**
 * Whether this window's daemon link (the `/ws/events` socket) is up. App feeds
 * it; a save that failed on a dead link waits for it before its one retry.
 */
export const daemonLinkUp = writable(true);

export function noteDaemonLink(up: boolean): void {
  daemonLinkUp.set(up);
}

/** What the buffer store offers App for the close-with-unsaved-edits flow. */
export interface EditingHost {
  /** Save `path`; resolves true once it is clean on disk. */
  save(path: string): Promise<boolean>;
  /** Throw away `path`'s unsaved edits (and their journaled draft). */
  discard(path: string): void;
}

let host: EditingHost | null = null;

/** Registered by the buffer store when the editor first loads. */
export function setEditingHost(h: EditingHost): void {
  host = h;
}

/** Save a dirty file. With no editor loaded nothing can be dirty: true. */
export function saveDirtyFile(path: string): Promise<boolean> {
  return host === null ? Promise.resolve(true) : host.save(path);
}

export function discardDirtyFile(path: string): void {
  host?.discard(path);
}
