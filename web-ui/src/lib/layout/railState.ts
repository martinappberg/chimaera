/**
 * Window-scoped rail chrome (sidebar width, FILES section open/size). Unlike
 * the pane layout — which is per (window, workspace) and lives on the daemon —
 * the sidebar's own dimensions are a window preference that should hold across
 * workspace switches, so they persist locally (localStorage, keyed by the same
 * window id as the view-state). Collapse/hide is NOT stored here: it maps onto
 * the layout's focus mode, which already persists and carries the strip.
 *
 * Reads clamp to the same bounds the drag enforces, so a hand-edited or stale
 * value can never wedge the rail off-screen. Writes keep only the most
 * recently saved windows' records (the key is per tab, and tabs are cheap).
 */

import { windowKey } from "./viewState";

const STORAGE_PREFIX = "chimaera.rail.";
/** How many windows' records to keep. Every browser tab mints its own window
 *  key, so without a bound each tab ever opened leaves a key behind for good.
 *  Newest by write stamp win; a record without one (pre-stamp) counts as
 *  oldest. */
const MAX_STORED_WINDOWS = 16;

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

/** The persisted shape: the chrome plus the write stamp eviction orders by. */
interface StoredRailChrome extends RailChrome {
  savedAt?: number;
}

function storageKey(): string {
  return `${STORAGE_PREFIX}${windowKey()}`;
}

function storedKeys(): string[] {
  const keys: string[] = [];
  for (let i = 0; i < localStorage.length; i++) {
    const k = localStorage.key(i);
    if (k !== null && k.startsWith(STORAGE_PREFIX)) keys.push(k);
  }
  return keys;
}

/** A record's write stamp; malformed or unstamped reads as oldest. */
function savedAtOf(key: string): number {
  try {
    const p = JSON.parse(localStorage.getItem(key) ?? "null") as Partial<StoredRailChrome> | null;
    return p !== null && typeof p.savedAt === "number" && Number.isFinite(p.savedAt)
      ? p.savedAt
      : 0;
  } catch {
    return 0;
  }
}

/** Evict other windows' records past `keepOthers`, oldest write first. */
function pruneStoredWindows(keep: string, keepOthers = MAX_STORED_WINDOWS - 1): void {
  const others = storedKeys().filter((k) => k !== keep);
  const excess = others.length - keepOthers;
  if (excess <= 0) return;
  const byAge = others.map((k) => ({ k, at: savedAtOf(k) })).sort((a, b) => a.at - b.at);
  for (const { k } of byAge.slice(0, excess)) localStorage.removeItem(k);
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

/** Persist this window's rail chrome (fire-and-forget; failures are harmless),
 *  shedding the oldest other windows' records past the bound first. */
export function saveRailChrome(chrome: RailChrome): void {
  try {
    const key = storageKey();
    const record: StoredRailChrome = { ...chrome, savedAt: Date.now() };
    const json = JSON.stringify(record);
    // Prune BEFORE the write: a full store throws on setItem, so a prune that
    // only followed a successful write would be skipped exactly when quota is
    // the problem.
    pruneStoredWindows(key);
    try {
      localStorage.setItem(key, json);
    } catch {
      // Quota (ours or a foreign key's): shed every other window's record —
      // a rail width is a cheap preference to lose — and try once more.
      pruneStoredWindows(key, 0);
      localStorage.setItem(key, json);
    }
  } catch {
    // storage unavailable; the width simply won't survive a reload
  }
}
