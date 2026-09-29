/**
 * Plain-word formatting for git history surfaces: short shas, compact
 * relative times, a commit's hover text, and a changed file's status in words
 * (the Source Control panel speaks people, not git).
 */
import { relativeAge } from "./launcher";
import type { GitChangedFile, GitCommit } from "./git";

export function shortSha(sha: string): string {
  return sha.slice(0, 7);
}

/** "3h", "2d" — the history rows' right-aligned, muted time. */
export function relTime(secs: number): string {
  return secs > 0 ? relativeAge(secs) : "";
}

/** A full local date, for hover text and the commit header. */
export function fullDate(secs: number): string {
  if (secs <= 0) return "";
  try {
    return new Date(secs * 1000).toLocaleString(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    });
  } catch {
    return new Date(secs * 1000).toISOString();
  }
}

/** A history row's hover: the message and a short sha. */
export function commitHover(c: GitCommit): string {
  const msg = c.body ? `${c.subject}\n\n${c.body}` : c.subject;
  return `${msg}\n\n${shortSha(c.sha)} · ${c.author} · ${fullDate(c.time)}`;
}

/** A changed file's status in words. */
export function statusWord(f: GitChangedFile): string {
  switch (f.status) {
    case "A":
      return "added";
    case "D":
      return "deleted";
    case "R":
      return "renamed";
    case "C":
      return "copied";
    case "T":
      return "type changed";
    case "?":
      return "new, not committed";
    default:
      return "modified";
  }
}

/** The single letter badge (the Changes list's own vocabulary). */
export function statusLetter(f: GitChangedFile): string {
  return f.status === "?" ? "U" : f.status;
}
