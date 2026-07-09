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
let pending: { key: string; state: unknown } | null = null;

/** Debounced PUT (500ms): coalesces divider drags into one write. */
export function saveViewState(key: string, state: unknown): void {
  pending = { key, state };
  if (timer !== null) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = null;
    void flushViewState();
  }, DEBOUNCE_MS);
}

/** Send the pending write now (used on pagehide so a close never loses state). */
export async function flushViewState(): Promise<void> {
  if (pending === null) return;
  const { key, state } = pending;
  pending = null;
  if (timer !== null) {
    clearTimeout(timer);
    timer = null;
  }
  try {
    await api(`/view-state/${key}`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(state),
      keepalive: true,
    });
  } catch {
    // daemon unreachable; the next layout change retries
  }
}
