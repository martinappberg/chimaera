/**
 * The rail's two pointer drags: the FILES section divider and the sidebar
 * width handle. Both capture the pointer, throttle to one write per frame,
 * clamp to `railState.ts`'s bounds, and restore the starting value on Escape
 * or pointercancel. The values themselves stay App's `$state`; this module
 * reaches them only through the getters and setters App lends it.
 */

import { startResizeDrag, type ResizeDragEnv } from "../shared/resizeDrag";
import { FILES_FRAC_MAX, FILES_FRAC_MIN, RAIL_DEFAULT, RAIL_MAX, RAIL_MIN } from "./railState";

export interface RailResizeHost {
  getRailEl(): HTMLElement | null;
  getDaemonEl(): HTMLElement | null;
  getFilesFrac(): number;
  setFilesFrac(frac: number): void;
  setFilesDividerActive(active: boolean): void;
  getRailWidth(): number;
  setRailWidth(width: number): void;
  setRailDividerActive(active: boolean): void;
  /** Terminals hold their fits while the stage width moves (`pool.setDragging`). */
  setDragging(dragging: boolean): void;
  /** Test seam: the drag's listener target and frame scheduler. */
  env?: Partial<ResizeDragEnv>;
}

export interface RailResize {
  onFilesDividerDown(e: PointerEvent): void;
  onRailResizeDown(e: PointerEvent): void;
  resetRailWidth(): void;
}

export function createRailResize(host: RailResizeHost): RailResize {
  /**
   * FILES section resize: a quiet divider above the section header; drag
   * moves the boundary (fraction of the rail, clamped), Escape restores.
   */
  function onFilesDividerDown(e: PointerEvent): void {
    const railEl = host.getRailEl();
    const daemonEl = host.getDaemonEl();
    if (e.button !== 0 || railEl === null || daemonEl === null) return;
    const rail = railEl;
    const daemon = daemonEl;
    host.setFilesDividerActive(true);
    startResizeDrag(e, {
      axis: "y",
      initial: host.getFilesFrac(),
      compute: (y) => {
        const railH = rail.getBoundingClientRect().height;
        if (railH <= 0) return null;
        const h = daemon.getBoundingClientRect().top - y;
        return Math.min(Math.max(h / railH, FILES_FRAC_MIN), FILES_FRAC_MAX);
      },
      apply: (frac) => host.setFilesFrac(frac),
      onEnd: () => host.setFilesDividerActive(false),
      env: host.env,
    });
  }

  /**
   * Sidebar width resize: a quiet vertical handle on the rail's right edge.
   * Drag moves the boundary (clamped to [RAIL_MIN, RAIL_MAX]); Escape restores.
   * The stage grows/shrinks with the rail, so terminals are told to hold their
   * fits until the drag ends (setDragging) to avoid per-frame reflow jank.
   */
  function onRailResizeDown(e: PointerEvent): void {
    const railEl = host.getRailEl();
    if (e.button !== 0 || railEl === null) return;
    const startWidth = railEl.getBoundingClientRect().width;
    host.setRailDividerActive(true);
    host.setDragging(true);
    startResizeDrag(e, {
      axis: "x",
      initial: host.getRailWidth(),
      compute: (x, startX) => Math.min(Math.max(startWidth + (x - startX), RAIL_MIN), RAIL_MAX),
      apply: (width) => host.setRailWidth(width),
      onEnd: () => {
        host.setRailDividerActive(false);
        // Flush the fits deferred during the drag now that the width is settled.
        host.setDragging(false);
      },
      env: host.env,
    });
  }

  /** Double-click the width handle to snap back to the default sidebar width. */
  function resetRailWidth(): void {
    host.setRailWidth(RAIL_DEFAULT);
  }

  return { onFilesDividerDown, onRailResizeDown, resetRailWidth };
}
