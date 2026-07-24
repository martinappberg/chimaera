/**
 * Pure hierarchy for BoardRail's layer tree: folds a page's top-level objects,
 * their GROUP descendants (real nested objects, from the parsed file), and
 * their COMPOSITE children (the render's derived `<id>/<part>` frames) into one
 * indented outline. Two nestings, one idiom.
 *
 * Layout truth stays server-side — this reads only identity (id/kind) from the
 * file's own parsed objects and the render's `childFrames` map, never geometry.
 * No network, no `$state`: BoardRail owns the reactive shell, this stays
 * vitest-able.
 */

import type { ChildFrame, ObjInfo } from "./boardInteract";

/**
 * How a click on a layer row selects on the stage, mapped to the two callbacks
 * BoardView already exposes to the rail:
 * - `object` → `onselect(id)`. A top-level object.
 * - `child` → `onselectchild(parent, id)`. A composite's derived child, or a
 *   GROUP MEMBER drilled into under its enclosing top-level group — one idiom
 *   for both nestings. `parent` stays the top-level object because that is the
 *   unit the stage moves (a group translates its whole subtree; a member has no
 *   move gesture of its own), so drilling in never costs draggability.
 */
export type LayerSelect =
  | { via: "object"; id: string }
  | { via: "child"; parent: string; id: string };

export interface LayerNode {
  /** Stable key + highlight identity: the object id, a group descendant's own
   *  id, or a composite child's derived `<parent>/<part>` id. */
  id: string;
  /** The row label — the id, or a composite child's `<part>` tail. */
  label: string;
  /** The object type, shown as the row glyph (the rail's existing idiom).
   *  Empty for composite children — the render's `childFrames` carries no
   *  kind, so those rows read as their derived tail alone, exactly as before. */
  kind: string;
  /** Indented children: group descendants and/or composite children. */
  children: LayerNode[];
  select: LayerSelect;
}

/** Depth + node ceilings. The board is agent-written (untrusted), so a
 *  pathological nesting or object count must never build an unbounded tree. */
const MAX_DEPTH = 8;
const MAX_NODES = 800;

/**
 * Build the outline tree for one page. Top-level objects come from the parsed
 * board (`objects`); a group's members come from that same parse (`children`);
 * a composite's children come from the render's `childFrames`. A group and a
 * composite are disjoint kinds, so an object never carries both.
 */
export function buildLayerTree(
  objects: readonly ObjInfo[],
  childFrames: Record<string, readonly ChildFrame[]>,
): LayerNode[] {
  let budget = MAX_NODES;
  const take = (): boolean => budget-- > 0;

  const compositeChildren = (id: string): LayerNode[] => {
    const prefix = `${id}/`;
    const out: LayerNode[] = [];
    for (const k of childFrames[id] ?? []) {
      if (!take()) break;
      out.push({
        id: k.id,
        label: k.id.startsWith(prefix) ? k.id.slice(prefix.length) : k.id,
        kind: "",
        children: [],
        select: { via: "child", parent: id, id: k.id },
      });
    }
    return out;
  };

  const groupChildren = (
    members: readonly ObjInfo[],
    groupId: string,
    depth: number,
  ): LayerNode[] => {
    if (depth > MAX_DEPTH) return [];
    const out: LayerNode[] = [];
    for (const c of members) {
      if (!take()) break;
      out.push({
        id: c.id,
        label: c.id,
        kind: c.kind,
        // A nested group recurses; every descendant drills in under the OUTER
        // top-level group — the whole unit the stage translates.
        children: groupChildren(c.children, groupId, depth + 1),
        select: { via: "child", parent: groupId, id: c.id },
      });
    }
    return out;
  };

  const roots: LayerNode[] = [];
  for (const o of objects) {
    if (!take()) break;
    roots.push({
      id: o.id,
      label: o.id,
      kind: o.kind,
      children:
        o.kind === "group" ? groupChildren(o.children, o.id, 1) : compositeChildren(o.id),
      select: { via: "object", id: o.id },
    });
  }
  return roots;
}

/**
 * The set of node ids to auto-open so the current selection is revealed: the
 * path from a root down to the node matching the selection, plus the target
 * itself (so selecting a GROUP opens it to show its contents, and drilling into
 * a COMPOSITE child opens its parent). A manual toggle overrides this.
 */
export function selectionBranch(
  roots: readonly LayerNode[],
  selected: string | null,
  selectedChild: string | null,
): Set<string> {
  const target = selectedChild ?? selected;
  if (target === null) return new Set();
  const out = new Set<string>();
  const path: string[] = [];
  const walk = (nodes: readonly LayerNode[]): boolean => {
    for (const n of nodes) {
      path.push(n.id);
      if (n.id === target || walk(n.children)) {
        for (const id of path) out.add(id);
        path.pop();
        return true;
      }
      path.pop();
    }
    return false;
  };
  walk(roots);
  out.add(target);
  return out;
}
