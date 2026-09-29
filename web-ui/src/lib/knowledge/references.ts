/**
 * The Knowledge snapshot as an id-reference source (shared/references.ts):
 * its `id_shapes` say which ids may become chips, its entries resolve them
 * (a reused finding id narrows to the citing entry's topic), and a chip
 * opens Knowledge at the entry. Registered while the active workspace has
 * a knowledge provider's snapshot; imported once by the app.
 */
import { derived } from "svelte/store";

import { registerReferenceSource, type RefTarget } from "../shared/references";
import { knowledgeRoot, openKnowledgeEntry, type KnowledgeLabels } from "../workspace/knowledge";
import { qualifiedId, resolveRef, type Entry, type KnowledgeIndex } from "./entries";
import { kindWord, providerLabels, stateLabel } from "./overview";
import { knowledgeLookup } from "./store";

export const KNOWLEDGE_SOURCE = "knowledge";

/** An entry as a reference target: its lines to preview, a note naming it,
 *  and an open that lands Knowledge on it. */
export function entryTarget(idx: KnowledgeIndex, e: Entry, labels: KnowledgeLabels, root: string | null): RefTarget {
  const parts = [qualifiedId(idx, e) || kindWord(labels, e.kind), kindWord(labels, e.kind)];
  if (e.date !== "") parts.push(e.date.slice(0, 10));
  const st = stateLabel(e);
  if (st !== null) parts.push(st.text);
  return {
    key: e.ekey,
    kind: e.kind,
    title: e.title,
    ...(e.span !== null ? { span: e.span, base: root } : {}),
    note: [...new Set(parts)].join(" · "),
    open: (from) => openKnowledgeEntry(e.ekey, from),
  };
}

function isEntry(v: unknown): v is Entry {
  return typeof v === "object" && v !== null && "ekey" in v && "kind" in v;
}

let unregister: (() => void) | null = null;

derived([knowledgeLookup, knowledgeRoot], (x) => x).subscribe(([lookup, root]) => {
  if (lookup === null || lookup.k.id_shapes.length === 0) {
    unregister?.();
    unregister = null;
    return;
  }
  const { k, idx } = lookup;
  const labels = providerLabels(k);
  // Registering replaces the source in place: one registry update per
  // snapshot, so every transcript re-links once, not twice.
  unregister = registerReferenceSource({
    id: KNOWLEDGE_SOURCE,
    shapes: k.id_shapes,
    root,
    lookup: (id, kind, near) =>
      resolveRef(idx, { kind, id }, isEntry(near) ? near : undefined).map((e) => entryTarget(idx, e, labels, root)),
  });
});
