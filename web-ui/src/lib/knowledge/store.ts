/**
 * The active workspace's knowledge as an index, derived once per snapshot
 * and shared by the Knowledge view and the dashboard. A snapshot that
 * didn't change keeps its object (the store only sets on new bytes), so
 * neither does this. Other surfaces resolve ids through the references
 * registry (`references.ts`), never through this store.
 */
import { derived, type Readable } from "svelte/store";

import { knowledge, type Knowledge } from "../workspace/knowledge";
import type { BadStatus } from "../workspace/timelineModel";
import { buildIndex, type KnowledgeIndex } from "./entries";
import { providerLabels, statusWord } from "./overview";

export interface KnowledgeLookup {
  k: Knowledge;
  idx: KnowledgeIndex;
}

/** Null until the active workspace has a provider's snapshot. */
export const knowledgeLookup: Readable<KnowledgeLookup | null> = derived(knowledge, (k) => {
  if (k === null || k.provider === null) return null;
  return { k, idx: buildIndex(k) };
});

/** Which Timeline statuses are bad news: the provider's status words whose
 *  tone is `bad` (none without a provider). */
export const knowledgeBadStatus: Readable<BadStatus> = derived(knowledgeLookup, (l) => {
  if (l === null) return () => false;
  const labels = providerLabels(l.k);
  return (to: string) => statusWord(labels, to)?.tone === "bad";
});
