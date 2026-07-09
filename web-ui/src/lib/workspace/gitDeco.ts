/**
 * Shared visual mapping for git status — one source of truth used by the file
 * tree, pane tabs, and the changes panel so a "modified" file looks the same
 * everywhere. VS Code's letter grammar (M/A/D/R/C/U/!), tinted with the
 * semantic `--git-*` tokens (see app.css).
 */
import type { GitEntry, GitDirCat } from "./git";

export interface GitDeco {
  /** Single-letter badge (M/A/D/R/C/T/U/!). */
  letter: string;
  /** CSS custom property (with `var(...)`) for the tint. */
  color: string;
  /** Human label for tooltips. */
  label: string;
}

/** The decoration for one status entry. */
export function decoFor(e: GitEntry): GitDeco {
  if (e.conflicted) return { letter: "!", color: "var(--git-conflict)", label: "Conflict" };
  if (e.untracked) return { letter: "U", color: "var(--git-untracked)", label: "Untracked" };
  // The significant code: the worktree (unstaged) status if present, else the
  // index (staged) status — mirrors what the user is most likely reviewing.
  const code = e.unstaged && e.y !== "." ? e.y : e.x;
  switch (code) {
    case "A":
      return { letter: "A", color: "var(--git-added)", label: "Added" };
    case "D":
      return { letter: "D", color: "var(--git-deleted)", label: "Deleted" };
    case "R":
      return { letter: "R", color: "var(--git-renamed)", label: "Renamed" };
    case "C":
      return { letter: "C", color: "var(--git-renamed)", label: "Copied" };
    case "T":
      return { letter: "T", color: "var(--git-modified)", label: "Type changed" };
    default:
      return { letter: "M", color: "var(--git-modified)", label: "Modified" };
  }
}

/** Tint for a folder rollup dot (collapsed directory with changes inside). */
export function dirColor(cat: GitDirCat): string {
  return cat === "conflicted"
    ? "var(--git-conflict)"
    : cat === "untracked"
      ? "var(--git-untracked)"
      : "var(--git-modified)";
}
