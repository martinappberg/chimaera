/**
 * Pointer-event drag & drop for the layout system (no HTML5 DnD — it cannot
 * hit 60fps and its ghosts are unstylable). Sources are pane tabs, the pane
 * bar's empty area (drags the active tab), rail rows, and file-tree entries;
 * targets are pane edge bands (split), pane centers (adopt as a tab), tab
 * bars (insertion index), and window edges (root split). A drag only becomes
 * active past a small movement threshold, so plain clicks keep working;
 * Escape cancels an active drag.
 */

import type { Side, SplitDir, Tab, Zone } from "./layout";

export type DropSpot =
  | { kind: "zone"; paneId: string; zone: Zone }
  | { kind: "tab"; paneId: string; index: number }
  | { kind: "edge"; edge: Side }
  /** The "@ reference" band over a session pane's bottom (file drags only). */
  | { kind: "ref"; paneId: string };

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
  /** Divider drag lifecycle — gates terminal refits. */
  dividerDrag(active: boolean): void;
  /** Split `paneId` (pane hover cluster; same as the mod+D chords). */
  splitPaneAt(paneId: string, dir: SplitDir): void;
  /** Toggle zoom on `paneId` (cluster button, zoom badge, tab dblclick). */
  zoomPane(paneId: string): void;
  /** Close the pane's active view; an empty pane collapses. */
  closeView(paneId: string): void;
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

/** The "@ reference" band covers the bottom ~22% of a session pane. */
export const REF_BAND_FRAC = 0.22;

/**
 * Hit-test priority: tab bars (precise insertion beats everything), then the
 * "@ reference" band (file drags over session panes — it owns the pane's
 * bottom, including the stage's bottom-edge strip there), then window edges,
 * then pane zones.
 */
function spotAt(
  x: number,
  y: number,
  refFor: ((paneId: string) => boolean) | null,
): DropSpot | null {
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
  if (refActive && paneHit !== null) {
    const ny = (y - paneHit.r.top) / paneHit.r.height;
    if (ny >= 1 - REF_BAND_FRAC) return { kind: "ref", paneId: paneHit.paneId };
  }
  const edge = windowEdgeAt(x, y);
  if (edge !== null) return edge;
  if (paneHit !== null) {
    const { paneId, r } = paneHit;
    const nx = (x - r.left) / r.width;
    const ny = (y - r.top) / r.height;
    let zone = zoneAt(nx, ny);
    // With the reference band active, the pane's bottom belongs to it — the
    // sliver between center and the band reads as center, never bottom-split.
    if (refActive && zone === "bottom") zone = "center";
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

/**
 * Start a potential drag from `e` (a pointerdown on the source element).
 * Captures the pointer on the source so terminals never see the moves.
 */
export function startDrag(e: PointerEvent, payload: DragPayload, cb: DragCallbacks): void {
  if (e.button !== 0) return;
  const source = e.currentTarget instanceof Element ? e.currentTarget : null;
  const pointerId = e.pointerId;
  const sx = e.clientX;
  const sy = e.clientY;
  let active = false;
  let ghost: HTMLDivElement | null = null;
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
    const next = spotAt(lastX, lastY, refFor);
    if (!sameSpot(next, spot)) {
      spot = next;
      cb.onSpot(spot);
    }
  };

  const onMove = (ev: PointerEvent) => {
    if (ev.pointerId !== pointerId) return;
    lastX = ev.clientX;
    lastY = ev.clientY;
    if (!active) {
      if (Math.hypot(lastX - sx, lastY - sy) < DRAG_THRESHOLD_PX) return;
      active = true;
      ghost = makeGhost(payload.label);
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
