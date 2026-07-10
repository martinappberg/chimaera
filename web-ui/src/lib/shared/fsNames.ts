/**
 * Validation for user-typed file/folder names (the inline create and rename
 * inputs in the tree, Finder, and folder picker). Create mode allows `/` —
 * a nested `a/b/c.txt` creates the intermediate directories server-side —
 * rename never does.
 */

/** null = valid; otherwise a short message for the inline error slot. */
export function validateEntryName(
  name: string,
  opts: { allowSlashes: boolean },
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
