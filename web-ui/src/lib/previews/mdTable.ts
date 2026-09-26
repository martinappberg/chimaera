/**
 * The inline model every markdown surface renders from, and the GFM table
 * model built on it — both from the shared syntax tree (`doc/parser.ts`),
 * as plain data. Nothing here touches the DOM: the live preview's table
 * widget (mdLive.ts) and the reading renderer (`doc/render.ts`) turn the
 * model into elements with createElement, so no document text is ever
 * injected as markup (an equation goes through the shared KaTeX policy,
 * raw HTML through the sanitizer).
 *
 * Inline: every syntax child as a node, the text between them decoded and
 * trimmed at line edges the way CommonMark reads a paragraph; a reference
 * link resolves through the document's definitions when the caller has
 * them. A table records every cell's source position (so a press on the
 * rendered cell can land the cursor in that cell's text).
 *
 * Shapes, from lezer's GFM extension: a Table holds a TableHeader, ONE
 * TableDelimiter node for the whole `|:--|--:|` row, and TableRows; cells
 * are TableCell nodes between `|` TableDelimiters; an EMPTY cell emits no
 * node at all (only its pipes); a leading or trailing pipe is optional; a
 * row shorter than the header is padded with empty cells and extra cells
 * are dropped (GFM). A quoted table carries its per-line QuoteMarks as
 * children of the Table, beside the rows. Inside a cell comrak unescapes
 * `\|` before any inline parsing — so also inside a code span or an
 * equation, where lezer's Escape node never reaches.
 */
import type { SyntaxNode } from "@lezer/common";
import { isDisplayMath, isMath, mathSource } from "./mdMath";
import { decodeEntities, decodeEntity, unescapeBackslashes } from "./doc/entities";

/** The one thing the model reads from a document: CodeMirror's `Text`, or
 *  a plain string wrapped (`doc/model.ts`). */
export interface DocText {
  sliceString(from: number, to?: number): string;
}

export type Align = "left" | "center" | "right" | null;

export type Inline =
  /** Plain text, entities and escapes decoded; a soft line break stays a
   *  `\n`. */
  | { kind: "text"; text: string }
  | { kind: "strong" | "em" | "strike" | "code"; children: Inline[] }
  | { kind: "link"; url: string; title: string | null; children: Inline[] }
  /** `![alt](url)`; `source` is what shows when the URL may not render. */
  | { kind: "image"; url: string; alt: string; title: string | null; source: string }
  | { kind: "math"; source: string; display: boolean }
  /** A hard line break (two trailing spaces, or a backslash). */
  | { kind: "break" }
  /** One raw HTML tag as written — never markup until sanitized. */
  | { kind: "html"; source: string }
  /** `[^label]` at source offset `at`; whether it names a definition (and
   *  its number) is the document's to say. */
  | { kind: "footnote"; label: string; source: string; at: number }
  /** `[[target#heading|alias]]`, or an `![[embed]]`. */
  | {
      kind: "wikilink";
      target: string;
      heading: string | null;
      alias: string | null;
      embed: boolean;
      source: string;
    };

/** A link reference definition (`[label]: url "title"`). */
export interface LinkDef {
  url: string;
  title: string | null;
}

export interface InlineOptions {
  /** The document's reference definitions, by normalized label; without
   *  it (the live table widget) a reference-style link stays source. */
  refs?: ((label: string) => LinkDef | null) | null;
  /** Inside a GFM table cell, where comrak unescapes `\|` everywhere. */
  cell?: boolean;
}

export interface CellModel {
  /** Where the cell's text begins — an empty cell points just past its
   *  opening pipe (and the space after it), where typing would go. */
  from: number;
  inline: Inline[];
}

export interface RowModel {
  /** The cells in column order, padded to the header's count (GFM). */
  cells: CellModel[];
  /** How many columns the source row actually has: one per closing `|`,
   *  plus an open last one. The cells beyond are padding with no text to
   *  land in — `completeRow` writes them before the cursor goes there. */
  present: number;
  /** The row's end with trailing whitespace excluded, its line end, and
   *  whether a `|` closes it. */
  end: number;
  to: number;
  closed: boolean;
}

export interface TableModel {
  from: number;
  to: number;
  /** The table's source text, the widget's identity: a table whose text is
   *  unchanged keeps its rendered DOM across rebuilds. */
  source: string;
  align: Align[];
  header: RowModel;
  rows: RowModel[];
}

const ALIGN_RE = /^(:?)-+(:?)$/;

/** `|:--|--:|:-:|` → left / right / center per column; a bare `---` is
 *  null. Mirrors what comrak writes as the `align` attribute. */
export function alignments(delimiterRow: string): Align[] {
  return delimiterRow
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((seg) => {
      const m = ALIGN_RE.exec(seg.trim());
      if (m === null) return null;
      if (m[1] !== "" && m[2] !== "") return "center";
      if (m[2] !== "") return "right";
      if (m[1] !== "") return "left";
      return null;
    });
}

/** A link's destination as written: `[a](<x y>)` wraps a destination that
 *  holds spaces in angle brackets, which lezer's URL node keeps. */
export function linkDestination(raw: string): string {
  return raw.startsWith("<") && raw.endsWith(">") ? raw.slice(1, -1) : raw;
}

/** comrak's `unescape_pipes`: `\|` is a pipe, and a backslash that is
 *  itself escaped (`\\|`) protects nothing. Applied where lezer leaves the
 *  text raw — a code span, an equation; plain text has the Escape node. */
function unescapePipes(s: string): string {
  return s.replace(/\\([\\|])/g, (m, ch: string) => (ch === "|" ? "|" : m));
}

/** CommonMark's label matching: case-folded, inner whitespace collapsed. */
export function normalizeLabel(label: string): string {
  return label.trim().replace(/[ \t\r\n]+/g, " ").toLowerCase().toUpperCase();
}

/** A title (`"t"`, `'t'`, `(t)`) without its delimiters, decoded. */
export function titleText(raw: string): string {
  return decodeEntities(unescapeBackslashes(raw.slice(1, -1)));
}

/** A destination as the link means it: brackets off, escapes and entities
 *  decoded (comrak's href escaping happens at render). */
export function destinationText(raw: string): string {
  return decodeEntities(unescapeBackslashes(linkDestination(raw)));
}

/** What an inline tree reads as in plain text — an image's alt, a
 *  heading's slug source (comrak's `collect_text`: footnote marks and raw
 *  tags contribute nothing, an equation its LaTeX). */
export function plainText(inline: readonly Inline[]): string {
  let out = "";
  for (const i of inline) {
    switch (i.kind) {
      case "text":
        out += i.text;
        break;
      case "image":
        out += i.alt;
        break;
      case "math":
        out += i.source;
        break;
      case "break":
        out += "\n";
        break;
      case "wikilink":
        out += i.alias ?? i.source;
        break;
      case "html":
      case "footnote":
        break;
      default:
        out += plainText(i.children);
    }
  }
  return out;
}

/** The text of `node`'s [from, to] with its QuoteMark children — and the
 *  space after each — cut out: a code span that crosses a quoted line. */
function sliceWithoutQuotes(node: SyntaxNode, from: number, to: number, doc: DocText): string {
  let out = "";
  let pos = from;
  for (const q of node.getChildren("QuoteMark")) {
    if (q.from < pos || q.to > to) continue;
    out += doc.sliceString(pos, q.from);
    pos = q.to;
    if (doc.sliceString(pos, pos + 1) === " ") pos++;
  }
  return out + doc.sliceString(pos, to);
}

/** A wikilink's parts: `target#heading|alias` between its marks. */
function wikilinkParts(inner: string): {
  target: string;
  heading: string | null;
  alias: string | null;
} {
  const bar = inner.indexOf("|");
  const alias = bar >= 0 ? inner.slice(bar + 1).trim() : "";
  const ref = bar >= 0 ? inner.slice(0, bar) : inner;
  const hash = ref.indexOf("#");
  const heading = hash >= 0 ? ref.slice(hash + 1).trim() : "";
  return {
    target: (hash >= 0 ? ref.slice(0, hash) : ref).trim(),
    heading: heading === "" ? null : heading,
    alias: alias === "" ? null : alias,
  };
}

/**
 * The inline tree of [from, to] inside `node`: the syntax children as model
 * nodes, with the plain text between them. Delimiter marks are dropped,
 * escapes and entities decode, a comment is nothing, a quote marker (a
 * quoted paragraph's later lines) is container chrome, and a soft break
 * keeps its `\n` with the lines' edge whitespace trimmed. A reference-style
 * link resolves through `opts.refs`, else stays the text it is; anything
 * the model has no node for (superscript, an emoji shortcode — literal on
 * GitHub) stays as its source text.
 */
export function inlineOf(
  node: SyntaxNode,
  from: number,
  to: number,
  doc: DocText,
  opts: InlineOptions = {},
): Inline[] {
  const out: Inline[] = [];
  // After a quote marker or a hard break the next text starts a line,
  // whose indentation is container chrome (CommonMark strips it).
  let lineStart = false;
  const push = (t: string): void => {
    if (t === "") return;
    const last = out[out.length - 1];
    if (last !== undefined && last.kind === "text") last.text += t;
    else out.push({ kind: "text", text: t });
  };
  const text = (a: number, b: number): void => {
    if (b <= a) return;
    let t = doc.sliceString(a, b).replace(/[ \t]*\n[ \t]*/g, "\n");
    if (lineStart) t = t.replace(/^[ \t]+/, "");
    lineStart = false;
    push(t);
  };
  let pos = from;
  for (let c = node.firstChild; c !== null; c = c.nextSibling) {
    if (c.to <= from || c.from >= to) continue;
    text(pos, c.from);
    pos = c.to;
    const name = c.name;
    if (name === "QuoteMark") {
      lineStart = true;
      continue;
    }
    lineStart = false;
    if (name === "StrongEmphasis" || name === "Emphasis" || name === "Strikethrough") {
      const markName = name === "Strikethrough" ? "StrikethroughMark" : "EmphasisMark";
      const marks = c.getChildren(markName);
      if (marks.length < 2) {
        text(c.from, c.to);
        continue;
      }
      out.push({
        kind: name === "StrongEmphasis" ? "strong" : name === "Emphasis" ? "em" : "strike",
        children: inlineOf(c, marks[0].to, marks[marks.length - 1].from, doc, opts),
      });
      continue;
    }
    if (name === "InlineCode") {
      const marks = c.getChildren("CodeMark");
      if (marks.length < 2) {
        text(c.from, c.to);
        continue;
      }
      let code = sliceWithoutQuotes(c, marks[0].to, marks[1].from, doc);
      if (opts.cell === true) code = unescapePipes(code);
      // CommonMark: line endings are spaces, then ONE space comes off each
      // side when both have one and the span is not all spaces.
      code = code.replace(/\n[ \t]*/g, " ");
      if (code.length >= 2 && code.startsWith(" ") && code.endsWith(" ") && /[^ ]/.test(code))
        code = code.slice(1, -1);
      out.push({ kind: "code", children: [{ kind: "text", text: code }] });
      continue;
    }
    if (name === "Link" || name === "Image") {
      linkOf(c, name === "Image", doc, opts, out, push);
      continue;
    }
    if (name === "Autolink" || name === "URL") {
      // `<https://…>` with its brackets, or GFM's bare `www.`/`https://`/
      // email; the text shows as written, the href gets comrak's scheme.
      const url = name === "URL" ? c : c.getChild("URL");
      const shown = url === null ? doc.sliceString(c.from, c.to) : doc.sliceString(url.from, url.to);
      // GFM: a bare `www.`/`http` link starts a line or follows space,
      // `*`, `_`, `~` or `(` — `<www.x.org>` is text (lezer is looser).
      const before = c.from > 0 ? doc.sliceString(c.from - 1, c.from) : "";
      if (name === "URL" && !shown.includes("@") && before !== "" && !/[\s*_~(]/.test(before)) {
        push(shown);
        continue;
      }
      let href = shown;
      if (!/^[a-z][a-z0-9+.-]*:/i.test(shown)) {
        if (shown.includes("@")) href = `mailto:${shown}`;
        else if (/^www\./i.test(shown)) href = `http://${shown}`;
      }
      out.push({ kind: "link", url: href, title: null, children: [{ kind: "text", text: shown }] });
      continue;
    }
    if (name === "Escape") {
      push(doc.sliceString(c.from + 1, c.to));
      continue;
    }
    if (name === "Entity") {
      push(decodeEntity(doc.sliceString(c.from, c.to)));
      continue;
    }
    if (name === "HardBreak") {
      out.push({ kind: "break" });
      lineStart = true;
      continue;
    }
    if (name === "Comment") continue; // `<!-- … -->` renders as nothing
    if (name === "HTMLTag") {
      out.push({ kind: "html", source: doc.sliceString(c.from, c.to) });
      continue;
    }
    if (name === "FootnoteReference") {
      const label = c.getChild("FootnoteLabel");
      out.push({
        kind: "footnote",
        label: label === null ? "" : doc.sliceString(label.from, label.to),
        source: doc.sliceString(c.from, c.to),
        at: c.from,
      });
      continue;
    }
    if (name === "Wikilink") {
      const marks = c.getChildren("WikilinkMark");
      const source = doc.sliceString(c.from, c.to);
      if (marks.length < 2) {
        push(source);
        continue;
      }
      const parts = wikilinkParts(doc.sliceString(marks[0].to, marks[1].from));
      if (parts.target === "" && parts.heading === null) {
        push(source);
        continue;
      }
      out.push({ kind: "wikilink", ...parts, embed: source.startsWith("!"), source });
      continue;
    }
    if (isMath(c.type)) {
      const source = mathSource(c, doc);
      if (source === null) text(c.from, c.to);
      else
        out.push({
          kind: "math",
          source: opts.cell === true ? unescapePipes(source) : source,
          display: isDisplayMath(c.type),
        });
      continue;
    }
    text(c.from, c.to);
  }
  text(pos, to);
  return out;
}

/** A Link or Image node: inline (`[a](url "t")`), full (`[a][ref]`),
 *  collapsed (`[a][]`) or shortcut (`[a]`). A reference that names no
 *  definition is literal text — its brackets as written around its (still
 *  parsed) label, as comrak renders it. */
function linkOf(
  c: SyntaxNode,
  image: boolean,
  doc: DocText,
  opts: InlineOptions,
  out: Inline[],
  push: (t: string) => void,
): void {
  const marks = c.getChildren("LinkMark");
  const source = doc.sliceString(c.from, c.to);
  if (marks.length < 2) {
    push(source);
    return;
  }
  const [open, close] = marks;
  const url = c.getChild("URL");
  const title = c.getChild("LinkTitle");
  let def: LinkDef | null = null;
  if (url !== null || (marks.length >= 3 && doc.sliceString(marks[2].from, marks[2].to) === "(")) {
    def = {
      url: url === null ? "" : destinationText(doc.sliceString(url.from, url.to)),
      title: title === null ? null : titleText(doc.sliceString(title.from, title.to)),
    };
  } else if (opts.refs !== undefined && opts.refs !== null) {
    const label = c.getChild("LinkLabel");
    const labelText = label === null ? "" : doc.sliceString(label.from + 1, label.to - 1);
    const key = labelText.trim() === "" ? doc.sliceString(open.to, close.from) : labelText;
    if (key.trim() !== "") def = opts.refs(normalizeLabel(key));
  }
  const children = inlineOf(c, open.to, close.from, doc, opts);
  if (def === null) {
    push(image ? "![" : "[");
    for (const child of children) {
      if (child.kind === "text") push(child.text);
      else out.push(child);
    }
    push(doc.sliceString(close.from, c.to));
    return;
  }
  if (image) {
    out.push({ kind: "image", url: def.url, alt: plainText(children), title: def.title, source });
  } else {
    out.push({ kind: "link", url: def.url, title: def.title, children });
  }
}

/** A cell with nothing in it, placed where its text would begin. */
function emptyCell(at: number, doc: DocText): CellModel {
  return { from: doc.sliceString(at, at + 1) === " " ? at + 1 : at, inline: [] };
}

/** One row's cells in column order, padded or trimmed to `columns` (the
 *  header passes null: its count IS the column count). Walking the row's
 *  children keeps the columns honest: a `|` closes a column whether or not
 *  a TableCell was seen in it (an empty cell has none), the row's leading
 *  pipe opens nothing, and the trailing one closes the last column. */
function rowModel(
  row: SyntaxNode,
  doc: DocText,
  columns: number | null,
  opts: InlineOptions,
): RowModel {
  const cells: CellModel[] = [];
  let closing = 0;
  let after = row.from;
  let lastPipe = -1;
  for (let c = row.firstChild; c !== null; c = c.nextSibling) {
    if (c.name === "TableDelimiter") {
      if (c.from === row.from) {
        after = c.to;
        continue;
      }
      if (cells.length === closing) cells.push(emptyCell(after, doc));
      closing++;
      lastPipe = c.to;
      after = c.to;
      continue;
    }
    if (c.name === "TableCell") {
      const inline = inlineOf(c, c.from, c.to, doc, opts);
      trimEdges(inline);
      cells.push({ from: c.from, inline });
    }
  }
  let end = row.to;
  while (end > row.from && /[ \t]/.test(doc.sliceString(end - 1, end))) end--;
  const closed = lastPipe === end;
  const present = closing + (closed ? 0 : 1);
  if (columns !== null) {
    cells.length = Math.min(cells.length, columns);
    while (cells.length < columns) cells.push({ from: end, inline: [] });
  }
  return { cells, present, end, to: row.to, closed };
}

/** Leading and trailing whitespace off an inline run's outer text (a
 *  paragraph's, a heading's, a cell's), as comrak trims block content. */
export function trimEdges(inline: Inline[]): void {
  const first = inline[0];
  if (first !== undefined && first.kind === "text") {
    first.text = first.text.replace(/^[ \t\n]+/, "");
    if (first.text === "") inline.shift();
  }
  const last = inline[inline.length - 1];
  if (last !== undefined && last.kind === "text") {
    last.text = last.text.replace(/[ \t\n]+$/, "");
    if (last.text === "") inline.pop();
  }
}

/** The model of a Table node, or null for a header-less parse. */
export function tableModel(
  table: SyntaxNode,
  doc: DocText,
  opts: InlineOptions = {},
): TableModel | null {
  const headerNode = table.getChild("TableHeader");
  if (headerNode === null) return null;
  const cellOpts: InlineOptions = { ...opts, cell: true };
  const delimiter = table.getChild("TableDelimiter");
  const align = delimiter === null ? [] : alignments(doc.sliceString(delimiter.from, delimiter.to));
  const header = rowModel(headerNode, doc, null, cellOpts);
  const columns = header.cells.length;
  const rows = table.getChildren("TableRow").map((r) => rowModel(r, doc, columns, cellOpts));
  while (align.length < columns) align.push(null);
  return {
    from: table.from,
    to: table.to,
    source: doc.sliceString(table.from, table.to),
    align: align.slice(0, columns),
    header,
    rows,
  };
}

export interface RowCompletion {
  from: number;
  to: number;
  insert: string;
  /** Where the cursor goes once the change is in: inside column `col`. */
  anchor: number;
}

/** The change that gives a short row the column a press asked for, and
 *  where the cursor then belongs — null when the column is already there.
 *  GFM pads a short row at render time only; typing at its end would land
 *  in the LAST real column, so the pipes get written first: ` |` closes an
 *  open last cell, then every missing column as `  |` (trailing whitespace
 *  replaced). The cursor sits between the two spaces of the asked column,
 *  so typing yields `| x |` like the cells around it. */
export function completeRow(row: RowModel, columns: number, col: number): RowCompletion | null {
  if (col < row.present) return null;
  const close = row.closed ? "" : " |";
  return {
    from: row.end,
    to: row.to,
    insert: close + "  |".repeat(columns - row.present),
    anchor: row.end + close.length + 3 * (col - row.present) + 1,
  };
}
