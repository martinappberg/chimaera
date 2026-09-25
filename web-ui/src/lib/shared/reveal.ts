/**
 * "Open at this spot": a transient request that a file view scroll to and
 * flash a line range once it is showing `path`. Deliberately NOT tab/layout
 * state — a reveal is a one-shot gesture, never persisted, never part of tab
 * identity (so dedupe and the saved layout are untouched).
 *
 * Flow: an opener calls `requestReveal` and then opens (or focuses) the tab;
 * the view for that path calls `takeReveal(path)` on mount and whenever
 * `revealRequest` changes. Taking clears the request, so a view that remounts
 * later (keep-alive eviction, a pane split) never replays a stale jump.
 */

import { get, writable } from "svelte/store";

/** A 1-based line range (and optional 1-based column) within a text file. */
export interface Reveal {
  line: number;
  endLine?: number;
  col?: number;
}

export interface RevealRequest extends Reveal {
  /** Absolute path the reveal targets. */
  path: string;
  /** Monotonic, so an identical repeat request still re-triggers. */
  nonce: number;
}

/** The latest unconsumed reveal (null once taken). */
export const revealRequest = writable<RevealRequest | null>(null);

let nonce = 0;

/** Ask whichever view shows `path` to reveal `r` (the next one to mount, if none is). */
export function requestReveal(path: string, r: Reveal): void {
  nonce += 1;
  revealRequest.set({ ...r, path, nonce });
}

/** Consume the pending reveal for `path`, if there is one. */
export function takeReveal(path: string): RevealRequest | null {
  const req = get(revealRequest);
  if (req === null || req.path !== path) return null;
  revealRequest.set(null);
  return req;
}
