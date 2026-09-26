/**
 * The remount key of a view that reads its whole file again on a new version
 * (FileView's `{#key}` around the spreadsheet, PDF, Parquet and binary
 * views). It moves when the file's version token CHANGES, never when it
 * first arrives: on a cold open the token lands (null → first value) moments
 * after the view mounted and started reading, and remounting then read the
 * file twice and dropped what the first mount had already taken — a
 * spreadsheet's `#sheet=…&range=…` reveal, which it holds while it switches
 * sheets. A token that goes away (the file vanished) and comes back
 * different moves it too. A new path starts over (FileView remounts per path
 * on its own).
 */

export interface VersionKey {
  path: string;
  /** The last token seen for `path` (kept while the token is absent). */
  mtime: string | null;
  key: number;
}

export const VERSION_KEY_START: VersionKey = { path: "", mtime: null, key: 0 };

export function nextVersionKey(prev: VersionKey, path: string, mtime: string | null): VersionKey {
  if (path !== prev.path) return { path, mtime, key: prev.key };
  if (mtime === null || mtime === prev.mtime) return prev;
  return { path, mtime, key: prev.mtime === null ? prev.key : prev.key + 1 };
}
