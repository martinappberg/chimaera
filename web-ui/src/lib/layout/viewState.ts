/**
 * Daemon-owned per-window view state, per the /api/v1/view-state contract:
 * GET  /view-state/{key} -> {"state": <JSON>} | 404
 * PUT  /view-state/{key} body: JSON <= 64KB -> 204
 * The window id lives in sessionStorage ("chimaera.win"), so a reload
 * restores this exact window's layout while a fresh tab starts clean.
 */

import { api } from "../net/api";

const WIN_KEY = "chimaera.win";
const DEBOUNCE_MS = 500;

let cachedId: string | null = null;

/** This window's stable view-state key (matches [A-Za-z0-9_-]{1,64}). */
export function windowKey(): string {
  if (cachedId !== null) return cachedId;
  let id = sessionStorage.getItem(WIN_KEY);
  if (id === null || !/^[A-Za-z0-9_-]{1,64}$/.test(id)) {
    id = crypto.randomUUID();
    sessionStorage.setItem(WIN_KEY, id);
  }
  cachedId = id;
  return id;
}

/** Stored blob for `key`, or null on 404 / invalid / unreachable daemon. */
export async function loadViewState(key: string): Promise<unknown> {
  try {
    const res = await api(`/view-state/${key}`);
    if (!res.ok) return null;
    const body = (await res.json()) as { state?: unknown };
    return body.state ?? null;
  } catch {
    return null;
  }
}

let timer: ReturnType<typeof setTimeout> | null = null;
// Pending writes keyed BY view-state key: a single flush window may target more
// than one key (e.g. the window-scoped key AND the workspace-scoped mirror), so
// a single slot would let the later call clobber the earlier one and only the
// last key would ever be persisted. Same-key rewrites still coalesce (Map.set).
const pending = new Map<string, unknown>();

/** Debounced PUT (500ms): coalesces divider drags into one write per key. */
export function saveViewState(key: string, state: unknown): void {
  pending.set(key, state);
  if (timer !== null) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = null;
    void flushViewState();
  }, DEBOUNCE_MS);
}

/** Send the pending writes now (used on pagehide so a close never loses state). */
export async function flushViewState(): Promise<void> {
  if (pending.size === 0) return;
  const writes = [...pending.entries()];
  pending.clear();
  if (timer !== null) {
    clearTimeout(timer);
    timer = null;
  }
  await Promise.all(
    writes.map(([key, state]) =>
      api(`/view-state/${key}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(state),
        keepalive: true,
      }).catch(() => {
        // daemon unreachable; the next layout change retries
      }),
    ),
  );
}
