/**
 * Shared dirty-state for lightweight single-file editing. The CodeMirror
 * views (code + markdown edit side) mark a path dirty while it has unsaved
 * changes; the pane tab shows a dot in its glyph slot, and App installs a
 * beforeunload guard whenever any file is dirty. Keeping this out of the
 * layout tree means dirty state survives tab drags and pane restructuring.
 */
import { writable } from "svelte/store";

/** Paths with unsaved edits (any open editor). */
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
