/** Names the app knows for session ids (set once from the live roster):
 *  an agent passes an id to message or read another, and the card should say
 *  the name the user sees. Ids it doesn't know stay ids. Apart from
 *  `toolLabels` so the always-loaded shell that sets it doesn't carry the
 *  chat's labels. */
let resolve: (id: string) => string | null = () => null;

export function setAgentNames(names: (id: string) => string | null): void {
  resolve = names;
}

export function agentName(id: string): string | null {
  return resolve(id);
}
