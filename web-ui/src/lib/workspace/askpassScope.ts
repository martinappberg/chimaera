import type { AskpassPrompt } from "../net/native";

/** Remote windows consume only their host's authentication prompts. Null is
 *  the local/home fallback and is also the only surface for an unscoped legacy
 *  prompt: guessing in a remote window could expose remote 2's auth on remote 1. */
export function askpassBelongsToHost(
  prompt: AskpassPrompt,
  hostAlias: string | null,
): boolean {
  return hostAlias === null || prompt.alias === hostAlias;
}
