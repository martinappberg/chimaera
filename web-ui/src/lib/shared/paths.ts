// Daemon paths as the UI sees them. The daemon sends one form on every
// platform (chimaera_core::paths::wire_path): forward slashes, and a root
// that is `/` on a Unix host, a drive root `C:/` (upper-case letter) on a
// Windows host, or a UNC share root `//server/share/`. Every helper here
// works on that form, so no caller needs to know which OS the daemon runs on.
// The daemon accepts the same form back.

const DRIVE_RE = /^[A-Za-z]:(?:\/|$)/;
const UNC_RE = /^\/\/[^/]+\/[^/]+(?:\/|$)/;

/**
 * The root of an absolute path, always ending in `/`: `/`, `C:/`, or
 * `//server/share/`. `""` for a relative path.
 */
export function rootOf(path: string): string {
  const unc = UNC_RE.exec(path);
  if (unc) return unc[0].endsWith("/") ? unc[0] : `${unc[0]}/`;
  if (DRIVE_RE.test(path)) return `${path.slice(0, 2)}/`;
  return path.startsWith("/") ? "/" : "";
}

/** Whether `path` is absolute on the daemon's host. */
export function isAbsolute(path: string): boolean {
  return rootOf(path) !== "";
}

/** Whether `path` names a root itself (`/`, `C:/`, `C:`, `//srv/share`). */
export function isRoot(path: string): boolean {
  const root = rootOf(path);
  return root !== "" && (path === root || `${path}/` === root);
}

/** Drop trailing slashes, but never below the root. */
export function trimSlash(path: string): string {
  const root = rootOf(path);
  let end = path.length;
  while (end > root.length && path[end - 1] === "/") end--;
  return path.slice(0, end) || root;
}

/** The last segment; `""` for a root. */
export function basename(path: string): string {
  if (isRoot(path)) return "";
  const trimmed = trimSlash(path);
  const i = trimmed.lastIndexOf("/");
  return i >= 0 ? trimmed.slice(i + 1) : trimmed;
}

/** The containing directory; a root's parent is the root itself. */
export function dirname(path: string): string {
  const root = rootOf(path);
  if (isRoot(path)) return root;
  const trimmed = trimSlash(path);
  const i = trimmed.lastIndexOf("/");
  if (i < 0) return root || ".";
  return i < root.length ? root : trimmed.slice(0, i);
}

/** Join a directory and a relative leaf with exactly one `/`. */
export function joinPath(dir: string, leaf: string): string {
  if (dir === "") return leaf;
  return dir.endsWith("/") ? `${dir}${leaf}` : `${dir}/${leaf}`;
}

/** Whether `path` is `dir` itself or inside it. */
export function isUnder(path: string, dir: string): boolean {
  const d = trimSlash(dir);
  const p = trimSlash(path);
  if (p === d) return true;
  return p.startsWith(d.endsWith("/") ? d : `${d}/`);
}

/**
 * `path` relative to `dir` (`"."` when equal), or `null` when it is not
 * under `dir`.
 */
export function relativeTo(path: string, dir: string): string | null {
  const d = trimSlash(dir);
  const p = trimSlash(path);
  if (p === d) return ".";
  const prefix = d.endsWith("/") ? d : `${d}/`;
  return p.startsWith(prefix) ? p.slice(prefix.length) : null;
}

/**
 * Every directory from the root down to `path`, inclusive, for breadcrumbs:
 * `C:/Users/me` → `["C:/", "C:/Users", "C:/Users/me"]`.
 */
export function ancestors(path: string): string[] {
  const root = rootOf(path);
  const out = root ? [root] : [];
  const rest = trimSlash(path).slice(root.length);
  let acc = root;
  for (const seg of rest.split("/")) {
    if (seg === "") continue;
    acc = acc === "" ? seg : joinPath(acc, seg);
    out.push(acc);
  }
  return out;
}
