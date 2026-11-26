/**
 * Names the app knows for session ids (set once from the live roster): an
 * agent passes an id to message or read another, and a tool row should say
 * the name the user sees. Ids it doesn't know stay ids.
 *
 * Its own module on purpose: the app shell is the one that sets the lookup,
 * and importing it from `toolLabels.ts` pulled that whole file (and what it
 * imports) into the always-loaded entry bundle.
 */
let resolveName: (id: string) => string | null = () => null;

export function setAgentNames(resolve: (id: string) => string | null): void {
  resolveName = resolve;
}

/** The name for a session id, or null when the app doesn't know it. */
export function agentName(id: string): string | null {
  return resolveName(id);
}
