/**
 * Pointer-event drag & drop for the layout system (no HTML5 DnD — it cannot
 * hit 60fps and its ghosts are unstylable). Sources are pane tabs, the pane
 * bar's empty area (drags the active tab), rail rows, and file-tree entries;
 * targets are pane edge bands (split), pane centers (adopt as a tab), tab
 * bars (insertion index), and window edges (root split). A drag only becomes
 * active past a small movement threshold, so plain clicks keep working;
 * Escape cancels an active drag.
 */

import type { DiffMode } from "./git";
import type { Side, SplitDir, Tab, Zone } from "./layout";

export type DropSpot =
  | { kind: "zone"; paneId: string; zone: Zone }
  | { kind: "tab"; paneId: string; index: number }
  | { kind: "edge"; edge: Side }
  /** The "@ reference" band over a session pane's bottom (file drags only). */
  | { kind: "ref"; paneId: string }
  /** The "link to agent" band over an agent pane's input area — a plain
   *  shell-terminal TAB drag (not link-intent); see startDrag's linkTargets. */
  | { kind: "link"; paneId: string }
  /** A whole agent pane, lit end-to-end while a LINK-INTENT drag (from the
   *  link icon) hovers anywhere inside it. */
  | { kind: "linkpane"; paneId: string; sessionId: string }
  /** An agent's TAB, lit while a link-intent drag hovers it — links to that
   *  agent even when it isn't the pane's active view. */
  | { kind: "linktab"; paneId: string; index: number; sessionId: string }
  /** An agent's rail row, highlighted while a shell terminal is dragged over
   *  it — the always-present link target (the agent needn't be open in a
   *  pane). See startDrag's linkSessions. */
  | { kind: "linkrow"; sessionId: string };

export interface DragPayload {
  /** The surface being dragged (terminal session or file preview). */
  tab: Tab;
  label: string;
}

/** Layout mutations the pane tree invokes; implemented by App. */
export interface LayoutCtrl {
  focusPane(paneId: string): void;
  activateTab(paneId: string, index: number): void;
  closeTab(paneId: string, index: number): void;
  setRatio(splitId: string, ratio: number): void;
  /** Begin a pointer drag of a tab (click-through handled by the drag). */
  dragTab(e: PointerEvent, paneId: string, index: number, tab: Tab): void;
  /**
   * Begin a pointer drag of an arbitrary surface with a custom click action —
   * the link icon uses this to drag its own terminal onto an agent (drop =
   * link) while a plain click still opens the link menu.
   */
  dragSurface(e: PointerEvent, tab: Tab, onClick: () => void): void;
  /** Divider drag lifecycle — gates terminal refits. */
  dividerDrag(active: boolean): void;
  /** Split `paneId` (pane hover cluster; same as the mod+D chords). */
  splitPaneAt(paneId: string, dir: SplitDir): void;
  /** Toggle zoom on `paneId` (cluster button, zoom badge, tab dblclick). */
  zoomPane(paneId: string): void;
  /** Close the pane's active view; an empty pane collapses. */
  closeView(paneId: string): void;
  /**
   * Open a file surfaced FROM `paneId` (terminal path link, touched-files
   * popover): lands in the adjacent pane, or a fresh split when the window
   * has one pane / `newSplit` (Cmd/Ctrl) is set.
   */
  openFileFrom(paneId: string, path: string, newSplit: boolean): void;
  /** Persist a Finder instance's current directory (its navigation state). */
  navigateFinder(id: string, path: string): void;
  /**
   * Open a side-by-side diff surfaced FROM `paneId` (a changes-panel row):
   * same adjacent-pane / fresh-split grammar as `openFileFrom`, so the panel
   * stays visible beside the diff it opened.
   */
  openDiffFrom(paneId: string, path: string, mode: DiffMode, newSplit: boolean): void;
  /**
   * Step the pane's terminal font size (`delta` +1/-1), or reset to the
   * default (`delta` 0). Same action as the Cmd/Ctrl +/−/0 chords.
   */
  adjustFont(paneId: string, delta: 1 | -1 | 0): void;
}

interface PaneReg {
  root: HTMLElement;
  content: HTMLElement;
  tabbar: HTMLElement | null;
}

const paneRegs = new Map<string, PaneReg>();

/** The stage element (the pane-tree viewport); its edges are drop targets. */
let stageEl: HTMLElement | null = null;

export function registerStage(el: HTMLElement): void {
  stageEl = el;
}

export function unregisterStage(el: HTMLElement): void {
  if (stageEl === el) stageEl = null;
}

export function registerPane(paneId: string, reg: PaneReg): void {
  paneRegs.set(paneId, reg);
}

export function unregisterPane(paneId: string, root: HTMLElement): void {
  // Guard against a re-registration racing the old registration's cleanup.
  if (paneRegs.get(paneId)?.root === root) paneRegs.delete(paneId);
}

/** Agent rail rows, registered so a shell-terminal drag can drop on one to
 *  link (the always-present target — the agent needn't be open in a pane). */
const linkRowRegs = new Map<string, HTMLElement>();

export function registerLinkRow(sessionId: string, el: HTMLElement): void {
  linkRowRegs.set(sessionId, el);
}

export function unregisterLinkRow(sessionId: string, el: HTMLElement): void {
  if (linkRowRegs.get(sessionId) === el) linkRowRegs.delete(sessionId);
}

export function paneContentEl(paneId: string): HTMLElement | null {
  return paneRegs.get(paneId)?.content ?? null;
}

/** The pane's root element (focus target for freshly split empty panes). */
export function paneRootEl(paneId: string): HTMLElement | null {
  return paneRegs.get(paneId)?.root ?? null;
}

export interface DragCallbacks {
  /** Current drop spot under the pointer (null when over nothing). */
  onSpot(spot: DropSpot | null): void;
  onDrop(spot: DropSpot): void;
  /** Pointer released before the drag threshold — treat as a plain click. */
  onClick(): void;
  /** Always fired last (drop, click, or cancel). */
  onEnd(): void;
  /**
   * Context bridge: true when `paneId` currently shows a live session, so a
   * FILE drag over it grows the "@ reference" band along its bottom. Omitted
   * (or false) for non-file drags — the band never appears for tab moves.
   */
  acceptsRef?(paneId: string): boolean;
}

const DRAG_THRESHOLD_PX = 4;

function sameSpot(a: DropSpot | null, b: DropSpot | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.kind !== b.kind) return false;
  if (a.kind === "edge" && b.kind === "edge") return a.edge === b.edge;
  if (a.kind === "tab" && b.kind === "tab") return a.paneId === b.paneId && a.index === b.index;
  if (a.kind === "zone" && b.kind === "zone") return a.paneId === b.paneId && a.zone === b.zone;
  if (a.kind === "ref" && b.kind === "ref") return a.paneId === b.paneId;
  if (a.kind === "link" && b.kind === "link") return a.paneId === b.paneId;
  if (a.kind === "linkpane" && b.kind === "linkpane") return a.paneId === b.paneId;
  if (a.kind === "linktab" && b.kind === "linktab")
    return a.paneId === b.paneId && a.index === b.index;
  if (a.kind === "linkrow" && b.kind === "linkrow") return a.sessionId === b.sessionId;
  return false;
}

/** Edge bands are ~25% of the pane; the middle half-by-half is "center". */
function zoneAt(nx: number, ny: number): Zone {
  if (nx > 0.25 && nx < 0.75 && ny > 0.25 && ny < 0.75) return "center";
  const edges: [Zone, number][] = [
    ["left", nx],
    ["right", 1 - nx],
    ["top", ny],
    ["bottom", 1 - ny],
  ];
  edges.sort((p, q) => p[1] - q[1]);
  return edges[0][0];
}

/** Pointer within this many px of the stage boundary targets a window edge. */
const WINDOW_EDGE_PX = 16;

function windowEdgeAt(x: number, y: number): DropSpot | null {
  if (stageEl === null) return null;
  const r = stageEl.getBoundingClientRect();
  if (r.width === 0 || x < r.left || x > r.right || y < r.top || y > r.bottom) return null;
  const d: [Side, number][] = [
    ["left", x - r.left],
    ["right", r.right - x],
    ["top", y - r.top],
    ["bottom", r.bottom - y],
  ];
  d.sort((p, q) => p[1] - q[1]);
  return d[0][1] <= WINDOW_EDGE_PX ? { kind: "edge", edge: d[0][0] } : null;
}

/** The "@ reference" / "link to agent" bands cover the bottom ~22% of a
 *  pane (one geometry, two payloads: files reference, terminals link). */
export const REF_BAND_FRAC = 0.22;

/**
 * Link-intent hit-test (a drag from a pane's link icon): the WHOLE agent view
 * is the target. In priority: an agent's rail row, then an agent's tab (links
 * to that agent even when it isn't the active view), then anywhere inside an
 * agent pane. Over anything else — a non-agent pane, empty space — nothing
 * responds, so the gesture only ever links.
 */
function linkSpotAt(
  x: number,
  y: number,
  linkTargets: ReadonlyMap<string, string> | undefined,
  linkSessions: ReadonlySet<string> | undefined,
): DropSpot | null {
  if (linkSessions !== undefined) {
    for (const [sid, el] of linkRowRegs) {
      if (!linkSessions.has(sid)) continue;
      const r = el.getBoundingClientRect();
      if (r.width > 0 && x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) {
        return { kind: "linkrow", sessionId: sid };
      }
    }
  }
  for (const [paneId, reg] of paneRegs) {
    const r = reg.root.getBoundingClientRect();
    if (r.width === 0 || x < r.left || x > r.right || y < r.top || y > r.bottom) continue;
    // An agent's tab links to THAT agent, active view or not.
    if (reg.tabbar !== null) {
      const tr = reg.tabbar.getBoundingClientRect();
      if (y >= tr.top && y <= tr.bottom) {
        for (const t of reg.tabbar.querySelectorAll<HTMLElement>("[data-tab-index]")) {
          const tb = t.getBoundingClientRect();
          if (x >= tb.left && x <= tb.right) {
            const agentId = t.dataset.linkAgent;
            if (agentId != null && agentId.length > 0) {
              return { kind: "linktab", paneId, index: Number(t.dataset.tabIndex), sessionId: agentId };
            }
            break; // a non-agent tab: fall through to the whole-pane check
          }
        }
      }
    }
    const agentId = linkTargets?.get(paneId);
    if (agentId !== undefined) return { kind: "linkpane", paneId, sessionId: agentId };
    return null; // a non-agent pane during a link drag: no-op
  }
  return null;
}

/**
 * Hit-test priority: tab bars (precise insertion beats everything), then the
 * bottom band when armed — "@ reference" for file drags over session panes,
 * "link to agent" for shell-terminal drags over agent panes; either owns the
 * pane's bottom, including the stage's bottom-edge strip there — then window
 * edges, then pane zones.
 */
function spotAt(
  x: number,
  y: number,
  refFor: ((paneId: string) => boolean) | null,
  linkTargets: ReadonlyMap<string, string> | undefined,
  linkSessions: ReadonlySet<string> | undefined,
  linkIntent: boolean,
): DropSpot | null {
  // A link-intent drag is link-only: the whole agent view is the target and
  // nothing else responds (never a tab move / tile).
  if (linkIntent) return linkSpotAt(x, y, linkTargets, linkSessions);

  let paneHit: { paneId: string; r: DOMRect } | null = null;
  for (const [paneId, reg] of paneRegs) {
    const r = reg.root.getBoundingClientRect();
    if (r.width === 0 || x < r.left || x > r.right || y < r.top || y > r.bottom) continue;
    if (reg.tabbar !== null) {
      const tr = reg.tabbar.getBoundingClientRect();
      if (y >= tr.top && y <= tr.bottom) {
        const tabs = reg.tabbar.querySelectorAll<HTMLElement>("[data-tab-index]");
        let index = tabs.length;
        for (const t of tabs) {
          const tb = t.getBoundingClientRect();
          if (x < tb.left + tb.width / 2) {
            index = Number(t.dataset.tabIndex);
            break;
          }
        }
        return { kind: "tab", paneId, index };
      }
    }
    paneHit = { paneId, r };
    break;
  }
  const refActive = paneHit !== null && refFor !== null && refFor(paneHit.paneId);
  const linkActive = paneHit !== null && linkTargets?.has(paneHit.paneId) === true;
  if (paneHit !== null && (refActive || linkActive)) {
    const ny = (y - paneHit.r.top) / paneHit.r.height;
    if (ny >= 1 - REF_BAND_FRAC) {
      // Mutually exclusive by payload: ref only arms for file drags, link
      // only for shell-terminal drags (see startDrag / DragOptions).
      return refActive
        ? { kind: "ref", paneId: paneHit.paneId }
        : { kind: "link", paneId: paneHit.paneId };
    }
  }
  const edge = windowEdgeAt(x, y);
  if (edge !== null) return edge;
  if (paneHit !== null) {
    const { paneId, r } = paneHit;
    const nx = (x - r.left) / r.width;
    const ny = (y - r.top) / r.height;
    let zone = zoneAt(nx, ny);
    // With a bottom band active, the pane's bottom belongs to it — the
    // sliver between center and the band reads as center, never bottom-split.
    if ((refActive || linkActive) && zone === "bottom") zone = "center";
    return { kind: "zone", paneId, zone };
  }
  return null;
}

function makeGhost(label: string): HTMLDivElement {
  const ghost = document.createElement("div");
  ghost.className = "drag-ghost";
  ghost.textContent = label;
  document.body.appendChild(ghost);
  return ghost;
}

const SVG_NS = "http://www.w3.org/2000/svg";

interface Leash {
  svg: SVGSVGElement;
  line: SVGPathElement;
  start: SVGCircleElement;
  end: SVGCircleElement;
}

/** A body-level SVG overlay: the leash chain (line + a node at each end). */
function makeLeash(): Leash {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("class", "leash-overlay");
  const line = document.createElementNS(SVG_NS, "path");
  line.setAttribute("class", "leash-line");
  const start = document.createElementNS(SVG_NS, "circle");
  start.setAttribute("class", "leash-node start");
  start.setAttribute("r", "3.5");
  const end = document.createElementNS(SVG_NS, "circle");
  end.setAttribute("class", "leash-node end");
  end.setAttribute("r", "5");
  svg.append(line, start, end);
  document.body.appendChild(svg);
  return { svg, line, start, end };
}

/** The point a leash should snap to for a spot (a link target's center), or
 *  null when the spot isn't a link target (the leash then trails the pointer). */
function leashAnchor(spot: DropSpot | null): { x: number; y: number } | null {
  if (spot === null) return null;
  if (spot.kind === "linkrow") {
    const el = linkRowRegs.get(spot.sessionId);
    if (el !== undefined) {
      const r = el.getBoundingClientRect();
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    }
  }
  if (spot.kind === "link") {
    const reg = paneRegs.get(spot.paneId);
    if (reg !== undefined) {
      const r = reg.content.getBoundingClientRect();
      // The band lives on the pane's lower ~22%; aim the leash into it.
      return { x: r.left + r.width / 2, y: r.bottom - r.height * 0.11 };
    }
  }
  if (spot.kind === "linkpane") {
    const reg = paneRegs.get(spot.paneId);
    if (reg !== undefined) {
      const r = reg.root.getBoundingClientRect();
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    }
  }
  if (spot.kind === "linktab") {
    const tab = paneRegs
      .get(spot.paneId)
      ?.tabbar?.querySelector<HTMLElement>(`[data-tab-index="${spot.index}"]`);
    if (tab != null) {
      const r = tab.getBoundingClientRect();
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    }
  }
  return null;
}

/**
 * Draw the leash from the grab origin to the pointer, snapping its far end to
 * the hovered link target's center. A gentle downward sag makes it read as a
 * hanging chain; over a target it goes taut (the `snapped` class solidifies it).
 */
function drawLeash(
  l: Leash,
  sx: number,
  sy: number,
  px: number,
  py: number,
  spot: DropSpot | null,
): void {
  const anchor = leashAnchor(spot);
  const ex = anchor?.x ?? px;
  const ey = anchor?.y ?? py;
  const sag = Math.min(Math.max(Math.hypot(ex - sx, ey - sy) * 0.16, 6), 46);
  const cx = (sx + ex) / 2;
  const cy = (sy + ey) / 2 + sag;
  l.line.setAttribute("d", `M ${sx} ${sy} Q ${cx} ${cy} ${ex} ${ey}`);
  l.start.setAttribute("cx", `${sx}`);
  l.start.setAttribute("cy", `${sy}`);
  l.end.setAttribute("cx", `${ex}`);
  l.end.setAttribute("cy", `${ey}`);
  l.svg.classList.toggle("snapped", anchor !== null);
}

export interface DragOptions {
  /** Agent panes (paneId → the agent session shown there) whose input band is
   *  a "link to agent" target for a plain shell-terminal TAB drag. */
  linkTargets?: ReadonlyMap<string, string>;
  /** Agent session ids whose rail rows are "link to agent" targets for this
   *  payload (a shell terminal). Independent of whether the agent is open. */
  linkSessions?: ReadonlySet<string>;
  /**
   * This drag started from a pane's link icon: it exists only to link. It
   * draws the leash chain and makes the WHOLE agent view a target — any part
   * of an agent pane, an agent's tab, or an agent's rail row — never a tab
   * move. (A plain tab drag keeps the precise band via linkTargets instead.)
   */
  linkIntent?: boolean;
}

/**
 * Start a potential drag from `e` (a pointerdown on the source element).
 * Captures the pointer on the source so terminals never see the moves.
 */
export function startDrag(
  e: PointerEvent,
  payload: DragPayload,
  cb: DragCallbacks,
  opts: DragOptions = {},
): void {
  if (e.button !== 0) return;
  const source = e.currentTarget instanceof Element ? e.currentTarget : null;
  const pointerId = e.pointerId;
  const sx = e.clientX;
  const sy = e.clientY;
  let active = false;
  let ghost: HTMLDivElement | null = null;
  let leash: Leash | null = null;
  let raf = 0;
  let lastX = sx;
  let lastY = sy;
  let spot: DropSpot | null = null;
  let done = false;

  try {
    source?.setPointerCapture(pointerId);
  } catch {
    // capture can fail if the pointer is already gone; drag still works
  }

  // The reference band only exists for FILE drags (the payload is the gate;
  // the callback decides per-pane whether a live session sits there).
  const refFor = payload.tab.surface === "file" ? (cb.acceptsRef?.bind(cb) ?? null) : null;

  const update = () => {
    raf = 0;
    if (ghost !== null) {
      ghost.style.transform = `translate(${lastX + 14}px, ${lastY + 10}px)`;
    }
    const next = spotAt(
      lastX,
      lastY,
      refFor,
      opts.linkTargets,
      opts.linkSessions,
      opts.linkIntent === true,
    );
    if (!sameSpot(next, spot)) {
      spot = next;
      cb.onSpot(spot);
    }
    if (leash !== null) drawLeash(leash, sx, sy, lastX, lastY, spot);
  };

  const onMove = (ev: PointerEvent) => {
    if (ev.pointerId !== pointerId) return;
    lastX = ev.clientX;
    lastY = ev.clientY;
    if (!active) {
      if (Math.hypot(lastX - sx, lastY - sy) < DRAG_THRESHOLD_PX) return;
      active = true;
      ghost = makeGhost(payload.label);
      if (opts.linkIntent === true) leash = makeLeash();
      document.body.classList.add("dragging");
    }
    if (raf === 0) raf = requestAnimationFrame(update);
  };

  const finish = (commit: boolean) => {
    if (done) return;
    done = true;
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onCancel);
    window.removeEventListener("keydown", onKey, true);
    if (raf !== 0) cancelAnimationFrame(raf);
    ghost?.remove();
    leash?.svg.remove();
    document.body.classList.remove("dragging");
    try {
      source?.releasePointerCapture(pointerId);
    } catch {
      // already released
    }
    if (commit && active && spot !== null) {
      cb.onDrop(spot);
    } else if (commit && !active) {
      cb.onClick();
    }
    cb.onSpot(null);
    cb.onEnd();
  };

  const onUp = (ev: PointerEvent) => {
    if (ev.pointerId === pointerId) finish(true);
  };
  const onCancel = (ev: PointerEvent) => {
    if (ev.pointerId === pointerId) finish(false);
  };
  const onKey = (ev: KeyboardEvent) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      ev.stopPropagation();
      finish(false);
    }
  };

  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onCancel);
  window.addEventListener("keydown", onKey, true);
}
