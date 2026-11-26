/**
 * An OS file drop on a session, on the computer the daemon runs on: the
 * file is already where the agent can read it, so the drop references it
 * where it is instead of copying it into the workspace's upload pad.
 *
 * A web view never tells the page where a dropped file lives (`File` has a
 * name and bytes, no path), so the native shell reports the paths of the
 * drag that just ended (`native.ts::droppedPaths`). That report is the
 * system's drag pasteboard, not this drop's own data, so nothing in it is
 * trusted by position: each dropped item takes a reported path only when
 * the name, the kind and (for a file) the size agree. An item without one
 * uploads, exactly as on a remote window or in a browser.
 */

/** One entry of the shell's report. */
export interface DroppedPath {
  /** Absolute, on this computer. */
  path: string;
  name: string;
  /** Bytes; 0 for a folder. */
  size: number;
  dir: boolean;
}

/** What the drop event itself says about one dropped item. */
export interface DroppedItem {
  name: string;
  /** Bytes; ignored for a folder (the browser reports an arbitrary size). */
  size: number;
  dir: boolean;
}

/** File names compare by their composed form: the same name reaches the
 *  page and the shell through different APIs. */
function sameName(a: string, b: string): boolean {
  return a.normalize("NFC") === b.normalize("NFC");
}

/**
 * The reported path each dropped item is at, or null where it has to
 * upload. A reported path is taken at most once, so two dropped files that
 * share a name and a size (from two folders) each get their own. Entries
 * that are not absolute paths are ignored: the report crossed a process
 * boundary.
 */
export function pairDroppedPaths(items: readonly DroppedItem[], reported: readonly DroppedPath[]): (DroppedPath | null)[] {
  const free = reported.filter((r) => r.path.startsWith("/") && r.name !== "");
  return items.map((item) => {
    const at = free.findIndex(
      (r) => r.dir === item.dir && sameName(r.name, item.name) && (item.dir || r.size === item.size),
    );
    return at < 0 ? null : free.splice(at, 1)[0];
  });
}

/** How one dropped item reaches the session. */
export type DropRoute =
  /** Its mention is typed with the path it already has. */
  | { how: "reference"; path: string; kind: "file" | "dir" }
  /** Its bytes go to the session's host, then the uploaded copy is mentioned. */
  | { how: "upload" }
  /** A folder with nowhere to reference it from: folders do not upload. */
  | { how: "refuse" };

/**
 * Route one dropped item. `local`: this window's files are this computer's
 * (`native.ts::hasLocalFiles`, the connection the app already knows — never
 * a guess from a host name). Off a local window nothing is referenced in
 * place, whatever path came along: that path would name another machine's
 * file.
 */
export function routeDrop(item: DroppedItem, at: DroppedPath | null, local: boolean): DropRoute {
  if (local && at !== null) return { how: "reference", path: at.path, kind: item.dir ? "dir" : "file" };
  return item.dir ? { how: "refuse" } : { how: "upload" };
}
