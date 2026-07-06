/**
 * Pointer-event drag & drop for the layout system (no HTML5 DnD — it cannot
 * hit 60fps and its ghosts are unstylable). Sources are rail rows and pane
 * tabs; targets are pane edge zones (left/right/top/bottom/center) and tab
 * bars (insertion index). A drag only becomes active past a small movement
 * threshold, so plain clicks keep working; Escape cancels an active drag.
 */

import type { SplitDir, Tab, Zone } from "./layout";

export type DropSpot =
  | { kind: "zone"; paneId: string; zone: Zone }
  | { kind: "tab"; paneId: string; index: number };

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
}

const DRAG_THRESHOLD_PX = 4;

function sameSpot(a: DropSpot | null, b: DropSpot | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.kind !== b.kind || a.paneId !== b.paneId) return false;
  if (a.kind === "tab" && b.kind === "tab") return a.index === b.index;
  if (a.kind === "zone" && b.kind === "zone") return a.zone === b.zone;
  return false;
}

function zoneAt(nx: number, ny: number): Zone {
  if (nx > 0.28 && nx < 0.72 && ny > 0.28 && ny < 0.72) return "center";
  const edges: [Zone, number][] = [
    ["left", nx],
    ["right", 1 - nx],
    ["top", ny],
    ["bottom", 1 - ny],
  ];
  edges.sort((p, q) => p[1] - q[1]);
  return edges[0][0];
}

function spotAt(x: number, y: number): DropSpot | null {
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
    const nx = (x - r.left) / r.width;
    const ny = (y - r.top) / r.height;
    return { kind: "zone", paneId, zone: zoneAt(nx, ny) };
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

  const update = () => {
    raf = 0;
    if (ghost !== null) {
      ghost.style.transform = `translate(${lastX + 14}px, ${lastY + 10}px)`;
    }
    const next = spotAt(lastX, lastY);
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
