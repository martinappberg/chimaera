/**
 * Shared path-affordance helpers for chat text (agent prose and the user's
 * own messages). Candidates are only ever a PRE-FILTER — /fs/validate decides
 * what actually becomes clickable, so these bound daemon traffic, not truth.
 */

/** Loose "could be a path" pre-filter: no whitespace, and a slash, tilde,
 *  dotfile, or extension-looking name. */
export function pathCandidate(s: string): boolean {
  if (s.length === 0 || s.length > 200 || /\s/.test(s)) return false;
  return (
    s.includes("/") ||
    s.startsWith("~") ||
    /^\.[\w.-]+$/.test(s) ||
    /^[\w@-][\w@.-]*\.[A-Za-z][A-Za-z0-9]{0,7}$/.test(s)
  );
}

/** A resolved /fs/validate hit. */
export interface PathHit {
  path: string;
  kind: "file" | "dir";
}

/** Batch validator signature threaded from ChatView down to the text
 *  renderers (candidates resolve against the session cwd). */
export type ResolvePaths = (candidates: string[]) => Promise<Map<string, PathHit>>;

/** Word split for user text: the clickable head (punctuation-trimmed) and
 *  the residual tail rendered as plain text ("see src/foo.rs." → head
 *  "src/foo.rs", tail "."). */
export function trimPathWord(word: string): { head: string; tail: string } {
  const head = word.replace(/[.,;:!?)\]}>'"`]+$/, "");
  return { head, tail: word.slice(head.length) };
}
