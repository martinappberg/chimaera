/**
 * Shared dirty-state for lightweight single-file editing. The buffer store
 * (`previews/buffers.svelte.ts`) owns every editor buffer — mounted or not —
 * and mirrors which paths hold unsaved edits here; the pane tab shows a dot in
 * its glyph slot, pane keep-alive never evicts them, and App installs the
 * beforeunload guard and the reload gate whenever any file is dirty. Keeping
 * this out of the layout tree (and free of CodeMirror, which loads lazily)
 * means App and the panes can act on unsaved state before any editor loaded.
 */
import { get, writable } from "svelte/store";

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

/**
 * How long the close dialog's "Save" waits before handing control back. A
 * save across a dead link waits for the link to return (the buffer retries
 * once it does), so without a deadline the dialog could say "saving…" for
 * ever.
 */
export const CLOSE_SAVE_DEADLINE_MS = 15_000;

export interface SaveDirtyResult {
  /** On disk and clean now. */
  saved: string[];
  /** Refused, failed, dirty again (keys landed mid-save), or unconfirmed at the deadline. */
  unsaved: string[];
  /** The deadline passed with a save still unconfirmed (it carries on in the background). */
  timedOut: boolean;
}

const TIMED_OUT = Symbol("timed out");

function within(save: Promise<boolean>, ms: number): Promise<boolean | typeof TIMED_OUT> {
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(TIMED_OUT), ms);
    const done = (ok: boolean) => {
      clearTimeout(timer);
      resolve(ok);
    };
    save.then(done, () => done(false));
  });
}

/**
 * Save `paths` one after another for the close dialog, waiting at most
 * `deadlineMs` in all. A save still running at the deadline is NOT abandoned
 * — it may yet land — but its path reports unsaved and the rest are not
 * started, so the dialog can say "not saved" and give control back. Null when
 * `cancelled()` turned true (checked after every wait): the caller then
 * touches nothing.
 */
export async function saveDirtyFiles(
  paths: readonly string[],
  opts: { deadlineMs: number; cancelled?: () => boolean },
): Promise<SaveDirtyResult | null> {
  const deadline = Date.now() + opts.deadlineMs;
  const out: SaveDirtyResult = { saved: [], unsaved: [], timedOut: false };
  for (const path of new Set(paths)) {
    const left = deadline - Date.now();
    const r = out.timedOut || left <= 0 ? TIMED_OUT : await within(saveDirtyFile(path), left);
    if (opts.cancelled?.() === true) return null;
    if (r === TIMED_OUT) out.timedOut = true;
    // Saved, but keys landed meanwhile: still dirty, so it keeps asking.
    if (r === true && !get(dirtyFiles).has(path)) out.saved.push(path);
    else out.unsaved.push(path);
  }
  return out;
}
