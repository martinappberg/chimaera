/**
 * `$…$` / `$$…$$` math for the markdown editor's syntax tree: the lezer
 * parser extension behind the live preview's equation widgets and behind
 * source mode (an equation's `\;` and `_` are no longer highlighted as
 * markdown escapes and emphasis).
 *
 * Two forms, mirroring the reading view's comrak render so a document reads
 * the same in every mode:
 *
 * INLINE (comrak's `math_dollars`): an inline opener `$` must not be
 * followed by whitespace, its closer must not be preceded by whitespace or
 * followed by a digit (`$5 and $10` stays currency), `\$` inside inline math
 * is an escaped dollar, and `$$` display math takes everything up to the
 * next `$$` — line breaks included — within its paragraph. Three or more
 * dollars in a row are plain text. Backticks win: a `$` inside a code span
 * never reaches this parser.
 *
 * BLOCK (Obsidian's / GitHub's `$$` block, which the server promotes to a
 * ```math fence before comrak — `fs.rs::promote_math_blocks`, keep the two
 * in lockstep): a line whose content starts with `$$` (not `$$$`) and has no
 * closing `$$` later on it opens a block; the first later line ENDING in
 * `$$` closes it; the interior is raw. Without this, an equation wrapped as
 * `+ \left(…\right)` on its second line would be cut by the bullet list that
 * line starts. Blocks open inside blockquotes but not list items (the
 * server's line pass can't see list nesting), and an unterminated block
 * runs to the end of the document, as a fence does.
 */
import type { BlockContext, InlineContext, Line, MarkdownConfig } from "@lezer/markdown";

const DOLLAR = 36; // "$"
const BACKSLASH = 92; // "\"

/** cmark's `isspace` (space, \t \n \v \f \r) — comrak's rule, deliberately
 *  wider than lezer's own space set. */
function isSpace(ch: number): boolean {
  return ch === 32 || (ch >= 9 && ch <= 13);
}

function isDigit(ch: number): boolean {
  return ch >= 48 && ch <= 57;
}

/** End (exclusive) of the inline `$…$` opened at `pos`, or -1 when the run
 *  isn't math under the rules above. */
function scanInline(cx: InlineContext, pos: number): number {
  if (isSpace(cx.char(pos + 1))) return -1;
  for (let i = pos + 1; i < cx.end; i++) {
    if (cx.char(i) !== DOLLAR) continue;
    const before = cx.char(i - 1);
    if (isSpace(before)) return -1;
    if (before === BACKSLASH) continue; // `\$` is a literal dollar inside math
    if (isDigit(cx.char(i + 1))) return -1;
    return i + 1;
  }
  return -1;
}

/** End (exclusive) of the display `$$…$$` opened at `pos`, or -1. A lone
 *  `$` inside is content; the first `$$` closes. */
function scanDisplay(cx: InlineContext, pos: number): number {
  for (let i = pos + 2; i < cx.end; ) {
    if (cx.char(i) !== DOLLAR) {
      i++;
      continue;
    }
    let run = 0;
    while (cx.char(i + run) === DOLLAR) run++;
    if (run >= 2) return i + 2;
    i += run;
  }
  return -1;
}

/** Whether the line's content opens a `$$` block: `$$` first, not `$$$`,
 *  and no closing `$$` later on the same line (that is inline display). */
function opensMathBlock(line: Line): boolean {
  const t = line.text;
  const p = line.pos;
  if (line.next !== DOLLAR || t.charCodeAt(p + 1) !== DOLLAR || t.charCodeAt(p + 2) === DOLLAR)
    return false;
  return !t.includes("$$", p + 2);
}

/** `Line.depth` — how many enclosing containers the line still sits in — is
 *  what lezer's own fenced-code parser stops on when a block leaves its
 *  blockquote, but its typings leave it out. */
function lineDepth(line: Line): number {
  return (line as unknown as { depth: number }).depth;
}

function inListItem(cx: BlockContext): boolean {
  return cx.parentType().name === "ListItem";
}

/** Syntax-tree math: `InlineMath` (`$…$`), `DisplayMath` (`$$…$$` within a
 *  paragraph) and `MathBlock` (`$$` block lines), each with `MathMark`
 *  delimiter children — two when closed — and nothing else parsed inside
 *  (a quoted block keeps its per-line `QuoteMark`s as children, like a
 *  fence). No highlight tags: the live decorator styles them, and source
 *  mode keeps an equation in the prose color. Deliberately NOT the chat
 *  dialect (`chat/math.ts`, which mirrors what agents emit). */
export const mathExtension: MarkdownConfig = {
  defineNodes: [
    { name: "InlineMath" },
    { name: "DisplayMath" },
    { name: "MathBlock", block: true },
    { name: "MathMark" },
  ],
  parseBlock: [
    {
      name: "MathBlock",
      parse(cx, line) {
        if (!opensMathBlock(line) || inListItem(cx)) return false;
        const from = cx.lineStart + line.pos;
        const marks = [cx.elt("MathMark", from, from + 2)];
        while (cx.nextLine() && lineDepth(line) >= cx.depth) {
          for (const m of line.markers) marks.push(m);
          const end = line.text.trimEnd().length;
          if (end - 2 >= line.pos && line.text.startsWith("$$", end - 2)) {
            marks.push(cx.elt("MathMark", cx.lineStart + end - 2, cx.lineStart + end));
            cx.nextLine();
            break;
          }
        }
        cx.addElement(cx.elt("MathBlock", from, cx.prevLineEnd(), marks));
        return true;
      },
      // A `$$` line interrupts a paragraph (a fence does too), so
      // `prose\n$$\n…\n$$` is prose + a block, never one paragraph — unless
      // the paragraph has display math OPEN (an odd count of `$$`): then the
      // lone `$$` closes it, and `so $$\nx\n$$` is one display equation.
      endLeaf(cx, line, leaf) {
        if (!opensMathBlock(line) || inListItem(cx)) return false;
        return (leaf.content.match(/\$\$/g) ?? []).length % 2 === 0;
      },
    },
  ],
  parseInline: [
    {
      name: "DollarMath",
      parse(cx, next, pos) {
        if (next !== DOLLAR) return -1;
        let n = 1;
        while (cx.char(pos + n) === DOLLAR) n++;
        // Consuming the whole run as text (rather than declining) keeps a
        // later `$` in a `$$$` run from opening math comrak wouldn't.
        if (n > 2) return pos + n;
        const end = n === 2 ? scanDisplay(cx, pos) : scanInline(cx, pos);
        if (end < 0) return n === 2 ? pos + 2 : -1;
        return cx.addElement(
          cx.elt(n === 2 ? "DisplayMath" : "InlineMath", pos, end, [
            cx.elt("MathMark", pos, pos + n),
            cx.elt("MathMark", end - n, end),
          ]),
        );
      },
    },
  ],
};
