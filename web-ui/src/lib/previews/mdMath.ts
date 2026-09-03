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
 * ```math fence before comrak — `fs.rs::promote_math_blocks`; a case list,
 * `mathBlocks.fixture.json`, runs on both sides and keeps the two
 * in lockstep): a line whose content starts with `$$` (not `$$$`) with no
 * closing `$$` later on it opens a block, PROVIDED a closer is in sight — a
 * later line containing `$$` before the next blank line. The first such
 * line closes it (text after the closer stays outside); the interior is
 * raw. Without this, an equation wrapped as `+ \left(…\right)` on its
 * second line would be cut by the bullet list that line starts. Requiring
 * the closer keeps a slip cheap: prose that merely begins with `$$` (`$$ is
 * the shell's PID`) stays prose, and a block can never swallow more than
 * the paragraph it sits in. A lone `$$` line never interrupts a paragraph
 * whose display math is still open (an odd `$$` count), so `so $$` ⏎ `x`
 * ⏎ `$$` is one inline display equation. GitHub's ```math fence is the
 * same thing under another name and typesets too (mdLive treats it as a
 * block; the server already renders it as one).
 */
import { NodeProp, type Input, type NodeType, type SyntaxNode } from "@lezer/common";
import type {
  BlockContext,
  InlineContext,
  LeafBlock,
  LeafBlockParser,
  Line,
  MarkdownConfig,
} from "@lezer/markdown";

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
 *  blockquote, but its typings leave it out. Likewise `input`: the raw
 *  document, which the look-ahead below reads without consuming lines. */
function lineDepth(line: Line): number {
  return (line as unknown as { depth: number }).depth;
}

function rawInput(cx: BlockContext): Input {
  return (cx as unknown as { input: Input }).input;
}

const LOOKAHEAD = 1 << 16;

/** Whether a closer is in sight: a later line containing `$$` before the
 *  next blank line. Reads the raw input (quote markers stripped loosely), so
 *  it can disagree with the consuming loop's exact container rules — only
 *  ever toward a block that ends unclosed where a line leaves the
 *  enclosing list item (the fixture pins it; a blank line never gets that
 *  far, since it ends this look-ahead first), which then shows as source.
 *  Never toward swallowing the document. */
function closerAhead(cx: BlockContext, line: Line): boolean {
  const input = rawInput(cx);
  const from = cx.lineStart + line.text.length + 1;
  if (from >= input.length) return false;
  const text = input.read(from, Math.min(input.length, from + LOOKAHEAD));
  // Lines that leave the enclosing blockquote(s) end the block, as a fence's
  // do — a lazily continued quote is left to inline pairing (the reading
  // render's comrak does the same). The depth comes from the context: the
  // opener's own `>` is a node, not one of `line.markers`.
  let quoteDepth = 0;
  for (let d = 0; d < cx.depth; d++) if (cx.parentType(d).name === "Blockquote") quoteDepth++;
  for (const raw of text.split("\n")) {
    const stripped = raw.replace(/^(?: {0,3}>[ \t]?)*/, "");
    const depth = (raw.length - stripped.length && raw.slice(0, raw.length - stripped.length).split(">").length - 1) || 0;
    const t = stripped.trim();
    if (t.length === 0 || depth < quoteDepth) return false;
    if (t.includes("$$")) return true;
  }
  return false;
}

/** `$$` delimiters in a run of text: pairs not part of a longer dollar run
 *  (`$$$` is text) and not inside a backtick code span (backticks win). The
 *  parity of this count says whether display math is open. */
export function countDollarPairs(text: string): number {
  let n = 0;
  const bare = text.replace(/`+[^`]*`+/g, "");
  for (let i = bare.indexOf("$$"); i >= 0; i = bare.indexOf("$$", i + 2)) {
    if (bare[i - 1] === "$" || bare[i + 2] === "$") {
      // a longer run: skip the whole run
      let j = i;
      while (bare[j] === "$") j++;
      i = j - 2;
      continue;
    }
    n++;
  }
  return n;
}

/** Tracks, per line as the paragraph grows, whether its display math is
 *  open — so `endLeaf` reads one flag instead of re-scanning the whole
 *  paragraph on every `$$` line (a paragraph of thousands of `$$` lines
 *  would otherwise go quadratic inside one parse step). */
class DollarParity implements LeafBlockParser {
  odd: boolean;
  constructor(firstLine: string) {
    this.odd = countDollarPairs(firstLine) % 2 === 1;
  }
  nextLine(_cx: BlockContext, line: Line): boolean {
    if (countDollarPairs(line.text.slice(line.pos)) % 2 === 1) this.odd = !this.odd;
    return false;
  }
  finish(): boolean {
    return false;
  }
}

function displayMathOpen(leaf: LeafBlock): boolean {
  const p = leaf.parsers.find((x) => x instanceof DollarParity);
  return p instanceof DollarParity && p.odd;
}

/** The delimiter node: the `$` / `$$` on either side of an equation. */
export const MATH_MARK = "MathMark";

/** The groups (`NodeType.is`) the math nodes join — callers ask `isMath` /
 *  `isDisplayMath` rather than comparing names: `Math` is every equation
 *  node, `MathDisplay` the ones typeset display-style. Appended to the
 *  groups lezer already gave the node (`MathBlock` is a `Block`/`LeafBlock`
 *  like any block node) — a plain `group.add({…})` would replace them. */
const MATH_GROUPS = new Map<string, readonly string[]>([
  ["InlineMath", ["Math"]],
  ["DisplayMath", ["Math", "MathDisplay"]],
  ["MathBlock", ["Math", "MathDisplay"]],
]);

/** Whether the node is an equation: `InlineMath`, `DisplayMath` or `MathBlock`. */
export function isMath(type: NodeType): boolean {
  return type.is("Math");
}

/** Whether the equation is typeset display-style (`DisplayMath`, `MathBlock`). */
export function isDisplayMath(type: NodeType): boolean {
  return type.is("MathDisplay");
}

/** The opening and closing delimiters of a closed equation, or null while a
 *  `$$` block is unclosed — the list item its lines sat in ended before a
 *  closer, and they show as source until one is typed. Inline math is only
 *  ever emitted closed. */
export function mathDelimiters(node: SyntaxNode): [SyntaxNode, SyntaxNode] | null {
  const marks = node.getChildren(MATH_MARK);
  return marks.length === 2 ? [marks[0], marks[1]] : null;
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
    { name: MATH_MARK },
  ],
  props: [
    NodeProp.group.add((type) => {
      const groups = MATH_GROUPS.get(type.name);
      // Only `undefined` leaves a node alone: lezer stores any other return,
      // `[]` included, as the node's group in place of its own. The filter
      // keeps a second configure from listing the groups twice.
      if (groups === undefined) return undefined;
      return [...(type.prop(NodeProp.group) ?? []), ...groups.filter((g) => !type.is(g))];
    }),
  ],
  parseBlock: [
    {
      name: "MathBlock",
      parse(cx, line) {
        if (!opensMathBlock(line) || !closerAhead(cx, line)) return false;
        const from = cx.lineStart + line.pos;
        const marks = [cx.elt(MATH_MARK, from, from + 2)];
        let end = -1;
        while (cx.nextLine() && lineDepth(line) >= cx.depth) {
          // A blank line means the look-ahead and the container rules
          // disagreed: end here, unclosed (shown as source), never beyond.
          if (line.pos === line.text.length) break;
          for (const m of line.markers) marks.push(m);
          const i = line.text.indexOf("$$", line.pos);
          if (i >= 0) {
            marks.push(cx.elt(MATH_MARK, cx.lineStart + i, cx.lineStart + i + 2));
            end = cx.lineStart + i + 2;
            cx.nextLine();
            break;
          }
        }
        cx.addElement(cx.elt("MathBlock", from, end >= 0 ? end : cx.prevLineEnd(), marks));
        return true;
      },
      // A `$$` line interrupts a paragraph (a fence does too), so
      // `prose\n$$\n…\n$$` is prose + a block, never one paragraph — unless
      // the paragraph has display math OPEN (an odd count of `$$`): then the
      // lone `$$` closes it, and `so $$\nx\n$$` is one display equation.
      endLeaf(cx, line, leaf) {
        return opensMathBlock(line) && !displayMathOpen(leaf) && closerAhead(cx, line);
      },
      leaf(_cx, leaf) {
        return new DollarParity(leaf.content);
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
            cx.elt(MATH_MARK, pos, pos + n),
            cx.elt(MATH_MARK, end - n, end),
          ]),
        );
      },
    },
  ],
};
