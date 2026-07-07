/**
 * Window-scoped rail chrome (sidebar width, FILES section open/size). Unlike
 * the pane layout — which is per (window, workspace) and lives on the daemon —
 * the sidebar's own dimensions are a window preference that should hold across
 * workspace switches, so they persist locally (localStorage, keyed by the same
 * window id as the view-state). Collapse/hide is NOT stored here: it maps onto
 * the layout's focus mode, which already persists and carries the strip.
 *
 * Reads clamp to the same bounds the drag enforces, so a hand-edited or stale
 * value can never wedge the rail off-screen.
 */

import { windowKey } from "./viewState";

/** Draggable sidebar width bounds (px). */
export const RAIL_MIN = 190;
export const RAIL_MAX = 460;
export const RAIL_DEFAULT = 230;

/** FILES section share of the rail height (fraction), matching the divider clamp. */
export const FILES_FRAC_MIN = 0.12;
export const FILES_FRAC_MAX = 0.8;
export const FILES_FRAC_DEFAULT = 0.4;

export interface RailChrome {
  width: number;
  filesOpen: boolean;
  filesFrac: number;
}

function clamp(n: number, lo: number, hi: number): number {
  return Math.min(Math.max(n, lo), hi);
}

function storageKey(): string {
  return `chimaera.rail.${windowKey()}`;
}

/** The defaults, used on first run and whenever the store is unreadable. */
export function defaultRailChrome(): RailChrome {
  return { width: RAIL_DEFAULT, filesOpen: true, filesFrac: FILES_FRAC_DEFAULT };
}

/** Load this window's rail chrome, clamped; defaults for anything missing. */
export function loadRailChrome(): RailChrome {
  const base = defaultRailChrome();
  try {
    const raw = localStorage.getItem(storageKey());
    if (raw === null) return base;
    const p = JSON.parse(raw) as Partial<RailChrome>;
    return {
      width:
        typeof p.width === "number" && Number.isFinite(p.width)
          ? clamp(p.width, RAIL_MIN, RAIL_MAX)
          : base.width,
      filesOpen: typeof p.filesOpen === "boolean" ? p.filesOpen : base.filesOpen,
      filesFrac:
        typeof p.filesFrac === "number" && Number.isFinite(p.filesFrac)
          ? clamp(p.filesFrac, FILES_FRAC_MIN, FILES_FRAC_MAX)
          : base.filesFrac,
    };
  } catch {
    // private-mode / quota / malformed JSON — defaults hold
    return base;
  }
}

/** Persist this window's rail chrome (fire-and-forget; failures are harmless). */
export function saveRailChrome(chrome: RailChrome): void {
  try {
    localStorage.setItem(storageKey(), JSON.stringify(chrome));
  } catch {
    // storage unavailable; the width simply won't survive a reload
  }
}
