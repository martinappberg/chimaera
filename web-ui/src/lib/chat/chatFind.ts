import type { ChatBlock } from "./store.svelte";
import { literalPattern } from "../shared/findText";

export interface MessageMatch { uid: number; excerpt: string; }
export const MESSAGE_MATCH_LIMIT = 500;

/** Search the retained conversation, not just its small mounted DOM window. */
export function findMessages(blocks: readonly ChatBlock[], query: string, caseSensitive: boolean): MessageMatch[] {
  const pattern = literalPattern(query, caseSensitive);
  if (!pattern) return [];
  const matches: MessageMatch[] = [];
  for (const block of blocks) {
    if (block.kind !== "user" && block.kind !== "message" && block.kind !== "agent_message") continue;
    pattern.lastIndex = 0;
    const match = pattern.exec(block.text);
    if (!match) continue;
    const start = Math.max(0, match.index - 50);
    const end = Math.min(block.text.length, match.index + match[0].length + 100);
    matches.push({ uid: block.uid, excerpt: `${start > 0 ? "…" : ""}${block.text.slice(start, end)}${end < block.text.length ? "…" : ""}` });
    if (matches.length >= MESSAGE_MATCH_LIMIT) break;
  }
  return matches;
}
