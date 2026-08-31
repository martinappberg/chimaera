/**
 * Incremental segmentation of STREAMING markdown source, so the renderer can
 * cache already-parsed prefix blocks and re-parse only the trailing open
 * segment per wire chunk (O(tail) instead of O(message) — the streaming
 * transcript's scaling fix).
 *
 * THE SECURITY INVARIANT: the streaming render must NEVER be more permissive
 * than the settled render. Splitting inside a construct whose interior is
 * inert when parsed whole (a fenced code block, block math, a raw-HTML block)
 * would hand that interior to marked as ordinary markdown — turning inert
 * text into live DOM (an `<img>` fires a network beacon; a link becomes
 * clickable) that DOMPurify happily allows. So fences and block math are
 * tracked and never split, and any line that can OPEN a CommonMark HTML block
 * (types 1-6 — `<pre`, `<!--`, `<?`, `<!X`, CDATA, or any `<tag`/`</tag`
 * line) BAILS segmentation for the whole message: raw HTML's continuation
 * rules are too varied to track safely, and HTML-in-agent-markdown is rare
 * enough that falling back to whole-tail parsing is cheap. An HTML bail is
 * never force-closed either — see below.
 *
 * The contract with the renderer:
 * - A "closed" segment ended at a SAFE top-level blank-line boundary:
 *   parsing it alone yields the same block structure as parsing it inside the
 *   full document. Safety is deliberately conservative — fenced code, block
 *   math, open lists (loose continuations INCLUDING lazy-continuation lines),
 *   and indented tails all refuse to close; a reference-link definition
 *   anywhere disables normal closing for the message (its targets resolve
 *   document-globally — and note the limitation: a reference USED in a
 *   segment that closed before the definition streamed in renders literally
 *   until settle). When in doubt the trailing segment simply grows.
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
 * Cost: the scan is INCREMENTAL — construct state and a cursor persist in
 * {@link SegmenterState}, so each advance walks only the newly arrived lines
 * (the block text is append-only; a rewrite is detected and rebuilt from
 * scratch). A reference definition does NOT stop the scan: it only makes
 * boundaries sticky (`refDefSeen`) — once the open tail exceeds
 * {@link BAIL_SEGMENT_CAP}, closes resume at the SAME safe boundaries (never
 * inside a fence/math, never in a list), so splitting can only cost reference
 * resolution (cosmetic, heals at settle), never activate inert content. An
 * HTML bail and a stuck-open fence/math tail get NO size relief — splitting
 * those can activate inert content (the invariant above) — so their parse
 * cost stays O(open tail) per chunk by construction; that residual worst case
 * (one giant unterminated fence) is accepted and documented.
 *
 * The scanner mirrors what marked actually does, not CommonMark in the
 * abstract: lines are compared after stripping one trailing `\r` (marked's
 * lexer normalizes `\r\n` to `\n`), and the block-math open/close lines must
 * be EXACTLY `$`/`$$` (math.ts BLOCK_DOLLAR demands `\n$$\n` — a trailing
 * space un-matches it) with `\[ … \]` closing only on a line that ENDS with
 * `\]`. The pin tests in streamSegments.test.ts hold the two in lockstep.
 *
 * Pure string math, no DOM — unit-tested in streamSegments.test.ts.
 */

/** Persisted scanner machine — everything needed to resume at `cursor`
 *  without rescanning the open region. `cursor` always sits at the start of
 *  the first line not yet consumed; the final UNTERMINATED line is never
 *  consumed (it may still grow), so its construct effects apply only once it
 *  gains a newline. */
export interface ScanState {
  cursor: number;
  fence: { char: string; len: number } | null;
  /** Open block math: the exact `$`/`$$` opener, or "bracket" for `\[`. */
  math: string | null;
  /** Inside a top-level list block (covers loose continuations across blank
   *  lines AND lazy-continuation plain lines). Cleared only when a flush,
   *  non-list, non-indented line starts a new block after a blank run. */
  inList: boolean;
  /** A reference-link definition was seen: boundaries turn sticky (refs
   *  resolve document-globally), relaxed only by the size cap — and even
   *  then only at the ordinary safe boundaries. Sticky for the message. */
  refDefSeen: boolean;
  lastContentLine: string | null;
  /** Start offset of the current run of complete blank lines, -1 if none. */
  blankRunStart: number;
  /** The next scanned line starts a new top-level block whose boundary was
   *  already decided EAGERLY while the line was unterminated — it must still
   *  count as block-starting (for the list-end rule) without re-deciding. */
  blockStartPending: boolean;
}

export type BailReason = false | "html";

export interface SegmenterState {
  /** The full source seen so far — the prefix-invalidation witness. */
  source: string;
  /** Offset into `source` where the open (trailing) segment starts. */
  consumed: number;
  /** "html": segmentation is off for the whole message and gets NO size
   *  relief (splitting raw HTML can activate inert content). */
  bail: BailReason;
  scan: ScanState;
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
/** Reference-link definition ("[label]: target", label may carry escaped
 *  brackets) — the non-local construct. Over-matching is fine (a bail is
 *  always safe). */
const REF_DEF = /^ {0,3}\[(?:\\.|[^\]])*\]:/;
/** Any line that can OPEN a CommonMark HTML block (types 1-7, conservatively:
 *  `<` immediately followed by a tag name, `/`, `!`, or `?`). Also matches
 *  line-leading autolinks/inline HTML — over-matching bails, which is safe. */
const HTML_BLOCK_OPEN = /^ {0,3}<[a-zA-Z!?/]/;
const LIST_ITEM = /^ {0,3}(?:[-*+]|\d{1,9}[.)])(?:[ \t]|$)/;
/** Size (UTF-16 units ≈ bytes for prose) past which a ref-def-sticky tail
 *  resumes closing at the ordinary safe boundaries — bounding the per-chunk
 *  re-parse. Never relaxes fences, math, lists, or an HTML bail (see the
 *  security invariant). */
export const BAIL_SEGMENT_CAP = 16 * 1024;

function freshScan(): ScanState {
  return {
    cursor: 0,
    fence: null,
    math: null,
    inList: false,
    refDefSeen: false,
    lastContentLine: null,
    blankRunStart: -1,
    blockStartPending: false,
  };
}

export function advanceSegments(prev: SegmenterState | null, text: string): SegmentAdvance {
  const extended = prev !== null && text.startsWith(prev.source);
  let consumed = extended && prev !== null ? prev.consumed : 0;
  let bail: BailReason = extended && prev !== null ? prev.bail : false;
  const scan: ScanState = extended && prev !== null ? { ...prev.scan } : freshScan();
  const newlyClosed: string[] = [];

  /** Close the pending segment at `end` if its tail allows it. */
  const tryClose = (end: number): void => {
    if (scan.lastContentLine === null) return; // an all-blank candidate merges forward
    if (scan.inList) return; // loose lists + lazy continuations span blank lines
    if (/^[ \t]/.test(scan.lastContentLine)) return; // indented code/continuation
    // Reference definitions make boundaries sticky — until the open tail is
    // large enough that per-chunk re-parses hurt; then closing (still only at
    // safe boundaries) costs at most reference resolution until settle.
    if (scan.refDefSeen && end - consumed <= BAIL_SEGMENT_CAP) return;
    newlyClosed.push(text.slice(consumed, end));
    consumed = end;
    scan.lastContentLine = null;
  };

  if (bail === false) {
    while (scan.cursor < text.length) {
      const nl = text.indexOf("\n", scan.cursor);
      if (nl === -1) break; // the final unterminated line may still grow
      const lineStart = scan.cursor;
      const raw = text.slice(lineStart, nl);
      // marked's lexer normalizes \r\n to \n before tokenizing — compare what
      // marked will actually see, or CRLF streams never close a fence/math.
      const line = raw.endsWith("\r") ? raw.slice(0, -1) : raw;
      scan.cursor = nl + 1;

      if (line.trim().length === 0) {
        // Only a COMPLETE blank line is a boundary candidate; inside an open
        // construct it is ordinary content.
        if (scan.fence === null && scan.math === null && scan.blankRunStart === -1) {
          scan.blankRunStart = lineStart;
        }
        continue;
      }

      const startsBlock = scan.blankRunStart !== -1 || scan.blockStartPending;
      if (scan.blankRunStart !== -1) {
        // The blank run ended before this line: decide the boundary here.
        tryClose(lineStart);
        scan.blankRunStart = -1;
      }
      scan.blockStartPending = false;

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
        // Mirror math.ts exactly: BLOCK_DOLLAR closes only on a line that IS
        // the opener (`\n$$\n` — trailing whitespace un-matches); bracket math
        // closes only on a line ENDING with `\]` ((?:\n|$) after it).
        if (scan.math === "bracket") {
          if (line.endsWith("\\]")) scan.math = null;
        } else if (line === scan.math) {
          scan.math = null;
        }
      } else if (HTML_BLOCK_OPEN.test(line)) {
        bail = "html";
        break;
      } else {
        if (REF_DEF.test(line)) scan.refDefSeen = true;
        if (LIST_ITEM.test(line)) {
          scan.inList = true;
        } else if (startsBlock && !/^[ \t]/.test(line)) {
          // A flush non-list block start after a blank run ends the list.
          scan.inList = false;
        }
        const fence = FENCE_OPEN.exec(line);
        if (fence !== null) {
          scan.fence = { char: fence[1][0], len: fence[1].length };
        } else if (line === "$" || line === "$$") {
          scan.math = line; // math.ts BLOCK_DOLLAR opener: the exact line
        } else if (line.startsWith("\\[") && !line.endsWith("\\]")) {
          scan.math = "bracket";
        }
      }
      scan.lastContentLine = line;
    }
    // The loop consumes only TERMINATED lines; a pending blank run can still
    // resolve now, because the boundary rules look backward only:
    if (bail === false && scan.blankRunStart !== -1) {
      if (scan.cursor >= text.length) {
        // A complete blank run at the very end of the text is a boundary —
        // the next chunk starts a fresh top-level block either way.
        tryClose(text.length);
        scan.blankRunStart = -1;
        scan.blockStartPending = true;
      } else if (text.slice(scan.cursor).trim().length > 0) {
        // The run is followed by an unterminated line that already carries
        // content: decide the boundary at its start now (append-only text
        // can't blank it out; blockStartPending tells the completed line it
        // still starts a block — without re-deciding the boundary).
        tryClose(scan.cursor);
        scan.blankRunStart = -1;
        scan.blockStartPending = true;
      }
      // else: the trailing unterminated line is whitespace-so-far — it may
      // extend the run or grow content; leave the run pending.
    }
  }

  const state: SegmenterState = { source: text, consumed, bail, scan };
  return { state, newlyClosed, open: text.slice(consumed), extended };
}
