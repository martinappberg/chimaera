/**
 * Insert-into-composer registry: the chat counterpart of typing into a PTY's
 * input. The workbench's reference flows (selection "reference in agent",
 * copy provenance tags, @term: grants) compose text for a session's input —
 * for chat sessions that input is the mounted Composer, not a socket.
 */

const registry = new Map<string, (text: string) => void>();

/** Register a mounted composer's insert function; returns the unregister. */
export function registerComposer(sessionId: string, insert: (text: string) => void): () => void {
  registry.set(sessionId, insert);
  return () => {
    if (registry.get(sessionId) === insert) registry.delete(sessionId);
  };
}

/** Insert text into a session's composer; false when none is mounted. */
export function insertIntoComposer(sessionId: string, text: string): boolean {
  const insert = registry.get(sessionId);
  if (insert === undefined) return false;
  insert(text);
  return true;
}
