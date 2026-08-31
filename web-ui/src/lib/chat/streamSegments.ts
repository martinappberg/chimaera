/**
 * Incremental segmentation of STREAMING markdown source, so the renderer can
 * cache already-parsed prefix blocks and re-parse only the trailing open
 * segment per wire chunk (O(tail) instead of O(message) — the streaming
 * transcript's scaling fix).
 *
 * The contract with the renderer:
 * - A "closed" segment is a source slice that ended at a SAFE top-level
 *   blank-line boundary: parsing it alone yields the same block structure as
 *   parsing it inside the full document. Safety is deliberately conservative —
 *   fenced code, block math, list/indented continuations, and HTML-ish blocks
 *   all refuse to close, and a reference-link definition anywhere disables
 *   further closing for the whole message (its effect is non-local). When in
 *   doubt the trailing segment simply grows.
 * - The partition is LOSSLESS: closed segments plus the open tail concatenate
 *   back to exactly the input, byte for byte. Segmentation can therefore never
 *   eat or double the `\n\n` block separators the chat reducer materializes at
 *   agent text-block boundaries (PR #122) — boundaries are consumed into the
 *   closed segment that precedes them, never dropped.
 * - Any residual segmentation artifact is transient BY CONSTRUCTION: the
 *   renderer does one canonical full re-parse of the whole message when the
 *   stream settles, so the settled transcript is identical to a non-streamed
 *   render regardless of how the stream was segmented.
 *
 * Pure string math, no DOM, no marked — unit-tested in streamSegments.test.ts.
 */

export interface SegmenterState {
  /** The full source seen so far — the prefix-invalidation witness. */
  source: string;
  /** Offset into `source` where the open (trailing) segment starts. */
  consumed: number;
  /** Closed segments emitted so far (cache-slot count for the renderer). */
  closedCount: number;
  /** A reference-link definition was seen: its targets resolve across the
   *  whole document, so per-segment parsing would silently break references.
   *  Once set, no further segments close — the tail grows to stream end and
   *  the canonical settle parse renders the message whole. Sticky. */
  bail: boolean;
}

export interface SegmentAdvance {
  state: SegmenterState;
  /** Source slices (in order) that closed during THIS advance. Each includes
   *  its trailing blank-line separator, so slices concatenate losslessly. */
  newlyClosed: string[];
  /** The trailing open segment's source (may be empty). */
  open: string;
  /** False when `text` did not extend the previous source (a rewrite or
   *  retraction): the state was rebuilt from scratch and `newlyClosed` holds
   *  EVERY closed segment of the new text — the caller must drop its cached
   *  prefix and rebuild. */
  extended: boolean;
}

const FENCE_OPEN = /^ {0,3}(`{3,}|~{3,})/;
const FENCE_CLOSE = /^ {0,3}(`{3,}|~{3,})[ \t]*$/;
/** A bare `$`/`$$` line opens (and later closes) block dollar math — the
 *  math.ts BLOCK_DOLLAR form, whose body may span blank lines. */
const MATH_DOLLAR_LINE = /^(\${1,2})[ \t]*$/;
const MATH_BRACKET_OPEN = /^ {0,3}\\\[/;
/** Reference-link definition ("[label]: target") — the non-local construct. */
const REF_DEF = /^ {0,3}\[[^\]]*\]:/;
/** A boundary is UNSAFE when the candidate segment's last content line could
 *  still be continued by future top-level text across the blank line:
 *  list items and anything indented (loose-list continuations, indented code
 *  blocks span blank lines), plus HTML-ish blocks (multi-line raw HTML). */
const UNSAFE_LAST_LINE = /^(?:\s|[-*+](?:\s|$)|\d{1,9}[.)](?:\s|$)|<)/;

interface Scanner {
  fence: { char: string; len: number } | null;
  /** Open block-math construct: the exact `$`/`$$` opener, or "bracket". */
  math: string | null;
}

export function advanceSegments(prev: SegmenterState | null, text: string): SegmentAdvance {
  const extended = prev !== null && text.startsWith(prev.source);
  const base: SegmenterState =
    extended && prev !== null
      ? prev
      : { source: "", consumed: 0, closedCount: 0, bail: false };

  const newlyClosed: string[] = [];
  let consumed = base.consumed;
  let bail = base.bail;

  if (!bail) {
    // Re-scan ONLY the open region (consumed → end). Construct state (fences,
    // math) never spans a closed boundary — a segment only closes with all
    // constructs closed — so the scan is self-contained and per-call work is
    // proportional to the tail, not the message.
    const scan: Scanner = { fence: null, math: null };
    let segStart = consumed;
    let lastContentLine: string | null = null;
    let blankRunStart = -1;

    /** Close the pending segment at `end` if its last content line allows it. */
    const tryClose = (end: number): void => {
      if (lastContentLine === null) return; // an all-blank candidate merges forward
      if (UNSAFE_LAST_LINE.test(lastContentLine)) return;
      newlyClosed.push(text.slice(segStart, end));
      segStart = end;
      lastContentLine = null;
    };

    let ls = consumed;
    while (ls < text.length && !bail) {
      const nl = text.indexOf("\n", ls);
      const complete = nl !== -1;
      const le = complete ? nl : text.length;
      const line = text.slice(ls, le);
      const blank = line.trim().length === 0;

      if (blank) {
        // Only a COMPLETE blank line is a boundary candidate — the final
        // unterminated line may still grow into content. Inside an open
        // construct it is ordinary content.
        if (scan.fence === null && scan.math === null && complete && blankRunStart === -1) {
          blankRunStart = ls;
        }
      } else {
        if (blankRunStart !== -1) {
          // The blank run ended before this line: decide the boundary here.
          tryClose(ls);
          blankRunStart = -1;
        }
        if (scan.fence !== null) {
          const close = FENCE_CLOSE.exec(line);
          if (
            close !== null &&
            close[1][0] === scan.fence.char &&
            close[1].length >= scan.fence.len
          ) {
            scan.fence = null;
          }
        } else if (scan.math !== null) {
          if (scan.math === "bracket") {
            if (line.includes("\\]")) scan.math = null;
          } else if (line.trimEnd() === scan.math) {
            scan.math = null;
          }
        } else if (REF_DEF.test(line)) {
          bail = true;
          break;
        } else {
          const fence = FENCE_OPEN.exec(line);
          const dollar = MATH_DOLLAR_LINE.exec(line);
          if (fence !== null) {
            scan.fence = { char: fence[1][0], len: fence[1].length };
          } else if (dollar !== null) {
            scan.math = dollar[1];
          } else if (MATH_BRACKET_OPEN.test(line) && !line.includes("\\]")) {
            scan.math = "bracket";
          }
        }
        lastContentLine = line;
      }
      if (!complete) break;
      ls = le + 1;
    }
    // A complete blank run at the very end of the text is a boundary too:
    // the next chunk starts a fresh top-level block either way.
    if (!bail && blankRunStart !== -1) tryClose(text.length);
    consumed = segStart;
  }

  const state: SegmenterState = {
    source: text,
    consumed,
    closedCount: base.closedCount + newlyClosed.length,
    bail,
  };
  return { state, newlyClosed, open: text.slice(consumed), extended };
}
