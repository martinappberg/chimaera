/**
 * `$…$` / `$$…$$` math for the markdown editor's syntax tree: the lezer
 * parser extension behind the live preview's equation widgets and behind
 * source mode (an equation's `\;` and `_` are no longer highlighted as
 * markdown escapes and emphasis).
 *
 * The delimiter rules mirror comrak's `math_dollars` extension, which renders
 * the reading view, so a document reads the same in every mode: an inline
 * opener `$` must not be followed by whitespace, its closer must not be
 * preceded by whitespace or followed by a digit (`$5 and $10` stays
 * currency), `\$` inside inline math is an escaped dollar, and `$$` display
 * math takes everything up to the next `$$` — line breaks included, so a
 * `$$` block on its own lines is one (multi-line) element of its paragraph.
 * Three or more dollars in a row are plain text. Backticks win: a `$` inside
 * a code span never reaches this parser.
 */
import type { InlineContext, MarkdownConfig } from "@lezer/markdown";

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

/** Syntax-tree math: `InlineMath` (`$…$`), `DisplayMath` (`$$…$$`), each
 *  with two `MathMark` delimiter children and nothing else parsed inside.
 *  No highlight tags: the live decorator styles them, and source mode keeps
 *  an equation in the prose color (the editor's highlight style has no rule
 *  the marks could usefully carry). Also deliberately NOT the chat dialect
 *  (`chat/math.ts`, which mirrors what agents emit) — see the module header. */
export const mathExtension: MarkdownConfig = {
  defineNodes: [{ name: "InlineMath" }, { name: "DisplayMath" }, { name: "MathMark" }],
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
