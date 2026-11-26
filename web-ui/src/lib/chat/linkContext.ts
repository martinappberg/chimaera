/**
 * The resolution context of a chat's path links, registered by App: the
 * same per-session context the terminal link provider uses (live cwd, spawn
 * cwd, workspace root, workspace id).
 *
 * A file of its own because App is in the always-loaded entry (a size
 * budget the build enforces): it imports this registry and none of
 * `paths.ts`, which loads with the chat.
 */

import type { LinkContext } from "../shared/fileRef";

type ContextLookup = (sessionId: string) => LinkContext;
let contextLookup: ContextLookup | null = null;

/** App-level wiring. Null unregisters. */
export function setChatLinkContext(fn: ContextLookup | null): void {
  contextLookup = fn;
}

/** The registered context for a session, or null (tests, a detached view). */
export function chatLinkContext(sessionId: string): LinkContext | null {
  return contextLookup?.(sessionId) ?? null;
}
