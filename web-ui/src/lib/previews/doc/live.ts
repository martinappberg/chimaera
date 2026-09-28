/**
 * The pure half of live mode (`mdLive.ts` draws it): how a document splits
 * into the blocks that render or show as source, which of them the
 * selection reveals, what spacing a rendered block needs from its
 * neighbour, the height cache that keeps a block's swap from moving the
 * page, and where a click on rendered text lands in the source.
 *
 * A **segment** is one top-level unit of the document on whole lines: a
 * block node (or a run an HTML block opened, as the reader groups them),
 * plus the blank lines after it — so revealing a block never inserts a line
 * ABOVE the text you clicked, only below it. Frontmatter is a segment of its
 * own; reference definitions and comments, which render nothing and have
 * nothing rendered to click, always show as source; the tail of a document
 * the background parser has not reached yet is source until it has.
 */
import type { SyntaxNode, Tree, TreeFragment } from "@lezer/common";
import { htmlRuns, type LinedText } from "./model";
import { docParser } from "./parser";
import type { DocText } from "../mdTable";

/** What the segment code reads from a document (CodeMirror's `Text`). */
export type LiveDoc = LinedText & DocText;

export type SegmentKind =
  /** Rendered by the block renderer unless the selection touches it. */
  | "block"
  /** The leading YAML block: the properties panel unless touched. */
  | "front"
  /** Renders nothing in place (a definition, a comment): always source. */
  | "source"
  /** Past what the parser has reached: source until it gets there. */
  | "raw";

export interface Segment {
  kind: SegmentKind;
  /** Whole lines: the first line's start … the last line's end, the
   *  blank lines after the block included. */
  from: number;
  to: number;
  /** The block itself: its first line's start and its last node's end. */
  blockFrom: number;
  blockTo: number;
  /** The run's node names, comma-joined (part of its identity). */
  names: string;
}

/** Top-level nodes that render nothing and have no rendered stand-in to
 *  click: they stay visible source. (A footnote definition renders in the
 *  footnote section at the end, whose entry leads back to it — so in place
 *  it is an empty block, as in reading.) */
export const SILENT: ReadonlySet<string> = new Set(["LinkReference", "CommentBlock"]);

/** The document's top-level nodes after `start` (frontmatter the editor
 *  parsed as markdown is not body). */
export function topNodes(tree: Tree, start = 0): SyntaxNode[] {
  const out: SyntaxNode[] = [];
  for (let c = tree.topNode.firstChild; c !== null; c = c.nextSibling) if (c.from >= start) out.push(c);
  return out;
}

/** The top-level nodes of `tree` inside [from, to] (a segment's run). */
export function nodesIn(tree: Tree, from: number, to: number): SyntaxNode[] {
  const out: SyntaxNode[] = [];
  let c: SyntaxNode | null = tree.topNode.childAfter(from - 1) ?? tree.topNode.firstChild;
  while (c !== null && c.to < from) c = c.nextSibling;
  for (; c !== null && c.from <= to; c = c.nextSibling) if (c.from >= from) out.push(c);
  return out;
}

/** Whether a node the editor parsed inside the frontmatter (it reads the
 *  YAML as markdown) runs past its end: a fence or an HTML block opened in
 *  the YAML swallows the body lines after it. */
export function crossesFrontmatter(tree: Tree, fmEnd: number): boolean {
  if (fmEnd <= 0) return false;
  const last = tree.topNode.childBefore(fmEnd);
  return last !== null && last.to > fmEnd;
}

/**
 * The tree to segment and draw the body by when the editor's own cannot
 * serve (`crossesFrontmatter`): the document parsed as reading parses it,
 * the frontmatter blanked (lines and offsets kept), from `fragments` of the
 * last such parse when given. Null when the editor's tree serves.
 */
export function bodyTree(
  tree: Tree,
  doc: LiveDoc,
  fmEnd: number,
  fragments?: readonly TreeFragment[],
): Tree | null {
  if (!crossesFrontmatter(tree, fmEnd)) return null;
  const text = doc.sliceString(0, doc.length);
  return docParser.parse(text.slice(0, fmEnd).replace(/[^\n]/g, " ") + text.slice(fmEnd), fragments);
}

/**
 * Split `doc` into segments by its (possibly partial) syntax tree. `fmEnd`
 * is where frontmatter ends (0 = none): its segment ends on the closing
 * line, whatever the tree made of the YAML — body lines a node opened there
 * swallowed stay source (a caller draws the body from `bodyTree`'s tree, so
 * there are none). Segments are sorted, disjoint and line-aligned; lines
 * before the first block belong to it, lines after the last block to the
 * last one.
 */
export function segmentsOf(tree: Tree, doc: LiveDoc, fmEnd: number): Segment[] {
  const nodes = topNodes(tree, fmEnd);
  // A partial parse may have cut its last node short at the frontier.
  let rawFrom = -1;
  if (tree.length < doc.length) {
    const last = nodes.pop();
    rawFrom = last !== undefined ? last.from : Math.max(fmEnd, Math.min(tree.length, doc.length));
  }
  const runs = htmlRuns(nodes, (n) => (n.name === "HTMLBlock" ? doc.sliceString(n.from, n.to) : null));
  const out: Segment[] = [];
  const lineStart = (pos: number): number => doc.lineAt(pos).from;
  if (fmEnd > 0) {
    const end = doc.lineAt(Math.min(fmEnd, doc.length)).to;
    out.push({ kind: "front", from: 0, to: end, blockFrom: 0, blockTo: end, names: "Frontmatter" });
    const cross = crossesFrontmatter(tree, fmEnd) ? tree.topNode.childBefore(fmEnd) : null;
    if (cross !== null && end < doc.length) {
      const from = end + 1;
      out.push({ kind: "source", from, to: 0, blockFrom: from, blockTo: Math.max(from, cross.to), names: cross.name });
    }
  }
  for (const run of runs) {
    const first = run[0];
    const last = run[run.length - 1];
    const blockFrom = lineStart(first.from);
    const prev = out[out.length - 1];
    // A block sharing a line with the one before (never, for lezer's block
    // nodes, but a partial tree is odd) joins it rather than overlap.
    if (prev !== undefined && blockFrom <= prev.blockTo) {
      prev.blockTo = Math.max(prev.blockTo, last.to);
      prev.names += `,${run.map((n) => n.name).join(",")}`;
      if (prev.kind === "source" && run.some((n) => !SILENT.has(n.name))) prev.kind = "block";
      continue;
    }
    out.push({
      kind: run.every((n) => SILENT.has(n.name)) ? "source" : "block",
      from: out.length === 0 ? 0 : blockFrom,
      to: 0,
      blockFrom,
      blockTo: Math.max(last.to, blockFrom),
      names: run.map((n) => n.name).join(","),
    });
  }
  if (rawFrom >= 0) {
    const from = lineStart(Math.min(rawFrom, doc.length));
    const prev = out[out.length - 1];
    if (prev !== undefined && from <= prev.blockTo) {
      prev.kind = "raw";
      prev.blockTo = doc.length;
    } else {
      out.push({ kind: "raw", from: out.length === 0 ? 0 : from, to: 0, blockFrom: from, blockTo: doc.length, names: "" });
    }
  }
  // Each segment runs to the line before the next one; the last to the end.
  for (let i = 0; i < out.length; i++) {
    const next = out[i + 1];
    out[i].to = next === undefined ? doc.length : Math.max(out[i].blockTo, next.from - 1);
  }
  return out;
}

/** Index of the segment holding `pos` (-1 when none does). */
export function segmentAt(segs: readonly Segment[], pos: number): number {
  let lo = 0;
  let hi = segs.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (segs[mid].to < pos) lo = mid + 1;
    else if (segs[mid].from > pos) hi = mid - 1;
    else return mid;
  }
  return -1;
}

/** Which segments show as source: every one a selection range touches
 *  (its lines, inclusive), plus every one that never renders. */
export function revealedOf(
  segs: readonly Segment[],
  ranges: readonly { from: number; to: number }[],
): boolean[] {
  const out = segs.map((s) => s.kind === "source" || s.kind === "raw");
  for (const r of ranges) {
    let i = segmentAt(segs, r.from);
    if (i < 0) {
      // Between segments cannot happen (they tile the document) — but a
      // range past the end touches the last one.
      i = r.from > (segs[segs.length - 1]?.to ?? 0) ? segs.length - 1 : 0;
    }
    for (; i >= 0 && i < segs.length && segs[i].from <= r.to; i++) out[i] = true;
  }
  return out;
}

/** A line that is one image reference and nothing else — `![alt](target)`
 *  or `![[name]]` — the source of a figure (an image-syntax block). Revealed,
 *  its figure stays drawn under it rather than collapsing to the line. */
const FIGURE = /^ {0,3}(!\[(?:[^\]\\\n]|\\.)*\]\((?:[^()\\\n]|\\.)*\)|!\[\[[^\]\n]+\]\])[ \t]*$/;

export function isFigureLine(text: string): boolean {
  return FIGURE.test(text);
}

// --- the spacing between rendered blocks -------------------------------------------

/** One element of a block's edge: its tag and the classes its margins
 *  depend on. */
export interface Shape {
  tag: string;
  cls: string;
}

const ALERT = /^> \[!(note|tip|important|warning|caution)\]/i;
/** Tags raw HTML may open that keep their margins in the document CSS. */
const HTML_BLOCK_TAGS = new Set([
  "p", "div", "details", "table", "ul", "ol", "h1", "h2", "h3", "h4", "h5", "h6", "blockquote",
  "pre", "hr", "figure", "section", "center", "dl",
]);

function shape(tag: string, cls = ""): Shape {
  return { tag, cls };
}

/** Whether a line between two positions is blank (markers only). */
function blankBetween(doc: LiveDoc, a: number, b: number): boolean {
  const from = doc.lineAt(a).number;
  const to = doc.lineAt(b).number;
  for (let l = from + 1; l < to; l++) if (/^[\s>]*$/.test(doc.line(l).text)) return true;
  return false;
}

function itemBlocks(item: SyntaxNode): SyntaxNode[] {
  const out: SyntaxNode[] = [];
  for (let c = item.firstChild; c !== null; c = c.nextSibling)
    if (c.name !== "ListMark" && c.name !== "QuoteMark") out.push(c);
  return out;
}

/** The renderer's list tightness (`model.ts listOf`): loose when a blank
 *  line separates two items, or two blocks of one item. */
function listTight(items: readonly SyntaxNode[], doc: LiveDoc): boolean {
  for (let i = 0; i < items.length; i++) {
    if (i > 0 && blankBetween(doc, items[i - 1].to, items[i].from)) return false;
    const kids = itemBlocks(items[i]);
    for (let k = 1; k < kids.length; k++) if (blankBetween(doc, kids[k - 1].to, kids[k].from)) return false;
  }
  return true;
}

function htmlShape(source: string, edge: "head" | "tail"): Shape {
  const bare = source.replace(/<!--[\s\S]*?(-->|$)/g, "");
  const tags = [...bare.matchAll(/<\/?([A-Za-z][A-Za-z0-9-]*)/g)].map((m) => m[1].toLowerCase());
  const tag = edge === "head" ? tags[0] : tags[tags.length - 1];
  return shape(tag !== undefined && HTML_BLOCK_TAGS.has(tag) ? tag : "div");
}

/**
 * The chain of elements at one edge of a block as the renderer draws it —
 * the outermost first — along which CSS margins collapse: a loose list's
 * first item's paragraph lends the list its top margin, a quote's padding
 * stops the chain. Empty for what renders nothing.
 */
export function edgeChain(node: SyntaxNode, doc: LiveDoc, edge: "head" | "tail"): Shape[] {
  const name = node.name;
  if (name === "Paragraph" || name === "Task" || name === "MathBlock") return [shape("p")];
  if (/^(ATX|Setext)Heading\d$/.test(name)) return [shape(`h${name.slice(-1)}`)];
  switch (name) {
    case "HorizontalRule":
      return [shape("hr")];
    case "Blockquote": {
      const m = ALERT.exec(doc.sliceString(node.from, Math.min(node.to, node.from + 32)));
      return [m === null ? shape("blockquote") : shape("div", `markdown-alert markdown-alert-${m[1].toLowerCase()}`)];
    }
    case "FencedCode": {
      const info = node.getChild("CodeInfo");
      const lang = info === null ? "" : doc.sliceString(info.from, info.to).trim().split(/\s/)[0];
      if (lang === "math") return [shape("p")];
      return [lang === "mermaid" ? shape("div", "md-mermaid") : shape("pre")];
    }
    case "CodeBlock":
      return [shape("pre")];
    case "Table":
      return [shape("table")];
    case "HTMLBlock":
    case "ProcessingInstructionBlock":
      return [htmlShape(doc.sliceString(node.from, node.to), edge)];
    case "BulletList":
    case "OrderedList": {
      const list = shape(name === "BulletList" ? "ul" : "ol");
      const items = node.getChildren("ListItem");
      const item = edge === "head" ? items[0] : items[items.length - 1];
      if (item === undefined) return [list];
      const kids = itemBlocks(item);
      const kid = edge === "head" ? kids[0] : kids[kids.length - 1];
      const out = [list, shape("li")];
      if (kid === undefined) return out;
      // A tight item's paragraphs are bare text: the chain ends at the item.
      if ((kid.name === "Paragraph" || kid.name === "Task") && listTight(items, doc)) return out;
      return out.concat(edgeChain(kid, doc, edge));
    }
    default:
      return [];
  }
}

/** Both edges of a segment's run: an HTML-opened run's wrapper encloses the
 *  rest, so it is both. */
export function runEdges(run: readonly SyntaxNode[], doc: LiveDoc): { head: Shape[]; tail: Shape[] } {
  if (run.length === 0) return { head: [], tail: [] };
  const head = edgeChain(run[0], doc, "head");
  if (run.length > 1) return { head, tail: head };
  return { head, tail: edgeChain(run[0], doc, "tail") };
}

/** A chain as a short stable string (widget identity). */
export function shapeKey(chain: readonly Shape[]): string {
  return chain.map((s) => (s.cls === "" ? s.tag : `${s.tag}.${s.cls.replace(/\s+/g, ".")}`)).join(">");
}

// --- heights ------------------------------------------------------------------------

/** cyrb53: a 53-bit string hash (a cache key, never a security boundary). */
export function hashString(s: string, seed = 0): string {
  let h1 = 0xdeadbeef ^ seed;
  let h2 = 0x41c6ce57 ^ seed;
  for (let i = 0; i < s.length; i++) {
    const ch = s.charCodeAt(i);
    h1 = Math.imul(h1 ^ ch, 2654435761);
    h2 = Math.imul(h2 ^ ch, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return (4294967296 * (2097151 & h2) + (h1 >>> 0)).toString(36);
}

/**
 * Measured block heights, by what the block renders and the width and type
 * size it rendered at. A widget CodeMirror has not drawn yet (off screen, or
 * just re-created after its block lost the cursor) estimates from here, so
 * the height map is exact before the block paints and nothing below moves.
 * Bounded: least recently used entries go first.
 */
export class HeightCache {
  private readonly map = new Map<string, number>();
  constructor(private readonly max = 4000) {}
  get(key: string): number | undefined {
    const h = this.map.get(key);
    if (h !== undefined) {
      this.map.delete(key);
      this.map.set(key, h);
    }
    return h;
  }
  set(key: string, height: number): void {
    this.map.delete(key);
    this.map.set(key, height);
    if (this.map.size > this.max) {
      const oldest = this.map.keys().next().value;
      if (oldest !== undefined) this.map.delete(oldest);
    }
  }
  get size(): number {
    return this.map.size;
  }
}

// --- clicks: rendered text → source offset -----------------------------------------------

/** Syntax a reader never sees as text: marks, destinations, tags. */
const HIDDEN = new Set([
  "HeaderMark", "EmphasisMark", "StrikethroughMark", "CodeMark", "CodeInfo", "LinkMark",
  "QuoteMark", "ListMark", "TaskMarker", "TableDelimiter", "HardBreak", "MathMark",
  "FootnoteMark", "WikilinkMark", "HTMLTag", "Comment", "ProcessingInstruction", "Image",
]);
const LEADING = new Set(["HeaderMark", "ListMark", "QuoteMark", "TaskMarker"]);
/** Hidden only inside a link or image (an autolink shows its URL). */
const LINK_PARTS = new Set(["URL", "LinkTitle", "LinkLabel"]);

/**
 * The text of [from, to] a reader sees rendered, with each character's
 * source offset: the source minus its syntax (marks, a link's destination,
 * raw tags, an escape's backslash). A rendered prefix aligns against it.
 */
export function visibleSource(
  tree: Tree,
  doc: DocText,
  from: number,
  to: number,
): { text: string; at: number[] } {
  const hidden: [number, number][] = [];
  tree.iterate({
    from,
    to,
    enter: (n) => {
      const name = n.name;
      if (HIDDEN.has(name)) {
        // A line's leading mark takes the spaces after it along (the
        // renderer trims them).
        let end = n.to;
        if (LEADING.has(name)) end += /^[ \t]*/.exec(doc.sliceString(n.to, n.to + 8))?.[0].length ?? 0;
        hidden.push([n.from, end]);
        return false;
      }
      if (LINK_PARTS.has(name)) {
        const parent = n.node.parent?.name;
        if (parent === "Link" || parent === "Image") {
          hidden.push([n.from, n.to]);
          return false;
        }
      }
      if (name === "Escape") {
        hidden.push([n.from, n.from + 1]);
        return false;
      }
      return undefined;
    },
  });
  hidden.sort((a, b) => a[0] - b[0]);
  const src = doc.sliceString(from, to);
  let text = "";
  const at: number[] = [];
  let h = 0;
  for (let i = 0; i < src.length; i++) {
    const pos = from + i;
    while (h < hidden.length && hidden[h][1] <= pos) h++;
    if (h < hidden.length && hidden[h][0] <= pos) continue;
    text += src[i];
    at.push(pos);
  }
  return { text, at };
}

/** How far a rendered character may sit from where the last one matched. */
const ALIGN_WINDOW = 48;
const WS = /\s/;

/**
 * Where a rendered prefix ends in `visible` (an index into it): each
 * rendered character matched greedily against the source's next ones, any
 * whitespace run against any whitespace run (a soft break renders as a
 * space). A rendered character with no source near by — a glyph an
 * equation drew, a list number — is skipped.
 */
export function alignPrefix(visible: string, prefix: string): number {
  let at = 0;
  const p = prefix.replace(/\s+/g, " ");
  for (let k = 0; k < p.length; k++) {
    const ch = p[k];
    const limit = Math.min(visible.length, at + ALIGN_WINDOW);
    let j = at;
    if (ch === " ") {
      while (j < limit && !WS.test(visible[j])) j++;
      if (j < limit) {
        while (j < visible.length && WS.test(visible[j])) j++;
        at = j;
      }
      continue;
    }
    while (j < limit && visible[j] !== ch) j++;
    if (j < limit) at = j + 1;
  }
  return at;
}

/** The source offset a rendered prefix ends at, inside [from, to]. */
export function sourceOffset(
  tree: Tree,
  doc: DocText,
  from: number,
  to: number,
  prefix: string,
): number {
  const { text, at } = visibleSource(tree, doc, from, to);
  if (text.length === 0) return from;
  const i = alignPrefix(text, prefix);
  if (i >= text.length) return Math.min(to, at[text.length - 1] + 1);
  return at[i];
}
