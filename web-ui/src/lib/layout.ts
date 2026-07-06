/**
 * The in-window layout tree: internal nodes are row/col splits with a
 * draggable ratio, leaves are panes, and a pane holds a stack of tabs
 * (surfaces). Today every surface is a terminal session; the model stays
 * surface-agnostic so file previews can arrive later as new tab kinds.
 *
 * Everything here is pure and DOM-free: ops take a Layout and return a new
 * Layout (structural sharing where nothing changed), which keeps Svelte 5
 * reactivity trivial ("layout = op(layout)"). Dev-only self-checks at the
 * bottom of the module assert the core invariants.
 */

export type SplitDir = "row" | "col";
export type FocusDir = "left" | "right" | "up" | "down";
export type Zone = "left" | "right" | "top" | "bottom" | "center";

/** A surface shown as a tab. Extend this union for file previews at M3. */
export interface TerminalTab {
  surface: "terminal";
  sessionId: string;
}
export type Tab = TerminalTab;

export interface PaneNode {
  type: "pane";
  id: string;
  tabs: Tab[];
  /** Index into `tabs`; meaningless (0) when `tabs` is empty. */
  active: number;
}

export interface SplitNode {
  type: "split";
  id: string;
  dir: SplitDir;
  /** Fraction of the axis given to `a`; clamped to [MIN_RATIO, 1-MIN_RATIO]. */
  ratio: number;
  a: LayoutNode;
  b: LayoutNode;
}

export type LayoutNode = PaneNode | SplitNode;

export interface Layout {
  root: LayoutNode;
  focusedPaneId: string;
  /** Invariant: zoomedPaneId is null or equals focusedPaneId. */
  zoomedPaneId: string | null;
  focusMode: boolean;
}

export const MIN_RATIO = 0.05;
const MAX_DEPTH = 32;

let counter = 0;
function uid(): string {
  counter += 1;
  return `${counter.toString(36)}${Math.random().toString(36).slice(2, 8)}`;
}

function clampRatio(r: number): number {
  if (!Number.isFinite(r)) return 0.5;
  return Math.min(Math.max(r, MIN_RATIO), 1 - MIN_RATIO);
}

export function emptyPane(): PaneNode {
  return { type: "pane", id: uid(), tabs: [], active: 0 };
}

export function defaultLayout(): Layout {
  const pane = emptyPane();
  return { root: pane, focusedPaneId: pane.id, zoomedPaneId: null, focusMode: false };
}

/** All panes, in-order (left-to-right / top-to-bottom of the tree). */
export function panes(node: LayoutNode): PaneNode[] {
  if (node.type === "pane") return [node];
  return [...panes(node.a), ...panes(node.b)];
}

export function findPane(node: LayoutNode, id: string): PaneNode | null {
  if (node.type === "pane") return node.id === id ? node : null;
  return findPane(node.a, id) ?? findPane(node.b, id);
}

/** Where a session is open, if anywhere (the no-duplicates invariant). */
export function paneForSession(
  node: LayoutNode,
  sessionId: string,
): { paneId: string; index: number } | null {
  for (const p of panes(node)) {
    const index = p.tabs.findIndex((t) => t.sessionId === sessionId);
    if (index >= 0) return { paneId: p.id, index };
  }
  return null;
}

/** Every session shown anywhere in the tree. */
export function allSessionIds(l: Layout): string[] {
  const out: string[] = [];
  for (const p of panes(l.root)) for (const t of p.tabs) out.push(t.sessionId);
  return out;
}

/** The focused pane's active session, if it has one. */
export function focusedSession(l: Layout): string | null {
  const p = findPane(l.root, l.focusedPaneId);
  return p?.tabs[p.active]?.sessionId ?? null;
}

/**
 * Replace the node with `id` by `next` (remove-and-collapse when `next` is
 * null: the sibling takes the parent split's place). Returns null when the
 * whole tree vanishes. Untouched subtrees keep identity.
 */
function replaceNode(node: LayoutNode, id: string, next: LayoutNode | null): LayoutNode | null {
  if (node.id === id) return next;
  if (node.type === "pane") return node;
  const a = replaceNode(node.a, id, next);
  const b = replaceNode(node.b, id, next);
  if (a === node.a && b === node.b) return node;
  if (a === null) return b;
  if (b === null) return a;
  return { ...node, a, b };
}

function withPane(root: LayoutNode, id: string, fn: (p: PaneNode) => PaneNode): LayoutNode {
  const p = findPane(root, id);
  if (p === null) return root;
  return replaceNode(root, id, fn(p)) ?? root;
}

/**
 * Restore invariants: at least one pane exists, the focused pane exists,
 * active indices are in range, and zoom only ever points at the focused pane
 * (so focusing elsewhere always un-zooms and reveals the tree).
 */
function normalize(l: Layout): Layout {
  let root = l.root;
  let list = panes(root);
  if (list.length === 0) {
    const p = emptyPane();
    return { root: p, focusedPaneId: p.id, zoomedPaneId: null, focusMode: l.focusMode };
  }
  for (const p of list) {
    const active = p.tabs.length === 0 ? 0 : Math.min(Math.max(p.active, 0), p.tabs.length - 1);
    if (active !== p.active) root = withPane(root, p.id, (x) => ({ ...x, active }));
  }
  list = panes(root);
  const focusedPaneId = list.some((p) => p.id === l.focusedPaneId) ? l.focusedPaneId : list[0].id;
  const zoomedPaneId = l.zoomedPaneId === focusedPaneId ? l.zoomedPaneId : null;
  if (
    root === l.root &&
    focusedPaneId === l.focusedPaneId &&
    zoomedPaneId === l.zoomedPaneId
  ) {
    return l;
  }
  return { root, focusedPaneId, zoomedPaneId, focusMode: l.focusMode };
}

export function focusPane(l: Layout, paneId: string): Layout {
  if (findPane(l.root, paneId) === null || l.focusedPaneId === paneId) return l;
  return normalize({ ...l, focusedPaneId: paneId });
}

/**
 * Split `paneId`, placing `pane` (a fresh empty pane by default) before or
 * after it along `dir`. The new pane takes focus; zoom clears.
 */
export function splitPane(
  l: Layout,
  paneId: string,
  dir: SplitDir,
  before = false,
  pane?: PaneNode,
): Layout {
  const target = findPane(l.root, paneId);
  if (target === null) return l;
  const np = pane ?? emptyPane();
  const split: SplitNode = {
    type: "split",
    id: uid(),
    dir,
    ratio: 0.5,
    a: before ? np : target,
    b: before ? target : np,
  };
  const root = replaceNode(l.root, paneId, split) ?? split;
  return normalize({ ...l, root, focusedPaneId: np.id, zoomedPaneId: null });
}

export function setRatio(l: Layout, splitId: string, ratio: number): Layout {
  const clamped = clampRatio(ratio);
  const map = (node: LayoutNode): LayoutNode => {
    if (node.type === "pane") return node;
    if (node.id === splitId) return { ...node, ratio: clamped };
    const a = map(node.a);
    const b = map(node.b);
    return a === node.a && b === node.b ? node : { ...node, a, b };
  };
  const root = map(l.root);
  return root === l.root ? l : { ...l, root };
}

export function activateTab(l: Layout, paneId: string, index: number): Layout {
  const p = findPane(l.root, paneId);
  if (p === null || index < 0 || index >= p.tabs.length) return l;
  const root = withPane(l.root, paneId, (x) => ({ ...x, active: index }));
  return normalize({ ...l, root, focusedPaneId: paneId });
}

/**
 * Remove a tab from a pane; detaching the view never touches the session.
 * A pane left with zero tabs closes (its sibling absorbs the split), except
 * when it is the only pane, which simply stays empty.
 */
export function detachTab(l: Layout, paneId: string, index: number): Layout {
  const p = findPane(l.root, paneId);
  if (p === null || index < 0 || index >= p.tabs.length) return l;
  const tabs = p.tabs.toSpliced(index, 1);
  if (tabs.length === 0) {
    if (panes(l.root).length === 1) {
      return normalize({ ...l, root: withPane(l.root, paneId, (x) => ({ ...x, tabs: [], active: 0 })) });
    }
    const root = replaceNode(l.root, paneId, null);
    return normalize({ ...l, root: root ?? emptyPane() });
  }
  const active = index < p.active ? p.active - 1 : Math.min(p.active, tabs.length - 1);
  const root = withPane(l.root, paneId, (x) => ({ ...x, tabs, active }));
  return normalize({ ...l, root });
}

/**
 * Open a session: focus its existing tab if it is open anywhere (VS Code
 * semantics, no duplicates), otherwise append it to the focused pane.
 */
export function openSession(l: Layout, sessionId: string): Layout {
  const loc = paneForSession(l.root, sessionId);
  if (loc !== null) {
    return activateTab(l, loc.paneId, loc.index);
  }
  const paneId = findPane(l.root, l.focusedPaneId) !== null ? l.focusedPaneId : panes(l.root)[0]?.id;
  if (paneId === undefined) return l;
  const root = withPane(l.root, paneId, (p) => ({
    ...p,
    tabs: [...p.tabs, { surface: "terminal", sessionId }],
    active: p.tabs.length,
  }));
  return normalize({ ...l, root, focusedPaneId: paneId });
}

/** Cycle the focused pane's active tab by `delta` (wraps). */
export function cycleTab(l: Layout, delta: number): Layout {
  const p = findPane(l.root, l.focusedPaneId);
  if (p === null || p.tabs.length < 2) return l;
  const n = p.tabs.length;
  const active = (((p.active + delta) % n) + n) % n;
  const root = withPane(l.root, p.id, (x) => ({ ...x, active }));
  return normalize({ ...l, root });
}

export function toggleZoom(l: Layout): Layout {
  if (l.zoomedPaneId !== null) return { ...l, zoomedPaneId: null };
  return normalize({ ...l, zoomedPaneId: l.focusedPaneId });
}

/**
 * Drop a session onto a pane. Edge zones tear off into a new split on that
 * side; center adds (or moves) the tab into the pane. The session's existing
 * tab, if any, is detached first — the no-duplicates invariant holds.
 */
export function dropSession(l: Layout, sessionId: string, targetPaneId: string, zone: Zone): Layout {
  if (findPane(l.root, targetPaneId) === null) return l;
  const src = paneForSession(l.root, sessionId);

  if (zone === "center") {
    if (src !== null && src.paneId === targetPaneId) {
      return activateTab(l, targetPaneId, src.index);
    }
    let next = src !== null ? detachTab(l, src.paneId, src.index) : l;
    if (findPane(next.root, targetPaneId) === null) return l;
    const root = withPane(next.root, targetPaneId, (p) => ({
      ...p,
      tabs: [...p.tabs, { surface: "terminal", sessionId }],
      active: p.tabs.length,
    }));
    return normalize({ ...next, root, focusedPaneId: targetPaneId });
  }

  // Tearing the only tab of a pane onto that same pane's edge is a no-op.
  if (src !== null && src.paneId === targetPaneId) {
    const srcPane = findPane(l.root, src.paneId);
    if (srcPane !== null && srcPane.tabs.length === 1) return l;
  }
  const next = src !== null ? detachTab(l, src.paneId, src.index) : l;
  if (findPane(next.root, targetPaneId) === null) return l;
  const np: PaneNode = {
    type: "pane",
    id: uid(),
    tabs: [{ surface: "terminal", sessionId }],
    active: 0,
  };
  const dir: SplitDir = zone === "left" || zone === "right" ? "row" : "col";
  const before = zone === "left" || zone === "top";
  return splitPane(next, targetPaneId, dir, before, np);
}

/**
 * Move a session's tab to `index` within `paneId`'s tab bar (reorder or
 * cross-pane move); a session not open anywhere is inserted fresh.
 */
export function moveTabToIndex(l: Layout, sessionId: string, paneId: string, index: number): Layout {
  const target = findPane(l.root, paneId);
  if (target === null) return l;
  const src = paneForSession(l.root, sessionId);
  if (src !== null && src.paneId === paneId) {
    const insertAt = index > src.index ? index - 1 : index;
    if (insertAt === src.index) return activateTab(l, paneId, src.index);
    const tabs = target.tabs.toSpliced(src.index, 1).toSpliced(insertAt, 0, target.tabs[src.index]);
    const root = withPane(l.root, paneId, (x) => ({ ...x, tabs, active: insertAt }));
    return normalize({ ...l, root, focusedPaneId: paneId });
  }
  let next = src !== null ? detachTab(l, src.paneId, src.index) : l;
  const t = findPane(next.root, paneId);
  if (t === null) return l;
  const at = Math.min(Math.max(index, 0), t.tabs.length);
  const root = withPane(next.root, paneId, (x) => ({
    ...x,
    tabs: x.tabs.toSpliced(at, 0, { surface: "terminal", sessionId }),
    active: at,
  }));
  return normalize({ ...next, root, focusedPaneId: paneId });
}

/**
 * Drop tabs whose sessions no longer exist; panes emptied by pruning close
 * (the last pane survives, empty).
 */
export function pruneSessions(l: Layout, live: ReadonlySet<string>): Layout {
  const walk = (node: LayoutNode): LayoutNode | null => {
    if (node.type === "pane") {
      const tabs = node.tabs.filter((t) => live.has(t.sessionId));
      if (tabs.length === node.tabs.length) return node;
      if (tabs.length === 0) return null;
      const activeSession = node.tabs[node.active]?.sessionId;
      const keptActive = tabs.findIndex((t) => t.sessionId === activeSession);
      const active = keptActive >= 0 ? keptActive : Math.min(node.active, tabs.length - 1);
      return { ...node, tabs, active };
    }
    const a = walk(node.a);
    const b = walk(node.b);
    if (a === node.a && b === node.b) return node;
    if (a === null) return b;
    if (b === null) return a;
    return { ...node, a, b };
  };
  const root = walk(l.root);
  if (root === l.root) return l;
  return normalize({ ...l, root: root ?? emptyPane() });
}

// --- geometric focus navigation -------------------------------------------

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

function collectRects(node: LayoutNode, r: Rect, out: Map<string, Rect>): void {
  if (node.type === "pane") {
    out.set(node.id, r);
    return;
  }
  const ratio = clampRatio(node.ratio);
  if (node.dir === "row") {
    collectRects(node.a, { x: r.x, y: r.y, w: r.w * ratio, h: r.h }, out);
    collectRects(node.b, { x: r.x + r.w * ratio, y: r.y, w: r.w * (1 - ratio), h: r.h }, out);
  } else {
    collectRects(node.a, { x: r.x, y: r.y, w: r.w, h: r.h * ratio }, out);
    collectRects(node.b, { x: r.x, y: r.y + r.h * ratio, w: r.w, h: r.h * (1 - ratio) }, out);
  }
}

/**
 * Move pane focus geometrically: among panes strictly beyond the focused
 * pane's edge in `dir` that overlap it on the perpendicular axis, pick the
 * nearest (ties broken by larger overlap). No candidate → no-op.
 */
export function moveFocus(l: Layout, dir: FocusDir): Layout {
  const rects = new Map<string, Rect>();
  collectRects(l.root, { x: 0, y: 0, w: 1, h: 1 }, rects);
  const cur = rects.get(l.focusedPaneId);
  if (cur === undefined) return l;
  const EPS = 1e-9;
  let best: { id: string; dist: number; overlap: number } | null = null;
  for (const [id, r] of rects) {
    if (id === l.focusedPaneId) continue;
    let dist: number;
    let overlap: number;
    if (dir === "left" || dir === "right") {
      dist = dir === "left" ? cur.x - (r.x + r.w) : r.x - (cur.x + cur.w);
      overlap = Math.min(cur.y + cur.h, r.y + r.h) - Math.max(cur.y, r.y);
    } else {
      dist = dir === "up" ? cur.y - (r.y + r.h) : r.y - (cur.y + cur.h);
      overlap = Math.min(cur.x + cur.w, r.x + r.w) - Math.max(cur.x, r.x);
    }
    if (dist < -EPS || overlap <= EPS) continue;
    if (
      best === null ||
      dist < best.dist - EPS ||
      (Math.abs(dist - best.dist) <= EPS && overlap > best.overlap)
    ) {
      best = { id, dist, overlap };
    }
  }
  return best === null ? l : focusPane(l, best.id);
}

// --- (de)serialization ------------------------------------------------------

interface SPane {
  t: "p";
  id: string;
  tabs: { s: string }[];
  active: number;
}
interface SSplit {
  t: "s";
  id: string;
  dir: SplitDir;
  ratio: number;
  a: SNode;
  b: SNode;
}
type SNode = SPane | SSplit;

function serNode(node: LayoutNode): SNode {
  if (node.type === "pane") {
    return { t: "p", id: node.id, tabs: node.tabs.map((t) => ({ s: t.sessionId })), active: node.active };
  }
  return { t: "s", id: node.id, dir: node.dir, ratio: node.ratio, a: serNode(node.a), b: serNode(node.b) };
}

/** JSON-safe blob for the daemon's per-window view state. */
export function serializeLayout(l: Layout): unknown {
  return {
    v: 1,
    focusMode: l.focusMode,
    zoom: l.zoomedPaneId,
    focused: l.focusedPaneId,
    root: serNode(l.root),
  };
}

function isRecord(x: unknown): x is Record<string, unknown> {
  return typeof x === "object" && x !== null && !Array.isArray(x);
}

function deserNode(
  raw: unknown,
  depth: number,
  ids: Set<string>,
  seenSessions: Set<string>,
): LayoutNode | null {
  if (!isRecord(raw) || depth > MAX_DEPTH) return null;
  if (typeof raw.id !== "string" || raw.id.length === 0 || raw.id.length > 64 || ids.has(raw.id)) {
    return null;
  }
  ids.add(raw.id);
  if (raw.t === "p") {
    if (!Array.isArray(raw.tabs) || typeof raw.active !== "number") return null;
    const tabs: Tab[] = [];
    for (const t of raw.tabs) {
      if (!isRecord(t) || typeof t.s !== "string" || t.s.length === 0) return null;
      if (seenSessions.has(t.s)) continue; // enforce no-duplicates on load
      seenSessions.add(t.s);
      tabs.push({ surface: "terminal", sessionId: t.s });
    }
    const active = Number.isInteger(raw.active) ? raw.active : 0;
    return { type: "pane", id: raw.id, tabs, active };
  }
  if (raw.t === "s") {
    if (raw.dir !== "row" && raw.dir !== "col") return null;
    if (typeof raw.ratio !== "number") return null;
    const a = deserNode(raw.a, depth + 1, ids, seenSessions);
    const b = deserNode(raw.b, depth + 1, ids, seenSessions);
    if (a === null || b === null) return null;
    return { type: "split", id: raw.id, dir: raw.dir, ratio: clampRatio(raw.ratio), a, b };
  }
  return null;
}

/** Validate a persisted blob; anything malformed yields null (caller falls back to defaultLayout). */
export function deserializeLayout(raw: unknown): Layout | null {
  if (!isRecord(raw) || raw.v !== 1) return null;
  const root = deserNode(raw.root, 0, new Set(), new Set());
  if (root === null) return null;
  const focused = typeof raw.focused === "string" ? raw.focused : "";
  const zoom = typeof raw.zoom === "string" ? raw.zoom : null;
  return normalize({
    root,
    focusedPaneId: focused,
    zoomedPaneId: zoom,
    focusMode: raw.focusMode === true,
  });
}

// --- dev-only self-checks ---------------------------------------------------
//
// No test runner is wired up; these unit-style assertions run once on the
// dev server (dead code in production builds) and fail loudly in the console
// on any regression of the core invariants.
if (import.meta.env.DEV) {
  const ok = (cond: boolean, msg: string) => console.assert(cond, `layout.ts self-check: ${msg}`);

  // split right focuses a fresh empty pane
  let l = defaultLayout();
  const firstPane = l.focusedPaneId;
  l = splitPane(l, l.focusedPaneId, "row");
  ok(panes(l.root).length === 2, "split creates a second pane");
  ok(l.focusedPaneId !== firstPane, "split focuses the new pane");
  ok(findPane(l.root, l.focusedPaneId)?.tabs.length === 0, "new pane is empty");

  // openSession dedupes: focuses the existing tab instead of duplicating
  l = openSession(l, "sessA");
  l = focusPane(l, firstPane);
  l = openSession(l, "sessA");
  ok(allSessionIds(l).length === 1, "openSession never duplicates");
  ok(l.focusedPaneId !== firstPane, "openSession focuses the pane already holding the session");

  // detaching the last tab collapses the pane back to a single-pane tree
  const loc = paneForSession(l.root, "sessA");
  ok(loc !== null, "session is findable");
  if (loc !== null) l = detachTab(l, loc.paneId, loc.index);
  ok(panes(l.root).length === 1, "empty pane collapses into its sibling");

  // geometric focus: A | (B over C) — from A moving right lands in B or C,
  // moving right again is a no-op, moving down from B lands in C.
  let g = defaultLayout();
  const a = g.focusedPaneId;
  g = splitPane(g, a, "row"); // A | B'
  const bp = g.focusedPaneId;
  g = splitPane(g, bp, "col"); // A | (B' over C')
  const cp = g.focusedPaneId;
  g = focusPane(g, a);
  g = moveFocus(g, "right");
  ok(g.focusedPaneId === bp || g.focusedPaneId === cp, "focus moves right into the stacked column");
  const before = g.focusedPaneId;
  g = moveFocus(g, "right");
  ok(g.focusedPaneId === before, "focus at the edge is a no-op");
  g = focusPane(g, bp);
  g = moveFocus(g, "down");
  ok(g.focusedPaneId === cp, "focus moves down within the column");

  // zoom invariant: focusing another pane clears zoom
  g = toggleZoom(g);
  ok(g.zoomedPaneId === g.focusedPaneId, "zoom targets the focused pane");
  g = focusPane(g, a);
  ok(g.zoomedPaneId === null, "focus change clears zoom");

  // serialize -> deserialize round-trips the tree shape
  const round = deserializeLayout(JSON.parse(JSON.stringify(serializeLayout(g))));
  ok(round !== null, "serialized layout deserializes");
  ok(round !== null && panes(round.root).length === panes(g.root).length, "round-trip keeps pane count");
  ok(round !== null && round.focusedPaneId === g.focusedPaneId, "round-trip keeps focus");
  ok(deserializeLayout({ v: 1, root: { t: "x" } }) === null, "malformed blobs are rejected");

  // pruning dead sessions collapses emptied panes
  let pr = defaultLayout();
  pr = openSession(pr, "s1");
  pr = splitPane(pr, pr.focusedPaneId, "row");
  pr = openSession(pr, "s2");
  pr = pruneSessions(pr, new Set(["s1"]));
  ok(panes(pr.root).length === 1 && allSessionIds(pr).join() === "s1", "prune drops dead tabs and panes");
}
