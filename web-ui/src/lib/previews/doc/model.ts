/**
 * The document model: the shared syntax tree (`parser.ts`) turned into plain
 * data blocks that carry their source ranges, plus the document-wide facts a
 * block's rendering depends on (reference definitions, footnote numbering,
 * heading ids). No DOM here — `render.ts` draws the model through either of
 * its targets, and the live editor reads the same inline model (`mdTable`).
 *
 * The rules are comrak's, as the daemon runs it (`fs.rs markdown_to_html`),
 * so the reading view renders a document the way the server fallback and
 * GitHub do: a leading `---` block is frontmatter (the daemon's shape: `---`
 * alone on line 1, a closer exactly `---` within 200 lines, a `key:` line
 * inside) and never body; GitHub alerts (`> [!NOTE]`, the marker right after
 * the quote's `> `, an optional title after it); heading ids are GitHub
 * slugs, numbered `-1`, `-2` on repeats; footnotes number in the order they
 * are first referenced, and a definition nobody references is dropped;
 * lists are tight unless a blank line separates items or an item's blocks.
 */
import type { SyntaxNode, Tree } from "@lezer/common";
import { TreeFragment } from "@lezer/common";
import {
  destinationText,
  inlineOf,
  normalizeLabel,
  plainText,
  tableModel,
  titleText,
  trimEdges,
  type DocText,
  type Inline,
  type InlineOptions,
  type LinkDef,
  type TableModel,
} from "../mdTable";
import { mathDelimiters, mathSource } from "../mdMath";
import { decodeEntities, unescapeBackslashes } from "./entities";
import { docParser } from "./parser";

export type AlertType = "note" | "tip" | "important" | "warning" | "caution";

interface Span {
  /** Source offsets of the block (lines come from the LineIndex). */
  from: number;
  to: number;
}

export type Block = Span &
  (
    | { kind: "paragraph"; inline: Inline[] }
    | { kind: "heading"; level: number; inline: Inline[]; id: string }
    | { kind: "rule" }
    | { kind: "quote"; children: Block[] }
    | { kind: "alert"; type: AlertType; title: string; children: Block[] }
    | { kind: "list"; ordered: boolean; start: number; tight: boolean; items: Item[] }
    | { kind: "code"; lang: string; text: string }
    | { kind: "math"; source: string }
    | { kind: "table"; table: TableModel }
    | { kind: "html"; source: string }
  );

export interface Item extends Span {
  task: "done" | "todo" | null;
  children: Block[];
}

/** A rendered footnote: its definition's blocks and every reference id. */
export interface Footnote extends Span {
  label: string;
  n: number;
  /** How many references point here (1 → `fnref-x`, 2 → also `fnref-x-2`). */
  refs: number;
  children: Block[];
}

// --- text and lines --------------------------------------------------------------

/** A plain string as the model's `DocText`. */
export function docText(text: string): DocText {
  return { sliceString: (from, to) => text.slice(from, to) };
}

/** 1-based lines of a document: what block rendering reads beyond the
 *  text itself (`data-sourcepos`, a fence's indentation, blank lines). */
export interface Lines {
  /** The line holding offset `pos`. */
  lineOf(pos: number): number;
  lineStart(line: number): number;
  lineText(line: number): string;
  /** comrak's `data-sourcepos`: `startLine:startCol-endLine:endCol`,
   *  1-based, the end column inclusive. */
  sourcepos(from: number, to: number): string;
}

function sourceposOf(lines: Lines, from: number, to: number): string {
  const l1 = lines.lineOf(from);
  const end = Math.max(from, to - 1);
  const l2 = lines.lineOf(end);
  return `${l1}:${from - lines.lineStart(l1) + 1}-${l2}:${end - lines.lineStart(l2) + 1}`;
}

/** The slice of CodeMirror's `Text` a `TextLines` reads. */
export interface LinedText {
  readonly lines: number;
  readonly length: number;
  lineAt(pos: number): { number: number; from: number; to: number; text: string };
  line(n: number): { from: number; text: string };
}

/** `Lines` over the editor's own document: no pass over the text (the
 *  live view renders single blocks of a document that changes per key). */
export class TextLines implements Lines {
  constructor(private readonly doc: LinedText) {}
  lineOf(pos: number): number {
    return this.doc.lineAt(Math.max(0, Math.min(pos, this.doc.length))).number;
  }
  lineStart(line: number): number {
    return line > this.doc.lines ? this.doc.length : this.doc.line(Math.max(1, line)).from;
  }
  lineText(line: number): string {
    return line < 1 || line > this.doc.lines ? "" : this.doc.line(line).text;
  }
  sourcepos(from: number, to: number): string {
    return sourceposOf(this, from, to);
  }
}

/** 1-based line numbers for offsets of one text. */
export class LineIndex implements Lines {
  readonly starts: number[] = [0];
  constructor(readonly text: string) {
    for (let i = text.indexOf("\n"); i >= 0; i = text.indexOf("\n", i + 1)) this.starts.push(i + 1);
  }
  /** The line holding offset `pos`. */
  lineOf(pos: number): number {
    const s = this.starts;
    let lo = 0;
    let hi = s.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (s[mid] <= pos) lo = mid;
      else hi = mid - 1;
    }
    return lo + 1;
  }
  lineStart(line: number): number {
    return this.starts[line - 1] ?? this.text.length;
  }
  lineText(line: number): string {
    const from = this.lineStart(line);
    const next = this.starts[line];
    return this.text.slice(from, next === undefined ? this.text.length : next - 1);
  }
  sourcepos(from: number, to: number): string {
    return sourceposOf(this, from, to);
  }
}

// --- frontmatter ------------------------------------------------------------------

export interface Frontmatter {
  /** The YAML between the delimiter lines (the properties panel's input). */
  raw: string;
  /** Offset just past the closing `---`. */
  end: number;
}

const FM_MAX_LINES = 200;

/**
 * The leading frontmatter block, by the daemon's rule (`fs.rs frontmatter`,
 * comrak's split plus the editor's shape): `---` alone on line 1, closed by
 * the first line that is exactly `---`, at most 200 lines in all, and at
 * least one `key:` line inside — so a document that merely opens with a
 * thematic break keeps it. Expects LF text.
 */
export function frontmatterOf(text: string): Frontmatter | null {
  const bom = text.charCodeAt(0) === 0xfeff ? 1 : 0;
  if (!text.startsWith("---\n", bom)) return null;
  const body = bom + 4;
  // The first `\n---` line closes it; `----` or `--- x` there is no closer
  // (comrak searches `\n---\n` first, then a `\n---` ending the text).
  let close = text.indexOf("\n---\n", body);
  if (close < 0) {
    close = text.indexOf("\n---", body);
    if (close < 0 || close + 4 !== text.length) return null;
  }
  const inner = text.slice(body, close);
  let lines = 3;
  for (let i = inner.indexOf("\n"); i >= 0; i = inner.indexOf("\n", i + 1)) lines++;
  if (lines > FM_MAX_LINES) return null;
  const keyed = inner.split("\n").some((l) => /^[A-Za-z0-9_-]+[ \t]*:/.test(l));
  return keyed ? { raw: inner, end: close + 4 } : null;
}

/** The text the parser sees: frontmatter blanked to spaces (lines and
 *  offsets kept, so every position still names the source), CRLF as LF. */
export function bodyText(source: string): { text: string; frontmatter: Frontmatter | null } {
  const text = source.includes("\r") ? source.replace(/\r\n?/g, "\n") : source;
  const fm = frontmatterOf(text);
  if (fm === null) return { text, frontmatter: null };
  return { text: text.slice(0, fm.end).replace(/[^\n]/g, " ") + text.slice(fm.end), frontmatter: fm };
}

// --- the document context -----------------------------------------------------------

/** GitHub's heading slug (comrak's anchorizer): lowercased; letters, marks,
 *  numbers, connector punctuation, `-` and spaces kept; spaces → `-`. */
export function githubSlug(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^\p{L}\p{M}\p{N}\p{Pc} -]/gu, "")
    .replace(/ /g, "-");
}

const HEADING = /^(ATXHeading[1-6]|SetextHeading[12])$/;
/** Nodes whose children are blocks: where definitions and headings nest. */
const CONTAINERS = new Set(["Document", "Blockquote", "BulletList", "OrderedList", "ListItem"]);

export interface DocContext {
  doc: DocText;
  lines: Lines;
  tree: Tree;
  refs: Map<string, LinkDef>;
  /** Normalized label → the definition node (the first one wins). */
  footnoteDefs: Map<string, SyntaxNode>;
  /** Heading node start → its slug (unprefixed), in render order. */
  headingIds: Map<number, string>;
  /** The body's headings, in order. */
  outline: OutlineEntry[];
  /** Footnote reference start → its number and which reference of its
   *  footnote it is (1-based). Only references naming a definition. */
  footnoteRefs: Map<number, { label: string; n: number; nth: number }>;
  /** Rendered footnotes in number order. */
  footnotes: { label: string; node: SyntaxNode; refs: number }[];
  inline: InlineOptions;
}

/** Visit block nodes in document order, descending only into containers
 *  (and footnote definitions when `defs` is set) — never into inline
 *  content, so a pass over a long document stays cheap. */
function eachBlock(parent: SyntaxNode, defs: boolean, f: (n: SyntaxNode) => void): void {
  for (let c = parent.firstChild; c !== null; c = c.nextSibling) {
    f(c);
    if (CONTAINERS.has(c.name) || (defs && c.name === "FootnoteDefinition")) eachBlock(c, defs, f);
  }
}

/** The link reference definition a LinkReference node holds. */
function linkReference(node: SyntaxNode, doc: DocText): [string, LinkDef] | null {
  const label = node.getChild("LinkLabel");
  const url = node.getChild("URL");
  if (label === null) return null;
  const title = node.getChild("LinkTitle");
  return [
    normalizeLabel(doc.sliceString(label.from + 1, label.to - 1)),
    {
      url: url === null ? "" : destinationText(doc.sliceString(url.from, url.to)),
      title: title === null ? null : titleText(doc.sliceString(title.from, title.to)),
    },
  ];
}

export function footnoteLabel(node: SyntaxNode, doc: DocText): string {
  const l = node.getChild("FootnoteLabel");
  return l === null ? "" : doc.sliceString(l.from, l.to);
}

/**
 * Parse `text` (already `bodyText`) and gather what blocks depend on beyond
 * their own source. `prev` makes the parse incremental: its tree's
 * unchanged regions are reused, so an edit re-parses only around itself.
 */
export function documentContext(
  text: string,
  prev: { text: string; tree: Tree } | null = null,
): DocContext {
  let fragments: readonly TreeFragment[] | undefined;
  if (prev !== null) {
    const a = prev.text;
    let start = 0;
    const max = Math.min(a.length, text.length);
    while (start < max && a.charCodeAt(start) === text.charCodeAt(start)) start++;
    let endA = a.length;
    let endB = text.length;
    while (endA > start && endB > start && a.charCodeAt(endA - 1) === text.charCodeAt(endB - 1)) {
      endA--;
      endB--;
    }
    fragments = TreeFragment.applyChanges(TreeFragment.addTree(prev.tree), [
      { fromA: start, toA: endA, fromB: start, toB: endB },
    ]);
  }
  return contextOf(docText(text), docParser.parse(text, fragments), new LineIndex(text));
}

/**
 * The document-wide facts of an already parsed document: its reference
 * definitions, footnote numbering and heading ids. The live editor hands
 * its own tree and text (`lines` over its `Text`), whose frontmatter it
 * parsed as markdown: nothing before `start` counts. A pass over block
 * containers only — never inline content — so it stays cheap per edit.
 */
export function contextOf(doc: DocText, tree: Tree, lines: Lines, start = 0): DocContext {
  const top = tree.topNode;
  const refs = new Map<string, LinkDef>();
  const footnoteDefs = new Map<string, SyntaxNode>();
  const headings: SyntaxNode[] = [];
  eachBlock(top, false, (n) => {
    if (n.from < start) return;
    if (n.name === "LinkReference") {
      const def = linkReference(n, doc);
      if (def !== null && !refs.has(def[0])) refs.set(def[0], def[1]);
    } else if (n.name === "FootnoteDefinition") {
      const label = normalizeLabel(footnoteLabel(n, doc));
      if (!footnoteDefs.has(label)) footnoteDefs.set(label, n);
      // Definitions nest reference definitions too; headings in one render
      // with the footnotes, after the body.
      eachBlock(n, true, (d) => {
        if (d.name !== "LinkReference") return;
        const def = linkReference(d, doc);
        if (def !== null && !refs.has(def[0])) refs.set(def[0], def[1]);
      });
    } else if (HEADING.test(n.name)) {
      headings.push(n);
    }
  });
  const inline: InlineOptions = { refs: refs.size === 0 ? null : (l) => refs.get(l) ?? null };

  // Footnotes number by first reference, in document order; a reference to
  // no definition stays text. Only blocks holding `[^` are walked.
  const footnoteRefs = new Map<number, { label: string; n: number; nth: number }>();
  const footnotes: DocContext["footnotes"] = [];
  const byLabel = new Map<string, DocContext["footnotes"][number] & { n: number }>();
  const visitRefs = (n: SyntaxNode): void => {
    for (let r = n.firstChild; r !== null; r = r.nextSibling) {
      if (r.name === "FootnoteDefinition") continue;
      if (r.name !== "FootnoteReference") {
        visitRefs(r);
        continue;
      }
      const label = normalizeLabel(doc.sliceString(r.from + 2, r.to - 1));
      const node = footnoteDefs.get(label);
      if (node === undefined) continue;
      let f = byLabel.get(label);
      if (f === undefined) {
        f = { label: footnoteLabel(node, doc), node, refs: 0, n: footnotes.length + 1 };
        byLabel.set(label, f);
        footnotes.push(f);
      }
      f.refs += 1;
      footnoteRefs.set(r.from, { label: f.label, n: f.n, nth: f.refs });
    }
  };
  const scanRefs = (scope: SyntaxNode): void => {
    for (let c = scope.firstChild; c !== null; c = c.nextSibling) {
      if (c.from < start || c.name === "FootnoteDefinition") continue;
      if (!doc.sliceString(c.from, c.to).includes("[^")) continue;
      visitRefs(c);
    }
  };
  if (footnoteDefs.size > 0) {
    scanRefs(top);
    // A reference inside a rendered footnote numbers after the body's.
    for (let i = 0; i < footnotes.length; i++) scanRefs(footnotes[i].node);
  }

  // Heading ids: the body in order, then headings inside the rendered
  // footnotes (comrak renders those last).
  const slugs = new Slugger();
  const outline = headings.map((n) => outlineEntry(n, doc, inline, slugs));
  const headingIds = new Map(outline.map((h) => [h.from, h.id]));
  for (const f of footnotes)
    eachBlock(f.node, false, (n) => {
      if (HEADING.test(n.name)) headingIds.set(n.from, outlineEntry(n, doc, inline, slugs).id);
    });

  return { doc, lines, tree, refs, footnoteDefs, headingIds, outline, footnoteRefs, footnotes, inline };
}

/**
 * What a stretch of the document's rendering reads beyond its own text:
 * the ids of the headings in it, the numbers of the footnote references
 * in it, and — when it could hold a reference link — every definition. A
 * block's text plus this is its rendering's identity (both views keep a
 * block's DOM while it holds).
 */
export function depsOf(cx: DocContext): (from: number, to: number, src: string) => string {
  const headings = [...cx.headingIds.entries()].sort((a, b) => a[0] - b[0]);
  const refs = [...cx.footnoteRefs.entries()].sort((a, b) => a[0] - b[0]);
  const defs =
    cx.refs.size === 0
      ? ""
      : [...cx.refs.entries()].map(([k, d]) => `${k}\u0000${d.url}\u0000${d.title ?? ""}`).join("\u0001");
  /** The first entry at or after `pos`. */
  const lowerBound = (list: readonly [number, unknown][], pos: number): number => {
    let lo = 0;
    let hi = list.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (list[mid][0] < pos) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  };
  return (from, to, src) => {
    let out = "";
    for (let k = lowerBound(headings, from); k < headings.length && headings[k][0] < to; k++)
      out += `#${headings[k][1]}`;
    for (let k = lowerBound(refs, from); k < refs.length && refs[k][0] < to; k++) {
      const f = refs[k][1];
      out += `^${f.label}:${f.n}:${f.nth}`;
    }
    if (defs !== "" && src.includes("[")) out += `\u0001${defs}`;
    return out;
  };
}

/** comrak's anchorizer: a repeated slug gets `-1`, `-2`, … */
class Slugger {
  private used = new Set<string>();
  take(base: string): string {
    let id = base;
    for (let k = 1; this.used.has(id); k++) id = `${base}-${k}`;
    this.used.add(id);
    return id;
  }
}

/** One heading of a document's outline. */
export interface OutlineEntry {
  from: number;
  level: number;
  text: string;
  /** Its anchor, unprefixed (`#id` links to it; the element id carries
   *  `user-content-`). */
  id: string;
}

function outlineEntry(n: SyntaxNode, doc: DocText, opts: InlineOptions, slugs: Slugger): OutlineEntry {
  // A line break in a (setext) heading reads as a space, as comrak's
  // collected text has it.
  const text = plainText(headingInline(n, doc, opts)).replace(/\n/g, " ");
  return {
    from: n.from,
    level: Number(n.name.slice(-1)),
    text: text.replace(/\s+/g, " ").trim(),
    id: slugs.take(githubSlug(text)),
  };
}

/** The headings of a parsed document with their anchors — the same ids
 *  the reading render gives them, for a tree the live editor holds (which
 *  parses frontmatter as text: nothing before `start` counts). */
export function outlineOf(tree: Tree, doc: DocText, start = 0): OutlineEntry[] {
  const refs = new Map<string, LinkDef>();
  const headings: SyntaxNode[] = [];
  eachBlock(tree.topNode, false, (n) => {
    if (n.from < start) return;
    if (n.name === "LinkReference") {
      const def = linkReference(n, doc);
      if (def !== null && !refs.has(def[0])) refs.set(def[0], def[1]);
    } else if (HEADING.test(n.name)) {
      headings.push(n);
    }
  });
  const opts: InlineOptions = { refs: refs.size === 0 ? null : (l) => refs.get(l) ?? null };
  const slugs = new Slugger();
  return headings.map((n) => outlineEntry(n, doc, opts, slugs));
}

// --- blocks ---------------------------------------------------------------------------

/** A heading's inline content: between an ATX heading's marks, or a setext
 *  heading's lines above its underline. */
export function headingInline(node: SyntaxNode, doc: DocText, opts: InlineOptions): Inline[] {
  const marks = node.getChildren("HeaderMark");
  let from = node.from;
  let to = node.to;
  if (node.name.startsWith("ATX")) {
    if (marks.length > 0) from = marks[0].to;
    if (marks.length > 1) to = marks[marks.length - 1].from;
  } else if (marks.length > 0) {
    to = marks[marks.length - 1].from;
  }
  const inline = inlineOf(node, from, to, doc, opts);
  trimEdges(inline);
  return inline;
}

function paragraph(node: SyntaxNode, from: number, cx: DocContext): Block {
  const inline = inlineOf(node, from, node.to, cx.doc, cx.inline);
  trimEdges(inline);
  return { kind: "paragraph", from, to: node.to, inline };
}

/** Whether a line between two blocks is blank: nothing but container
 *  markers (`>`) and whitespace. */
function blankBetween(cx: DocContext, a: number, b: number): boolean {
  const from = cx.lines.lineOf(a);
  const to = cx.lines.lineOf(b);
  for (let l = from + 1; l < to; l++) if (/^[\s>]*$/.test(cx.lines.lineText(l))) return true;
  return false;
}

const ALERT = /^> \[!(note|tip|important|warning|caution)\][ \t]*([^\n]*)/i;
const ALERT_TITLES: Record<AlertType, string> = {
  note: "Note",
  tip: "Tip",
  important: "Important",
  warning: "Warning",
  caution: "Caution",
};

/** The blocks of a container node (its children minus markers). */
function children(node: SyntaxNode, cx: DocContext): Block[] {
  const out: Block[] = [];
  for (let c = node.firstChild; c !== null; c = c.nextSibling) {
    const b = blockOf(c, cx);
    if (b !== null) out.push(b);
  }
  return out;
}

function listOf(node: SyntaxNode, cx: DocContext): Block {
  const itemNodes = node.getChildren("ListItem");
  let tight = true;
  const items: Item[] = itemNodes.map((it, i) => {
    if (i > 0 && blankBetween(cx, itemNodes[i - 1].to, it.from)) tight = false;
    let task: Item["task"] = null;
    const blocks: Block[] = [];
    let prevEnd = -1;
    for (let c = it.firstChild; c !== null; c = c.nextSibling) {
      // Markers are chrome, not blocks: a quoted item's `>` on a blank line
      // must not hide the blank line from the tightness check.
      if (c.name === "ListMark" || c.name === "QuoteMark") continue;
      if (prevEnd >= 0 && blankBetween(cx, prevEnd, c.from)) tight = false;
      prevEnd = c.to;
      if (c.name === "Task" && blocks.length === 0) {
        const marker = c.getChild("TaskMarker");
        if (marker !== null) {
          task = /x/i.test(cx.doc.sliceString(marker.from, marker.to)) ? "done" : "todo";
          const inline = inlineOf(c, marker.to, c.to, cx.doc, cx.inline);
          trimEdges(inline);
          blocks.push({ kind: "paragraph", from: c.from, to: c.to, inline });
          continue;
        }
      }
      // `- [ ]` with nothing after it: lezer reads a paragraph, comrak (and
      // GitHub) an empty task.
      if (c.name === "Paragraph" && blocks.length === 0 && /^\[[ xX]\]$/.test(cx.doc.sliceString(c.from, c.to))) {
        task = /x/i.test(cx.doc.sliceString(c.from, c.to)) ? "done" : "todo";
        continue;
      }
      const b = blockOf(c, cx);
      if (b !== null) blocks.push(b);
    }
    return { from: it.from, to: it.to, task, children: blocks };
  });
  const ordered = node.name === "OrderedList";
  let start = 1;
  if (ordered) {
    const mark = itemNodes[0]?.getChild("ListMark");
    if (mark !== null && mark !== undefined) start = parseInt(cx.doc.sliceString(mark.from, mark.to), 10) || 0;
  }
  return { kind: "list", from: node.from, to: node.to, ordered, start, tight, items };
}

/** A fence's content: its CodeText lines joined (container markers are
 *  siblings, never inside), less the fence's own indentation, with the
 *  closing newline comrak keeps. */
function fenceText(node: SyntaxNode, cx: DocContext): string {
  const texts = node.getChildren("CodeText");
  const marks = node.getChildren("CodeMark");
  const open = marks[0];
  let text = texts.map((t) => cx.doc.sliceString(t.from, t.to)).join("");
  if (open !== undefined && texts.length > 0) {
    const col = (pos: number): number => pos - cx.lines.lineStart(cx.lines.lineOf(pos));
    const indent = col(open.from) - col(texts[0].from);
    if (indent > 0) {
      const strip = new RegExp(`^ {1,${indent}}`, "gm");
      text = text.replace(strip, "");
    }
  }
  const openLine = cx.lines.lineOf(node.from);
  const closed = marks.length > 1;
  const lastLine = closed ? cx.lines.lineOf(marks[marks.length - 1].from) - 1 : cx.lines.lineOf(node.to);
  return lastLine > openLine ? `${text}\n` : text;
}

/** Raw HTML as written, less the quote markers its lines carry inside a
 *  blockquote. */
function htmlSource(node: SyntaxNode, cx: DocContext): string {
  let out = "";
  let pos = node.from;
  for (const q of node.getChildren("QuoteMark")) {
    out += cx.doc.sliceString(pos, q.from);
    pos = q.to;
    if (cx.doc.sliceString(pos, pos + 1) === " ") pos++;
  }
  return out + cx.doc.sliceString(pos, node.to);
}

/** The model of one block node; null for what renders nothing in place
 *  (definitions, markers). */
export function blockOf(node: SyntaxNode, cx: DocContext): Block | null {
  const { from, to } = node;
  switch (node.name) {
    case "Paragraph":
      return paragraph(node, from, cx);
    case "ATXHeading1":
    case "ATXHeading2":
    case "ATXHeading3":
    case "ATXHeading4":
    case "ATXHeading5":
    case "ATXHeading6":
    case "SetextHeading1":
    case "SetextHeading2":
      return {
        kind: "heading",
        from,
        to,
        level: Number(node.name.slice(-1)),
        inline: headingInline(node, cx.doc, cx.inline),
        id: cx.headingIds.get(from) ?? "",
      };
    case "HorizontalRule":
      return { kind: "rule", from, to };
    case "Blockquote": {
      const markerLine = cx.lines.lineOf(from);
      const m = ALERT.exec(cx.doc.sliceString(from, cx.lines.lineStart(markerLine + 1)));
      if (m === null) return { kind: "quote", from, to, children: children(node, cx) };
      // The marker line is the alert's; its content starts on the next line
      // (a paragraph lezer began on the marker line keeps only its rest).
      const type = m[1].toLowerCase() as AlertType;
      const title = decodeEntities(unescapeBackslashes(m[2].trim()));
      const markerEnd = cx.lines.lineStart(markerLine + 1);
      const blocks: Block[] = [];
      for (let c = node.firstChild; c !== null; c = c.nextSibling) {
        if (c.from < markerEnd) {
          if (c.name === "Paragraph" && c.to > markerEnd) {
            const p = paragraph(c, markerEnd, cx);
            if (p.kind === "paragraph" && p.inline.length > 0) blocks.push(p);
          }
          continue;
        }
        const b = blockOf(c, cx);
        if (b !== null) blocks.push(b);
      }
      return {
        kind: "alert",
        from,
        to,
        type,
        title: title === "" ? ALERT_TITLES[type] : title,
        children: blocks,
      };
    }
    case "BulletList":
    case "OrderedList":
      return listOf(node, cx);
    case "FencedCode": {
      const info = node.getChild("CodeInfo");
      const lang = info === null ? "" : decodeEntities(unescapeBackslashes(cx.doc.sliceString(info.from, info.to)));
      const text = fenceText(node, cx);
      if (lang === "math") return { kind: "math", from, to, source: text };
      return { kind: "code", from, to, lang, text };
    }
    case "CodeBlock": {
      const text = node
        .getChildren("CodeText")
        .map((t) => cx.doc.sliceString(t.from, t.to))
        .join("");
      return { kind: "code", from, to, lang: "", text: `${text}\n` };
    }
    case "MathBlock": {
      // An unclosed block (its lines left the item before a closer) is
      // prose, as the live editor shows it.
      if (mathDelimiters(node) === null) return paragraph(node, from, cx);
      // The literal the server hands over: the promoted fence's content,
      // from the line after the opener, ending in a newline.
      const source = (mathSource(node, cx.doc) ?? "").replace(/^[ \t]*\n/, "");
      return { kind: "math", from, to, source: source.endsWith("\n") ? source : `${source}\n` };
    }
    case "Table": {
      const table = tableModel(node, cx.doc, cx.inline);
      return table === null ? paragraph(node, from, cx) : { kind: "table", from, to, table };
    }
    case "HTMLBlock":
    case "ProcessingInstructionBlock":
      return { kind: "html", from, to, source: htmlSource(node, cx) };
    // A comment block renders nothing (the sanitizer strips comments).
    default:
      return null;
  }
}

// --- raw HTML around markdown ------------------------------------------------------

/** Container tags an HTML block may open around markdown (`<details>` ⏎
 *  text ⏎ `</details>`, a README's `<div align="center">`). */
const WRAPPER_TAGS = new Set([
  "article", "aside", "blockquote", "center", "details", "div", "dl", "figure", "footer",
  "header", "main", "nav", "section", "summary", "table",
]);

/** Push the wrapper tags `html` opens onto `stack`, pop the ones it closes
 *  (comments ignored). */
export function trackOpenTags(html: string, stack: string[]): void {
  const bare = html.includes("<!--") ? html.replace(/<!--[\s\S]*?(-->|$)/g, "") : html;
  for (const m of bare.matchAll(/<(\/?)([A-Za-z][A-Za-z0-9-]*)\b[^>]*?(\/?)>/g)) {
    const name = m[2].toLowerCase();
    if (!WRAPPER_TAGS.has(name) || m[3] === "/") continue;
    if (m[1] === "/") {
      const i = stack.lastIndexOf(name);
      if (i >= 0) stack.length = i;
    } else {
      stack.push(name);
    }
  }
}

/**
 * Siblings grouped for rendering: an HTML block that leaves a wrapper open
 * takes the blocks after it until its tags close (or its container ends).
 * comrak writes such a run out as one stretch of HTML that its sanitizer
 * parses whole — so the markdown lands INSIDE the `<details>` — and the
 * renderer draws a run as one raw island the same way.
 */
export function htmlRuns<T>(items: readonly T[], html: (item: T) => string | null): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < items.length; i++) {
    const run = [items[i]];
    const src = html(items[i]);
    if (src !== null) {
      const stack: string[] = [];
      trackOpenTags(src, stack);
      while (stack.length > 0 && i + 1 < items.length) {
        const next = items[++i];
        run.push(next);
        const more = html(next);
        if (more !== null) trackOpenTags(more, stack);
      }
    }
    out.push(run);
  }
  return out;
}

/** The raw HTML a block node holds, for `htmlRuns` (null for markdown). */
export function htmlOfNode(node: SyntaxNode, cx: DocContext): string | null {
  return node.name === "HTMLBlock" ? htmlSource(node, cx) : null;
}

/** The rendered footnotes, in number order, as blocks. */
export function footnotesOf(cx: DocContext): Footnote[] {
  return cx.footnotes.map((f, i) => ({
    from: f.node.from,
    to: f.node.to,
    label: f.label,
    n: i + 1,
    refs: f.refs,
    children: children(f.node, cx),
  }));
}
