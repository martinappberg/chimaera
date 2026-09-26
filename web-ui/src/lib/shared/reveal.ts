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
  /** Region of an image (natural pixels) or a PDF page (PDF points at 100%,
   *  from the page's top-left): `#xywh=x,y,w,h`. `percent` (`#xywh=percent:…`)
   *  means the four numbers are percentages of the image or page. */
  region?: { x: number; y: number; w: number; h: number; percent?: boolean };
  /** A block of a CSV/TSV (`#row=5-9`, `#col=2`, `#cell=5,2-9,4`, RFC 7111
   *  syntax): 1-based DATA rows (the header line is not counted, matching
   *  the grid's row numbers) and 1-based columns. An absent side spans
   *  everything; an end is only present when it differs from the start. */
  table?: { row?: number; endRow?: number; col?: number; endCol?: number };
  /** A spreadsheet sheet (`#sheet=Summary`). */
  sheet?: string;
  /** A spreadsheet A1 range (`#range=B2:F9`), 1-based SHEET coordinates
   *  (row 1 is the sheet's first row, whatever the viewer shows as header). */
  range?: { row?: number; col?: number; endRow?: number; endCol?: number };
}

/** The non-text part of a reveal: what a `#fragment` other than `#L…` names. */
export type Locator = Omit<Reveal, "line" | "endLine" | "col">;

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
