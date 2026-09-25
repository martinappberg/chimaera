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

/**
 * Where to land in a file. `line` (1-based, with optional `endLine`/`col`)
 * addresses text; the other fields address the non-text viewers, which ignore
 * `line` (pass 1) when they have their own anchor. Mirrors the locator
 * fragments in docs/document-workbench-plan.md.
 */
export interface Reveal {
  line: number;
  endLine?: number;
  col?: number;
  /** PDF page, 1-based (`#page=N`). */
  page?: number;
  /** Notebook cell, 1-based (`#cell=N`). */
  cell?: number;
  /** Slide, 1-based (`#slide=N`). */
  slide?: number;
  /** Media time range in seconds (`#t=start,end`). */
  time?: { start: number; end?: number };
  /** Pixel region of an image or PDF page (`#xywh=x,y,w,h`). */
  region?: { x: number; y: number; w: number; h: number };
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
