/**
 * The in-window layout tree: internal nodes are row/col splits with a
 * draggable ratio, leaves are panes, and a pane holds a stack of tabs
 * (surfaces). Surfaces are terminal sessions or read-only file previews;
 * the model stays surface-agnostic so further tab kinds can arrive later.
 *
 * Everything here is pure and DOM-free: ops take a Layout and return a new
 * Layout (structural sharing where nothing changed), which keeps Svelte 5
 * reactivity trivial ("layout = op(layout)"). Dev-only self-checks at the
 * bottom of the module assert the core invariants.
 */

import type { DiffMode } from "../workspace/git";

export type SplitDir = "row" | "col";
export type FocusDir = "left" | "right" | "up" | "down";
export type Zone = "left" | "right" | "top" | "bottom" | "center";
/** A window edge (root-split drop target). */
export type Side = Exclude<Zone, "center">;

/** A surface shown as a tab. */
export interface TerminalTab {
  surface: "terminal";
  sessionId: string;
}
export interface FileTab {
  surface: "file";
  /** Absolute path on the daemon's filesystem. */
  path: string;
}
/** The settings surface — a singleton view (no-duplicates gives "focus the
 *  existing settings tab" for free, VS Code semantics). */
export interface SettingsTab {
  surface: "settings";
}
/**
 * A Finder (Miller-columns file browser). Keyed by a stable `id` — NOT by
 * `path` — so several Finders can coexist and each navigates freely; `path` is
 * that instance's current directory (mutable navigation state, persisted so a
 * reload restores where it was browsing).
 */
export interface FinderTab {
  surface: "finder";
  id: string;
  path: string;
}
/** A side-by-side git diff of one file, at a chosen comparison. The mode is
 *  part of the identity, so unstaged/staged diffs of the same file coexist. */
export interface DiffTab {
  surface: "diff";
  /** Absolute path of the file being diffed. */
  path: string;
  mode: DiffMode;
}
/** The source-control (changes) panel — a singleton view like settings. */
export interface GitTab {
  surface: "git";
}
/**
 * A review of the files ONE session changed — a session-scoped changes list
 * built on the same git status/diff APIs as the source-control panel. Keyed by
 * session so re-opening focuses the existing tab; it reads the session's live
 * touched-files list.
 */
export interface ChangesTab {
  surface: "changes";
  sessionId: string;
}
export type Tab =
  | TerminalTab
  | FileTab
  | SettingsTab
  | FinderTab
  | DiffTab
  | GitTab
  | ChangesTab;

/** Identity key for the no-duplicates invariant (one tab per surface). */
export function tabKey(t: Tab): string {
  if (t.surface === "terminal") return `s:${t.sessionId}`;
  if (t.surface === "file") return `f:${t.path}`;
  if (t.surface === "finder") return `d:${t.id}`;
  // `g:`, not `d:` — the Finder owns the `d:` namespace, and two surfaces
  // sharing a key prefix would alias inside the no-duplicates set.
  if (t.surface === "diff") return `g:${t.mode}:${t.path}`;
  if (t.surface === "git") return "v:git";
  if (t.surface === "changes") return `changes:${t.sessionId}`;
  return "v:settings";
}

export interface PaneNode {
  type: "pane";
  id: string;
  tabs: Tab[];
  /** Index into `tabs`; meaningless (0) when `tabs` is empty. */
  active: number;
  /**
   * Per-pane terminal font size override (px); undefined = the default.
   * Applies to whichever terminal tab the pane shows; persisted with the
   * layout so a reload keeps every pane's text size.
   */
  fontSize?: number;
}

/** Clamp bounds for the per-pane terminal font size (px). */
export const FONT_MIN = 9;
export const FONT_MAX = 28;

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

/** Where a surface is open, if anywhere (the no-duplicates invariant). */
export function paneForTab(node: LayoutNode, tab: Tab): { paneId: string; index: number } | null {
  const key = tabKey(tab);
  for (const p of panes(node)) {
    const index = p.tabs.findIndex((t) => tabKey(t) === key);
    if (index >= 0) return { paneId: p.id, index };
  }
  return null;
}

/** Every session shown anywhere in the tree. */
export function allSessionIds(l: Layout): string[] {
  const out: string[] = [];
  for (const p of panes(l.root))
    for (const t of p.tabs) if (t.surface === "terminal") out.push(t.sessionId);
  return out;
}

/** Every file path shown anywhere in the tree. */
export function allFilePaths(l: Layout): string[] {
  const out: string[] = [];
  for (const p of panes(l.root)) for (const t of p.tabs) if (t.surface === "file") out.push(t.path);
  return out;
}

/** Total number of tabs of any kind. */
export function tabCount(l: Layout): number {
  let n = 0;
  for (const p of panes(l.root)) n += p.tabs.length;
  return n;
}

/** The focused pane's active session, if its active tab is a terminal. */
export function focusedSession(l: Layout): string | null {
  const p = findPane(l.root, l.focusedPaneId);
  const t = p?.tabs[p.active];
  return t !== undefined && t.surface === "terminal" ? t.sessionId : null;
}

/** The focused pane's active file path, if its active tab is a file. */
export function focusedFile(l: Layout): string | null {
  const p = findPane(l.root, l.focusedPaneId);
  const t = p?.tabs[p.active];
  return t !== undefined && t.surface === "file" ? t.path : null;
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
 * Close a pane outright (its sibling absorbs the split). The last pane
 * never closes — with tabs it stays, empty it simply stays empty.
 */
export function closePane(l: Layout, paneId: string): Layout {
  if (findPane(l.root, paneId) === null) return l;
  if (panes(l.root).length === 1) return l;
  const root = replaceNode(l.root, paneId, null);
  return normalize({ ...l, root: root ?? emptyPane() });
}

/**
 * Open a surface: focus its existing tab if it is open anywhere (VS Code
 * semantics, no duplicates), otherwise append it to the focused pane.
 */
export function openTab(l: Layout, tab: Tab): Layout {
  const loc = paneForTab(l.root, tab);
  if (loc !== null) {
    return activateTab(l, loc.paneId, loc.index);
  }
  const paneId = findPane(l.root, l.focusedPaneId) !== null ? l.focusedPaneId : panes(l.root)[0]?.id;
  if (paneId === undefined) return l;
  const root = withPane(l.root, paneId, (p) => ({
    ...p,
    tabs: [...p.tabs, tab],
    active: p.tabs.length,
  }));
  return normalize({ ...l, root, focusedPaneId: paneId });
}

export function openSession(l: Layout, sessionId: string): Layout {
  return openTab(l, { surface: "terminal", sessionId });
}

/** The pane currently holding a terminal tab for `sessionId`, if any. */
export function sessionPaneId(l: Layout, sessionId: string): string | null {
  return (
    paneForTab(l.root, { surface: "terminal", sessionId })?.paneId ?? null
  );
}

export function openFile(l: Layout, path: string): Layout {
  return openTab(l, { surface: "file", path });
}

/** Open (or focus) a side-by-side diff of `path` at the given comparison. */
export function openDiff(l: Layout, path: string, mode: DiffMode): Layout {
  return openTab(l, { surface: "diff", path, mode });
}

/** Open (or focus) the source-control panel. */
export function openGit(l: Layout): Layout {
  return openTab(l, { surface: "git" });
}

/** Open (or focus) the session-scoped changes review. */
export function openChanges(l: Layout, sessionId: string): Layout {
  return openTab(l, { surface: "changes", sessionId });
}

export function openSettings(l: Layout): Layout {
  return openTab(l, { surface: "settings" });
}

/**
 * Open a fresh Finder at `path` (a new instance every call — Finders are keyed
 * by id, so this never dedupes onto an existing one). Returns the new layout
 * and the minted id, so the caller can drive that instance afterward.
 */
export function openFinder(l: Layout, path: string): { layout: Layout; id: string } {
  const id = uid();
  return { layout: openTab(l, { surface: "finder", id, path }), id };
}

/**
 * A Finder tab NOT yet in any layout — the drag payload for a directory
 * (tree rows drag dirs as Finders, so a zone/tab drop opens a legitimate
 * browsing surface instead of a broken file preview). Each call mints a
 * fresh instance id, matching openFinder's never-dedupe semantics.
 */
export function freshFinderTab(path: string): FinderTab {
  return { surface: "finder", id: uid(), path };
}

/** The Finder instance with `id`, and where it lives, if open. */
export function findFinder(
  l: Layout,
  id: string,
): { paneId: string; index: number; tab: FinderTab } | null {
  for (const p of panes(l.root)) {
    const index = p.tabs.findIndex((t) => t.surface === "finder" && t.id === id);
    if (index >= 0) return { paneId: p.id, index, tab: p.tabs[index] as FinderTab };
  }
  return null;
}

/** Update a specific Finder's current directory (its navigation state). */
export function setFinderPath(l: Layout, id: string, path: string): Layout {
  const loc = findFinder(l, id);
  if (loc === null || loc.tab.path === path) return l;
  const root = withPane(l.root, loc.paneId, (p) => ({
    ...p,
    tabs: p.tabs.map((t, i) =>
      i === loc.index && t.surface === "finder" ? { ...t, path } : t,
    ),
  }));
  return root === l.root ? l : { ...l, root };
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

/** Set (or clear, with undefined) a pane's terminal font-size override. */
export function setPaneFont(l: Layout, paneId: string, size: number | undefined): Layout {
  const p = findPane(l.root, paneId);
  if (p === null || p.fontSize === size) return l;
  const clamped =
    size === undefined ? undefined : Math.min(Math.max(Math.round(size * 2) / 2, FONT_MIN), FONT_MAX);
  const root = withPane(l.root, paneId, (x) => {
    const next = { ...x };
    if (clamped === undefined) delete next.fontSize;
    else next.fontSize = clamped;
    return next;
  });
  return root === l.root ? l : { ...l, root };
}

/**
 * The pane a surface opened from `paneId` should land in: the geometrically
 * nearest neighbor (right, else left, below, above). Null when `paneId` is
 * the only pane — the caller splits instead.
 */
export function adjacentPane(l: Layout, paneId: string): string | null {
  if (findPane(l.root, paneId) === null) return null;
  const probe: Layout = { ...l, focusedPaneId: paneId, zoomedPaneId: null };
  for (const dir of ["right", "left", "down", "up"] as const) {
    const moved = moveFocus(probe, dir);
    if (moved.focusedPaneId !== paneId) return moved.focusedPaneId;
  }
  return null;
}

export function toggleZoom(l: Layout): Layout {
  if (l.zoomedPaneId !== null) return { ...l, zoomedPaneId: null };
  return normalize({ ...l, zoomedPaneId: l.focusedPaneId });
}

/**
 * Carry the focused pane's active tab into the neighboring pane in `dir`,
 * focus following the tab. No-op without a neighbor, and while zoomed —
 * the other panes aren't visible, so a silent move would be disorienting.
 */
export function moveTabDirection(l: Layout, dir: FocusDir): Layout {
  if (l.zoomedPaneId !== null) return l;
  const p = findPane(l.root, l.focusedPaneId);
  if (p === null || p.tabs.length === 0) return l;
  const probe = moveFocus(l, dir);
  if (probe.focusedPaneId === l.focusedPaneId) return l;
  // center drop detaches from the source pane and focuses the target
  return dropTab(l, p.tabs[p.active], probe.focusedPaneId, "center");
}

/**
 * Drop a surface onto a pane. Edge zones tear off into a new split on that
 * side; center adds (or moves) the tab into the pane. The surface's existing
 * tab, if any, is detached first — the no-duplicates invariant holds.
 */
export function dropTab(l: Layout, tab: Tab, targetPaneId: string, zone: Zone): Layout {
  if (findPane(l.root, targetPaneId) === null) return l;
  const src = paneForTab(l.root, tab);

  if (zone === "center") {
    if (src !== null && src.paneId === targetPaneId) {
      return activateTab(l, targetPaneId, src.index);
    }
    let next = src !== null ? detachTab(l, src.paneId, src.index) : l;
    if (findPane(next.root, targetPaneId) === null) return l;
    const root = withPane(next.root, targetPaneId, (p) => ({
      ...p,
      tabs: [...p.tabs, tab],
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
    tabs: [tab],
    active: 0,
  };
  const dir: SplitDir = zone === "left" || zone === "right" ? "row" : "col";
  const before = zone === "left" || zone === "top";
  return splitPane(next, targetPaneId, dir, before, np);
}

/**
 * Drop a surface on a window edge: split the ROOT, the new pane taking the
 * full window height/width on that side. Re-creating the shape that already
 * exists (the surface's single-tab pane already spans that edge) is a no-op.
 */
export function dropTabAtRootEdge(l: Layout, tab: Tab, side: Side): Layout {
  const dir: SplitDir = side === "left" || side === "right" ? "row" : "col";
  const before = side === "left" || side === "top";
  const src = paneForTab(l.root, tab);
  if (src !== null) {
    const sp = findPane(l.root, src.paneId);
    if (sp !== null && sp.tabs.length === 1) {
      // Its pane would collapse and re-materialize in the same place.
      if (panes(l.root).length === 1) return l;
      if (l.root.type === "split" && l.root.dir === dir) {
        const edgeChild = before ? l.root.a : l.root.b;
        if (edgeChild.id === src.paneId) return l;
      }
    }
  }
  const next = src !== null ? detachTab(l, src.paneId, src.index) : l;
  const np: PaneNode = { type: "pane", id: uid(), tabs: [tab], active: 0 };
  const split: SplitNode = {
    type: "split",
    id: uid(),
    dir,
    ratio: 0.5,
    a: before ? np : next.root,
    b: before ? next.root : np,
  };
  return normalize({ ...next, root: split, focusedPaneId: np.id, zoomedPaneId: null });
}

/**
 * Move a surface's tab to `index` within `paneId`'s tab bar (reorder or
 * cross-pane move); a surface not open anywhere is inserted fresh.
 */
export function moveTabToIndex(l: Layout, tab: Tab, paneId: string, index: number): Layout {
  const target = findPane(l.root, paneId);
  if (target === null) return l;
  const src = paneForTab(l.root, tab);
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
    tabs: x.tabs.toSpliced(at, 0, tab),
    active: at,
  }));
  return normalize({ ...next, root, focusedPaneId: paneId });
}

/**
 * Drop tabs failing `keep`; panes emptied by pruning close (the last pane
 * survives, empty). The active tab follows its surface when it survives.
 */
function pruneTabs(l: Layout, keep: (t: Tab) => boolean): Layout {
  const walk = (node: LayoutNode): LayoutNode | null => {
    if (node.type === "pane") {
      const tabs = node.tabs.filter(keep);
      if (tabs.length === node.tabs.length) return node;
      if (tabs.length === 0) return null;
      const activeTab = node.tabs[node.active];
      const keptActive =
        activeTab !== undefined ? tabs.findIndex((t) => tabKey(t) === tabKey(activeTab)) : -1;
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

/** Drop terminal AND session-changes tabs whose sessions no longer exist; a
 *  changes review outlives nothing its session left behind. File and (path-
 *  keyed) git-diff tabs are untouched. */
export function pruneSessions(l: Layout, live: ReadonlySet<string>): Layout {
  return pruneTabs(
    l,
    (t) =>
      (t.surface !== "terminal" && t.surface !== "changes") || live.has(t.sessionId),
  );
}

/** Drop file tabs whose paths are known-dead (404 on restore). */
export function pruneFiles(l: Layout, dead: ReadonlySet<string>): Layout {
  return pruneTabs(l, (t) => t.surface !== "file" || !dead.has(t.path));
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

/** Tab wire form: `{s}` terminal, `{f}` file, `{d,di}` finder (dir + instance
 *  id), `{gd,dm}` git diff (path + mode), `{cs}` session changes, `{v}` view
 *  (additive within blob v1; `v` is "settings" or "git"). */
type STab =
  | { s: string }
  | { f: string }
  | { v: string }
  | { d: string; di: string }
  | { gd: string; dm?: string }
  | { cs: string };

/** Coerce a persisted diff mode, defaulting to unstaged. */
function diffModeOf(x: unknown): DiffMode {
  return x === "staged" || x === "head" ? x : "unstaged";
}
interface SPane {
  t: "p";
  id: string;
  tabs: STab[];
  active: number;
  /** Per-pane terminal font size (px), when overridden. */
  fs?: number;
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
    const pane: SPane = {
      t: "p",
      id: node.id,
      tabs: node.tabs.map((t): STab => {
        if (t.surface === "terminal") return { s: t.sessionId };
        if (t.surface === "file") return { f: t.path };
        if (t.surface === "finder") return { d: t.path, di: t.id };
        if (t.surface === "diff") return { gd: t.path, dm: t.mode };
        if (t.surface === "git") return { v: "git" };
        if (t.surface === "changes") return { cs: t.sessionId };
        return { v: "settings" };
      }),
      active: node.active,
    };
    if (node.fontSize !== undefined) pane.fs = node.fontSize;
    return pane;
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
  seenTabs: Set<string>,
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
      if (!isRecord(t)) return null;
      let tab: Tab;
      if (typeof t.s === "string" && t.s.length > 0) {
        tab = { surface: "terminal", sessionId: t.s };
      } else if (typeof t.f === "string" && t.f.length > 0 && t.f.length <= 4096) {
        tab = { surface: "file", path: t.f };
      } else if (typeof t.d === "string" && t.d.length > 0 && t.d.length <= 4096) {
        // Finder: `di` is the instance id (mint a fresh one for pre-finder
        // blobs that somehow carry `d` without it).
        const id = typeof t.di === "string" && t.di.length > 0 ? t.di : uid();
        tab = { surface: "finder", id, path: t.d };
      } else if (typeof t.gd === "string" && t.gd.length > 0 && t.gd.length <= 4096) {
        tab = { surface: "diff", path: t.gd, mode: diffModeOf(t.dm) };
      } else if (t.v === "git") {
        tab = { surface: "git" };
      } else if (typeof t.cs === "string" && t.cs.length > 0) {
        tab = { surface: "changes", sessionId: t.cs };
      } else if (t.v === "settings") {
        tab = { surface: "settings" };
      } else {
        // A record-shaped tab of an unrecognized kind is almost certainly a
        // tab kind from a NEWER build persisted then rolled back to this one:
        // skip just that tab, don't null the whole pane (which would reset the
        // entire saved layout to default). Genuinely malformed structure —
        // a non-record entry (above) — still fails the pane.
        continue;
      }
      const key = tabKey(tab);
      if (seenTabs.has(key)) continue; // enforce no-duplicates on load
      seenTabs.add(key);
      tabs.push(tab);
    }
    const active = Number.isInteger(raw.active) ? raw.active : 0;
    const pane: PaneNode = { type: "pane", id: raw.id, tabs, active };
    if (typeof raw.fs === "number" && raw.fs >= FONT_MIN && raw.fs <= FONT_MAX) {
      pane.fontSize = raw.fs;
    }
    return pane;
  }
  if (raw.t === "s") {
    if (raw.dir !== "row" && raw.dir !== "col") return null;
    if (typeof raw.ratio !== "number") return null;
    const a = deserNode(raw.a, depth + 1, ids, seenTabs);
    const b = deserNode(raw.b, depth + 1, ids, seenTabs);
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

  // openFile dedupes across panes exactly like sessions
  l = focusPane(l, firstPane);
  l = openFile(l, "/tmp/a.txt");
  ok(l.focusedPaneId === firstPane, "openFile lands in the focused pane");
  l = focusPane(l, panes(l.root).find((p) => p.id !== firstPane)?.id ?? firstPane);
  l = openFile(l, "/tmp/a.txt");
  ok(allFilePaths(l).length === 1, "openFile never duplicates");
  ok(l.focusedPaneId === firstPane, "openFile focuses the pane already holding the file");

  // detaching the last tab collapses the pane back to a single-pane tree
  const locA = paneForTab(l.root, { surface: "file", path: "/tmp/a.txt" });
  ok(locA !== null, "file tab is findable");
  if (locA !== null) l = detachTab(l, locA.paneId, locA.index);
  const locS = paneForTab(l.root, { surface: "terminal", sessionId: "sessA" });
  ok(locS !== null, "session is findable");
  if (locS !== null) l = detachTab(l, locS.paneId, locS.index);
  ok(panes(l.root).length === 1, "empty pane collapses into its sibling");

  // closePane collapses a pane outright but never removes the last one
  let cl = defaultLayout();
  cl = splitPane(cl, cl.focusedPaneId, "row");
  const clNew = cl.focusedPaneId;
  cl = closePane(cl, clNew);
  ok(panes(cl.root).length === 1, "closePane collapses the pane");
  ok(findPane(cl.root, clNew) === null, "closed pane is gone");
  const clOnly = cl.focusedPaneId;
  cl = closePane(cl, clOnly);
  ok(panes(cl.root).length === 1 && cl.focusedPaneId === clOnly, "last pane never closes");

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

  // serialize -> deserialize round-trips the tree shape, including file tabs
  g = openFile(g, "/tmp/readme.md");
  const round = deserializeLayout(JSON.parse(JSON.stringify(serializeLayout(g))));
  ok(round !== null, "serialized layout deserializes");
  ok(round !== null && panes(round.root).length === panes(g.root).length, "round-trip keeps pane count");
  ok(round !== null && round.focusedPaneId === g.focusedPaneId, "round-trip keeps focus");
  ok(
    round !== null && allFilePaths(round).join() === "/tmp/readme.md",
    "round-trip keeps file tabs",
  );
  ok(deserializeLayout({ v: 1, root: { t: "x" } }) === null, "malformed blobs are rejected");

  // a record-shaped tab of an unknown kind (persisted by a newer build, then
  // rolled back to this one) is SKIPPED, not fatal — the pane and its other
  // tabs survive rather than the whole layout resetting to default.
  const mixed = deserializeLayout({
    v: 1,
    focused: "mixp",
    root: { t: "p", id: "mixp", active: 0, tabs: [{ nb: "/tmp/x.ipynb" }, { f: "/tmp/keep.md" }] },
  });
  ok(mixed !== null, "an unknown tab kind does not null its pane");
  ok(
    mixed !== null && allFilePaths(mixed).join() === "/tmp/keep.md",
    "the unknown tab is skipped; its sibling tabs survive",
  );

  // settings surface: singleton (dedupes) and survives serialization
  let st = defaultLayout();
  st = openSettings(st);
  st = openSettings(st);
  ok(tabCount(st) === 1, "openSettings never duplicates");
  const stRound = deserializeLayout(JSON.parse(JSON.stringify(serializeLayout(st))));
  ok(
    stRound !== null &&
      findPane(stRound.root, stRound.focusedPaneId)?.tabs[0]?.surface === "settings",
    "settings tab round-trips",
  );

  // finder surface: keyed by id, so two coexist; each navigates independently
  // and both survive serialization with their own path.
  let fd = defaultLayout();
  const o1 = openFinder(fd, "/tmp");
  fd = o1.layout;
  const o2 = openFinder(fd, "/etc");
  fd = o2.layout;
  ok(tabCount(fd) === 2, "openFinder never dedupes — two Finders coexist");
  fd = setFinderPath(fd, o1.id, "/tmp/sub");
  ok(findFinder(fd, o1.id)?.tab.path === "/tmp/sub", "setFinderPath moves one instance");
  ok(findFinder(fd, o2.id)?.tab.path === "/etc", "the other Finder is untouched");
  const fdRound = deserializeLayout(JSON.parse(JSON.stringify(serializeLayout(fd))));
  ok(
    fdRound !== null &&
      findFinder(fdRound, o1.id)?.tab.path === "/tmp/sub" &&
      findFinder(fdRound, o2.id)?.tab.path === "/etc",
    "both Finders round-trip with their ids and paths",
  );

  // window-edge root split: new pane spans the full edge; same-place no-ops
  let re = defaultLayout();
  re = openSession(re, "e1");
  const onlyPane = re.focusedPaneId;
  re = dropTabAtRootEdge(re, { surface: "terminal", sessionId: "e1" }, "left");
  ok(
    panes(re.root).length === 1 && re.focusedPaneId === onlyPane,
    "root-edge drop of the only pane's only tab is a no-op",
  );
  re = splitPane(re, onlyPane, "row");
  re = openSession(re, "e2");
  re = dropTabAtRootEdge(re, { surface: "terminal", sessionId: "e2" }, "bottom");
  ok(re.root.type === "split" && re.root.dir === "col", "root-edge drop splits the root");
  ok(panes(re.root).length === 2, "moving a pane's last tab away collapses the pane");
  const reBefore = re;
  re = dropTabAtRootEdge(re, { surface: "terminal", sessionId: "e2" }, "bottom");
  ok(re === reBefore, "re-dropping the edge pane's tab on the same edge is a no-op");

  // per-pane font size: set/clamp/reset, survives serialization
  let fz = defaultLayout();
  fz = setPaneFont(fz, fz.focusedPaneId, 15);
  ok(findPane(fz.root, fz.focusedPaneId)?.fontSize === 15, "font override sets");
  const fzRound = deserializeLayout(JSON.parse(JSON.stringify(serializeLayout(fz))));
  ok(
    fzRound !== null && findPane(fzRound.root, fzRound.focusedPaneId)?.fontSize === 15,
    "font override round-trips",
  );
  fz = setPaneFont(fz, fz.focusedPaneId, 100);
  ok(findPane(fz.root, fz.focusedPaneId)?.fontSize === FONT_MAX, "font clamps to bounds");
  fz = setPaneFont(fz, fz.focusedPaneId, undefined);
  ok(findPane(fz.root, fz.focusedPaneId)?.fontSize === undefined, "font resets to default");

  // adjacentPane: right neighbor first, null for a single pane
  let ap = defaultLayout();
  ok(adjacentPane(ap, ap.focusedPaneId) === null, "single pane has no neighbor");
  const apLeft = ap.focusedPaneId;
  ap = splitPane(ap, apLeft, "row");
  ok(adjacentPane(ap, apLeft) === ap.focusedPaneId, "right neighbor wins");
  ok(adjacentPane(ap, ap.focusedPaneId) === apLeft, "left neighbor is the fallback");

  // pruning dead sessions collapses emptied panes but never touches files
  let pr = defaultLayout();
  pr = openSession(pr, "s1");
  pr = openFile(pr, "/tmp/keep.md");
  pr = splitPane(pr, pr.focusedPaneId, "row");
  pr = openSession(pr, "s2");
  pr = pruneSessions(pr, new Set(["s1"]));
  ok(panes(pr.root).length === 1 && allSessionIds(pr).join() === "s1", "prune drops dead tabs and panes");
  ok(allFilePaths(pr).join() === "/tmp/keep.md", "session prune keeps file tabs");
  pr = pruneFiles(pr, new Set(["/tmp/keep.md"]));
  ok(allFilePaths(pr).length === 0, "file prune drops dead file tabs");
  ok(allSessionIds(pr).join() === "s1", "file prune keeps sessions");
}
