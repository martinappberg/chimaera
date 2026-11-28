/**
 * Validation for user-typed file/folder names (the inline create and rename
 * inputs in the tree, Finder, and folder picker). Create mode allows `/` —
 * a nested `a/b/c.txt` creates the intermediate directories server-side —
 * rename never does.
 */

import { rootOf } from "./paths";

const WINDOWS_RESERVED_CHAR_RE = /[\\:*?"<>|]/;
// Device names are reserved with any extension (`nul.txt` is still NUL).
const WINDOWS_DEVICE_RE = /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i;

/** Whether `dir` (a daemon wire path) lives on a Windows host: its root is a
 *  drive (`C:/`) or a UNC share (`//srv/share/`), never `/`. */
function onWindowsHost(dir: string): boolean {
  const root = rootOf(dir);
  return root !== "" && root !== "/";
}

/** Windows' extra rules for one path segment, or null when it is allowed. */
function windowsSegmentError(segment: string): string | null {
  if (WINDOWS_RESERVED_CHAR_RE.test(segment)) return 'name cannot contain \\ : * ? " < > |';
  if (/[. ]$/.test(segment)) return "name cannot end with a dot or space";
  if (WINDOWS_DEVICE_RE.test(segment)) return `“${segment}” is a reserved name on Windows`;
  return null;
}

/**
 * null = valid; otherwise a short message for the inline error slot. `dir` is
 * the directory the entry lands in; when it is on a Windows host, Windows'
 * reserved characters, trailing dots/spaces and device names are refused too.
 */
export function validateEntryName(
  name: string,
  opts: { allowSlashes: boolean; dir?: string },
): string | null {
  if (name.trim() === "") return "name is empty";
  // eslint-disable-next-line no-control-regex
  if (/[\x00-\x1f]/.test(name)) return "name contains control characters";
  if (!opts.allowSlashes && name.includes("/")) return "name cannot contain /";
  if (name.startsWith("/") || name.endsWith("/")) {
    return "name cannot start or end with /";
  }
  if (name.includes("//")) return "name cannot contain //";
  for (const segment of name.split("/")) {
    if (segment === "." || segment === "..") {
      return "name cannot contain . or .. segments";
    }
  }
  if (opts.dir !== undefined && onWindowsHost(opts.dir)) {
    for (const segment of name.split("/")) {
      const invalid = windowsSegmentError(segment);
      if (invalid !== null) return invalid;
    }
  }
  return null;
}

/**
 * How much of `name` the rename input preselects: the basename minus its
 * extension (last dot), so typing replaces the stem and keeps the extension.
 * Dotfiles (.gitignore) select the whole name.
 */
export function stemLength(name: string): number {
  const i = name.lastIndexOf(".");
  return i > 0 ? i : name.length;
}
