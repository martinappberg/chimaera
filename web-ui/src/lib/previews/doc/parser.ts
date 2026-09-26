/**
 * THE markdown parser configuration: every surface that reads a document —
 * the live editor (`mdLive.ts`, through `@codemirror/lang-markdown`) and the
 * reading renderer (`doc/model.ts`) — parses with exactly these extensions
 * on exactly this base, so the two views can never disagree about what a
 * line is.
 *
 * The base is lang-markdown's GFM language (tables, task lists, `~~`
 * strikethrough, autolinks, plus Superscript and Emoji, which render as the
 * literal text they are on GitHub). On top of it:
 *
 * - `$`/`$$` math (`mdMath.ts`, mirroring comrak's `math_dollars`);
 * - single-tilde strikethrough in place of Pandoc subscript: comrak's GFM
 *   strikethrough takes `~x~` as well as `~~x~~`, flanking by the same rules
 *   (so `~two words~` is struck, which subscript's no-space rule refused),
 *   and a run of three or more tildes is text;
 * - footnotes: `[^id]` references and `[^id]: …` definitions (a container:
 *   lines indented four columns continue it, as in comrak). A label holds
 *   no whitespace or brackets — `[^a b]` is an ordinary link, as there;
 * - wikilinks, read for Obsidian vaults: `[[target]]`, `[[target|alias]]`,
 *   `[[target#heading]]`, and `![[embed]]`, on one line.
 *
 * Node shapes the renderer and decorator read: `FootnoteReference` and
 * `FootnoteDefinition` each hold `FootnoteMark`s around a `FootnoteLabel`
 * (the definition's content blocks follow); `Wikilink` holds a
 * `WikilinkMark` on each side (`![[` marks an embed) around its text.
 */
import { markdownLanguage } from "@codemirror/lang-markdown";
import type { BlockContext, Line, MarkdownConfig, MarkdownParser } from "@lezer/markdown";
import { mathExtension } from "../mdMath";

const BRACKET_OPEN = 91; // "["
const BRACKET_CLOSE = 93; // "]"
const CARET = 94; // "^"
const BANG = 33; // "!"
const TILDE = 126; // "~"

/** Unicode whitespace or punctuation, lezer's flanking classes. */
const PUNCTUATION = /[!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~\p{P}\p{S}]/u;

/** A footnote label's characters: anything but whitespace and brackets. */
function isLabelChar(ch: number): boolean {
  return ch > 32 && ch !== BRACKET_OPEN && ch !== BRACKET_CLOSE && ch !== 127;
}

const SingleTilde = { resolve: "Strikethrough", mark: "StrikethroughMark" };

/** `~x~`: comrak's single-tilde strikethrough, resolved as the same
 *  `Strikethrough` node `~~` makes (a separate delimiter type, so a `~`
 *  never closes a `~~`). Runs of three or more are consumed as text, so no
 *  later position starts a `~~` inside one. */
export const tildeStrikeExtension: MarkdownConfig = {
  remove: ["Subscript"],
  parseInline: [
    {
      name: "TildeStrike",
      before: "Strikethrough",
      parse(cx, next, pos) {
        if (next !== TILDE || cx.char(pos - 1) === TILDE) return -1;
        let n = 1;
        while (cx.char(pos + n) === TILDE) n++;
        if (n >= 3) return pos + n;
        if (n === 2) return -1; // GFM's own `~~`
        const before = cx.slice(pos - 1, pos);
        const after = cx.slice(pos + 1, pos + 2);
        const sBefore = /\s|^$/.test(before);
        const sAfter = /\s|^$/.test(after);
        const pBefore = PUNCTUATION.test(before);
        const pAfter = PUNCTUATION.test(after);
        return cx.addDelimiter(
          SingleTilde,
          pos,
          pos + 1,
          !sAfter && (!pAfter || sBefore || pBefore),
          !sBefore && (!pBefore || sAfter || pAfter),
        );
      },
    },
  ],
};

/** `[^label]` at `pos` → the end of the label's `]`, or -1. */
function footnoteLabelEnd(text: string, pos: number): number {
  if (text.charCodeAt(pos) !== BRACKET_OPEN || text.charCodeAt(pos + 1) !== CARET) return -1;
  let i = pos + 2;
  while (i < text.length && isLabelChar(text.charCodeAt(i))) i++;
  return i > pos + 2 && text.charCodeAt(i) === BRACKET_CLOSE ? i : -1;
}

/** Content indent a definition's continuation lines need (GFM/comrak). */
const FOOTNOTE_INDENT = 4;

/** Whether the line opens a definition: `[^label]:` at most three columns
 *  in (four is indented code). */
function opensFootnote(line: Line): boolean {
  if (line.indent - line.baseIndent >= 4) return false;
  const close = footnoteLabelEnd(line.text, line.pos);
  return close >= 0 && line.text.charCodeAt(close + 1) === 58; /* ":" */
}

export const footnoteExtension: MarkdownConfig = {
  defineNodes: [
    {
      name: "FootnoteDefinition",
      block: true,
      // A line continues the definition when indented four columns past its
      // base (or blank); a lazy paragraph line is lezer's to decide.
      composite(_cx: BlockContext, line: Line, value: number): boolean {
        if (line.indent < line.baseIndent + value && line.next > -1) return false;
        line.moveBaseColumn(line.baseIndent + value);
        return true;
      },
    },
    { name: "FootnoteReference" },
    { name: "FootnoteLabel" },
    { name: "FootnoteMark" },
  ],
  parseBlock: [
    {
      name: "FootnoteDefinition",
      before: "LinkReference",
      parse(cx, line) {
        if (!opensFootnote(line)) return false;
        const close = footnoteLabelEnd(line.text, line.pos);
        const from = cx.lineStart + line.pos;
        cx.startComposite("FootnoteDefinition", line.pos, FOOTNOTE_INDENT);
        cx.addElement(cx.elt("FootnoteMark", from, from + 2));
        cx.addElement(cx.elt("FootnoteLabel", from + 2, cx.lineStart + close));
        cx.addElement(cx.elt("FootnoteMark", cx.lineStart + close, cx.lineStart + close + 2));
        line.moveBase(line.skipSpace(close + 2));
        return null;
      },
      // A definition line ends the paragraph before it (comrak), so
      // back-to-back definitions never run into one lazy paragraph.
      endLeaf: (_cx, line) => opensFootnote(line),
    },
  ],
  parseInline: [
    {
      name: "FootnoteReference",
      before: "Link",
      parse(cx, next, pos) {
        if (next !== BRACKET_OPEN || cx.char(pos + 1) !== CARET) return -1;
        let i = pos + 2;
        while (i < cx.end && isLabelChar(cx.char(i))) i++;
        if (i === pos + 2 || cx.char(i) !== BRACKET_CLOSE) return -1;
        return cx.addElement(
          cx.elt("FootnoteReference", pos, i + 1, [
            cx.elt("FootnoteMark", pos, pos + 2),
            cx.elt("FootnoteLabel", pos + 2, i),
            cx.elt("FootnoteMark", i, i + 1),
          ]),
        );
      },
    },
  ],
};

/** `[[…]]` / `![[…]]` on one line, not holding a bracket; null when the
 *  text there is no wikilink. */
function wikilinkEnd(cx: { char(pos: number): number; end: number }, open: number): number {
  for (let i = open; i < cx.end; i++) {
    const ch = cx.char(i);
    if (ch === 10 || ch === BRACKET_OPEN) return -1;
    if (ch === BRACKET_CLOSE) return cx.char(i + 1) === BRACKET_CLOSE && i > open ? i : -1;
  }
  return -1;
}

export const wikilinkExtension: MarkdownConfig = {
  defineNodes: [{ name: "Wikilink" }, { name: "WikilinkMark" }],
  parseInline: [
    {
      name: "Wikilink",
      before: "Link",
      parse(cx, next, pos) {
        const embed = next === BANG;
        const at = embed ? pos + 1 : pos;
        if ((next !== BRACKET_OPEN && !embed) || cx.char(at) !== BRACKET_OPEN) return -1;
        if (cx.char(at + 1) !== BRACKET_OPEN) return -1;
        const close = wikilinkEnd(cx, at + 2);
        if (close < 0) return -1;
        return cx.addElement(
          cx.elt("Wikilink", pos, close + 2, [
            cx.elt("WikilinkMark", pos, at + 2),
            cx.elt("WikilinkMark", close, close + 2),
          ]),
        );
      },
    },
  ],
};

/** The extensions both views add to the GFM base, in this order. */
export const docExtensions: MarkdownConfig[] = [
  mathExtension,
  tildeStrikeExtension,
  footnoteExtension,
  wikilinkExtension,
];

/** The reading renderer's parser: the live language's base with the same
 *  extensions (lang-markdown adds only nested-language mounts on top, which
 *  never change the markdown structure). */
export const docParser: MarkdownParser = (markdownLanguage.parser as MarkdownParser).configure(
  docExtensions,
);
