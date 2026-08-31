/** One history page and the maximum steady-state transcript DOM window. */
export const TRANSCRIPT_PAGE = 64;
export const TRANSCRIPT_WINDOW = TRANSCRIPT_PAGE * 3;

/**
 * All ranges here are the reducer's ARRAY coordinates. Virtual coordinates
 * (array index + the store's `trimmedCount` at save time — a position among
 * every block ever appended) exist only in SAVED cursors, where the cap's
 * front-splices can't move them; {@link restoreVirtualWindow} converts back
 * at the restore boundary, and {@link trimShift} keeps a mounted view's array
 * range naming the same rows across a trim.
 */
export interface TranscriptWindow {
  start: number;
  end: number;
}

function boundedTotal(total: number): number {
  return Math.max(0, Math.floor(total));
}

/** The newest page shown after initial hydration or a jump to live activity. */
export function tailWindow(total: number): TranscriptWindow {
  const end = boundedTotal(total);
  return { start: Math.max(0, end - TRANSCRIPT_PAGE), end };
}

/**
 * Keep an already-rendered live tail current without snapping back to a
 * one-page window for every event. New blocks append at the bottom; only the
 * oldest DOM is discarded once the steady-state ceiling is reached.
 */
export function advanceTailWindow(
  current: TranscriptWindow,
  total: number,
): TranscriptWindow {
  const end = boundedTotal(total);
  const start = Math.max(0, Math.min(Math.floor(current.start), end));
  return { start: Math.max(start, end - TRANSCRIPT_WINDOW), end };
}

/**
 * Repair a persisted absolute range against a reducer that may have compacted
 * while the view was unmounted. An entirely stale range falls back to a window
 * ending at the current tail instead of restoring an empty transcript.
 */
export function restoreWindow(saved: TranscriptWindow, total: number): TranscriptWindow {
  const bounded = boundedTotal(total);
  const width = Math.min(
    TRANSCRIPT_WINDOW,
    Math.max(0, Math.floor(saved.end) - Math.floor(saved.start)),
  );
  const end = Math.max(0, Math.min(Math.floor(saved.end), bounded));
  let start = Math.max(0, Math.min(Math.floor(saved.start), end));
  if (start === end && width > 0) start = Math.max(0, end - width);
  if (end - start > TRANSCRIPT_WINDOW) start = end - TRANSCRIPT_WINDOW;
  return { start, end };
}

/**
 * Convert a window saved in virtual coordinates back to array coordinates
 * against the reducer's current trim count and length. Null when nothing of
 * the saved range survives — every row it covered was trimmed away, or a
 * reset/rewind left it beyond the transcript — so the caller falls back to
 * the tail rather than restoring an empty window.
 */
export function toArrayCoords(
  saved: TranscriptWindow,
  trimmedCount: number,
  total: number,
): TranscriptWindow | null {
  const bounded = boundedTotal(total);
  const end = Math.min(Math.floor(saved.end) - Math.floor(trimmedCount), bounded);
  const start = Math.min(Math.max(0, Math.floor(saved.start) - Math.floor(trimmedCount)), bounded);
  if (end <= start) return null;
  return { start, end };
}

/**
 * The ONE stale-cursor policy for restoring a pool-saved virtual window:
 * convert, repair against the current total, and floor the result to a full
 * page — a window straddling the trim point can survive as a 1-2 row sliver,
 * which the no-IntersectionObserver fallback path could never page out of.
 * Null means nothing survives; the caller falls back to the tail (and should
 * discard the saved scroll position with it).
 */
export function restoreVirtualWindow(
  saved: TranscriptWindow,
  trimmedCount: number,
  total: number,
): TranscriptWindow | null {
  const converted = toArrayCoords(saved, trimmedCount, total);
  if (converted === null) return null;
  const repaired = restoreWindow(converted, total);
  if (repaired.end - repaired.start >= TRANSCRIPT_PAGE) return repaired;
  const bounded = boundedTotal(total);
  const end = Math.min(bounded, repaired.start + TRANSCRIPT_PAGE);
  return { start: Math.max(0, end - TRANSCRIPT_PAGE), end };
}

/**
 * Shift a mounted view's array range across a reducer trim of `trimDelta`
 * rows so it keeps naming the same surviving rows. `lost` is how many of the
 * range's own leading rows were trimmed away (the rendered slice must drop
 * exactly that many to stay aligned). Null when the whole range was trimmed —
 * the mounted mirror of {@link restoreVirtualWindow}'s null: fall back to the
 * tail.
 */
export function trimShift(
  current: TranscriptWindow,
  trimDelta: number,
): { window: TranscriptWindow; lost: number } | null {
  const delta = Math.max(0, Math.floor(trimDelta));
  const end = Math.floor(current.end) - delta;
  if (end <= 0) return null;
  const start = Math.max(0, Math.floor(current.start) - delta);
  const lost = Math.min(Math.floor(current.end), Math.max(0, delta - Math.floor(current.start)));
  return { window: { start, end }, lost };
}

export interface PagePlan {
  /** One-tick range used to measure the content being added at an edge. */
  expanded: TranscriptWindow;
  /** Steady-state range after far-away DOM is discarded. */
  settled: TranscriptWindow;
}

export interface AutoEarlierPagePlan extends PagePlan {
  /** Keep rendering and following the live tail while a short viewport fills. */
  preserveTail: boolean;
}

/** Prepend one page, then discard the farthest newer page past the cap. */
export function pageEarlier(current: TranscriptWindow, total: number): PagePlan {
  const end = Math.min(current.end, boundedTotal(total));
  const start = Math.max(0, current.start - TRANSCRIPT_PAGE);
  const expanded = { start, end };
  const settled =
    end - start > TRANSCRIPT_WINDOW ? { start, end: start + TRANSCRIPT_WINDOW } : expanded;
  return { expanded, settled };
}

/**
 * Plan an observer-driven earlier page. A visible sentinel can mean either
 * that the reader scrolled upward or that a short live tail has not filled
 * the viewport yet. The latter may grow backward without suspending follow,
 * but stops at the DOM cap instead of silently paging away from the live edge.
 */
export function autoPageEarlier(
  current: TranscriptWindow,
  total: number,
  atBottom: boolean,
): AutoEarlierPagePlan | null {
  const bounded = boundedTotal(total);
  const plan = pageEarlier(current, bounded);
  const atLiveTail = atBottom && current.end >= bounded;
  if (!atLiveTail) return { ...plan, preserveTail: false };
  if (plan.settled.end < bounded) return null;
  return { ...plan, preserveTail: true };
}

/** Append one page, then discard the farthest older page past the cap. */
export function pageLater(current: TranscriptWindow, total: number): PagePlan {
  const end = Math.min(boundedTotal(total), current.end + TRANSCRIPT_PAGE);
  const start = Math.max(0, Math.min(current.start, end));
  const expanded = { start, end };
  const settled =
    end - start > TRANSCRIPT_WINDOW ? { start: end - TRANSCRIPT_WINDOW, end } : expanded;
  return { expanded, settled };
}
