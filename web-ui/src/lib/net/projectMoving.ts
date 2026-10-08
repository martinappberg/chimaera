/**
 * A browser view of a project while the project moves between the user's
 * computer and their cloud. Nothing in the view is usable then (whatever
 * would answer is on its way), so the view says so once, over everything,
 * and clears as soon as the machine that holds the project serves it.
 *
 * Driven only by the view's own placement reads (every action and socket
 * authentication already makes one; nothing here polls) and by its events
 * socket answering. A placement read says the project is moving only in a
 * project view (`/workspace/{id}/`) of a project the account carries
 * between machines, so a native window or the free app never sets this.
 */
import { writable, type Readable } from "svelte/store";
import { gatewayWorkspace } from "./base";

/** Where a moving project is going, and whether the cloud has taken it and
 *  is setting it up (`arriving`: it holds it but does not serve it yet). */
export interface ProjectMoving {
  to: "cloud" | "computer";
  arriving: boolean;
  /** When this view first saw the move (ms since the epoch). */
  since: number;
}

type Place = "cloud" | "computer";

/** A move that has not finished by now is not one this view can say anything
 *  about: the account's own deadline brings a hand-over back within three
 *  minutes, so past this the view shows its ordinary connection state. */
export const MOVING_MAX_MS = 5 * 60_000;

/** The one sentence the view shows over everything while its project moves. */
export function movingSentence(moving: ProjectMoving): string {
  if (moving.to === "computer") return "Coming back to your computer…";
  return moving.arriving ? "Starting in your cloud…" : "Moving to your cloud…";
}

/** The place a routed placement row names, from its route kind. */
function placeOf(route: string | null): Place | null {
  if (route?.startsWith("worker-")) return "cloud";
  if (route?.startsWith("device-")) return "computer";
  return null;
}

/**
 * The next state from one placement read. `before` is where the latest
 * routed read put the project (null before any). Pure, for tests.
 *
 * - Nobody holds it (`unowned`, a clean release) after it ran somewhere:
 *   it is on its way to the other place.
 * - Held by the place it was going to: it is arriving there.
 * - Held by the place it was leaving: the move did not happen; nothing to say.
 * - Held by a different place than the last read, with no release seen in
 *   between: it moved there and is arriving.
 * - Anything else (expired, privacy off, a failed read) changes nothing.
 */
export function nextMoving(
  now: ProjectMoving | null,
  before: Place | null,
  row: { availability: string; route_host_id: string | null },
  at: number,
): ProjectMoving | null {
  if (row.availability === "unowned") {
    if (now !== null) return now;
    return before === null ? null : { to: before === "cloud" ? "computer" : "cloud", arriving: false, since: at };
  }
  if (row.availability !== "owned" && row.availability !== "suspended") return now;
  const place = placeOf(row.route_host_id);
  if (place === null) return now;
  if (now !== null) {
    if (now.to !== place) return null;
    return now.arriving ? now : { ...now, arriving: true };
  }
  if (before !== null && before !== place) return { to: place, arriving: true, since: at };
  return null;
}

const store = writable<ProjectMoving | null>(null);
/** The current move of this project view's project, or null. */
export const projectMoving: Readable<ProjectMoving | null> = { subscribe: store.subscribe };

let current: ProjectMoving | null = null;
let cap: ReturnType<typeof setTimeout> | null = null;

function set(next: ProjectMoving | null): void {
  if (next === current) return;
  current = next;
  store.set(next);
  if (cap !== null) { clearTimeout(cap); cap = null; }
  if (next !== null) {
    const left = next.since + MOVING_MAX_MS - Date.now();
    if (left <= 0) { current = null; store.set(null); return; }
    cap = setTimeout(() => { cap = null; set(null); }, left);
  }
}

/** One placement read of this project view (from `placement.ts`). */
export function noteMovingRead(before: Place | null, row: { availability: string; route_host_id: string | null }): void {
  if (gatewayWorkspace() === null) return;
  set(nextMoving(current, before, row, Date.now()));
}

/** The machine holding the project answered this view (its events socket
 *  delivered a snapshot): whatever was moving has arrived. */
export function noteServed(): void {
  if (current !== null) set(null);
  forget();
}

/* The view reloads once when its project's machine changes (placement.ts),
 * which would lose the move mid-way: the state rides that one reload in this
 * tab's session storage, keyed by the project, and is dropped once served. */
const KEY = "chimaera.projectMoving";

/** Before the one reload that follows the project to its new machine. */
export function carryMoving(): void {
  const workspace = gatewayWorkspace();
  if (workspace === null || current === null) return;
  try { sessionStorage.setItem(KEY, JSON.stringify({ workspace, ...current })); } catch { /* storage refused: the view just reloads */ }
}

function forget(): void {
  try { sessionStorage.removeItem(KEY); } catch { /* nothing kept */ }
}

/** The carried state, if it belongs to this project view and is still young. */
export function restoreMoving(raw: string | null, workspace: string | null, at: number): ProjectMoving | null {
  if (raw === null || workspace === null) return null;
  try {
    const value = JSON.parse(raw) as Record<string, unknown>;
    if (value.workspace !== workspace || (value.to !== "cloud" && value.to !== "computer")) return null;
    if (typeof value.since !== "number" || !Number.isFinite(value.since) || value.since > at || at - value.since >= MOVING_MAX_MS) return null;
    return { to: value.to, arriving: value.arriving === true, since: value.since };
  } catch { return null; }
}

{
  const workspace = typeof location === "undefined" ? null : gatewayWorkspace();
  if (workspace !== null) {
    let raw: string | null = null;
    try { raw = sessionStorage.getItem(KEY); } catch { /* none */ }
    const carried = restoreMoving(raw, workspace, Date.now());
    if (carried !== null) set(carried); else if (raw !== null) forget();
  }
}
