/**
 * The GFM table model the live preview's table widget renders — built from
 * the editor's own syntax tree, as plain data: every cell's source position
 * (so a press on the rendered cell can land the cursor in that cell's text)
 * and an inline tree of what the cell holds. Nothing here touches the DOM;
 * the widget in mdLive.ts turns the model into elements with createElement,
 * so the live preview's no-injected-HTML boundary holds for tables too (an
 * equation goes through the shared KaTeX policy like any other).
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
import type { Text } from "@codemirror/state";
import type { SyntaxNode } from "@lezer/common";
import { isDisplayMath, isMath, mathSource } from "./mdMath";

export type Align = "left" | "center" | "right" | null;

export type Inline =
  | { kind: "text"; text: string }
  /** An HTML character reference (`&amp;`, `&#x27;`) — decoded by the widget. */
  | { kind: "entity"; source: string }
  | { kind: "strong" | "em" | "strike" | "code"; children: Inline[] }
  | { kind: "link"; url: string; children: Inline[] }
  /** `![alt](url)`; `source` is what shows when the URL may not render. */
  | { kind: "image"; url: string; alt: string; source: string }
  | { kind: "math"; source: string; display: boolean };

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

/** comrak's `unescape_pipes`: `\|` is a pipe, and a backslash that is
 *  itself escaped (`\\|`) protects nothing. Applied where lezer leaves the
 *  text raw — a code span, an equation; plain text has the Escape node. */
function unescapePipes(s: string): string {
  return s.replace(/\\([\\|])/g, (m, ch: string) => (ch === "|" ? "|" : m));
}

/** The inline tree of [from, to] inside `node`: the syntax children the
 *  decorator styles, with the plain text between them. Delimiter marks are
 *  dropped, an escape yields its character, a comment nothing, and anything
 *  the widget has no element for (an HTML tag, a reference-style link)
 *  stays as its source text. */
function inlineOf(node: SyntaxNode, from: number, to: number, doc: Text): Inline[] {
  const out: Inline[] = [];
  const text = (a: number, b: number): void => {
    if (b > a) out.push({ kind: "text", text: doc.sliceString(a, b) });
  };
  let pos = from;
  for (let c = node.firstChild; c !== null; c = c.nextSibling) {
    if (c.to <= from || c.from >= to) continue;
    text(pos, c.from);
    pos = c.to;
    const name = c.name;
    if (name === "StrongEmphasis" || name === "Emphasis" || name === "Strikethrough") {
      const markName = name === "Strikethrough" ? "StrikethroughMark" : "EmphasisMark";
      const marks = c.getChildren(markName);
      if (marks.length < 2) {
        text(c.from, c.to);
        continue;
      }
      out.push({
        kind: name === "StrongEmphasis" ? "strong" : name === "Emphasis" ? "em" : "strike",
        children: inlineOf(c, marks[0].to, marks[marks.length - 1].from, doc),
      });
      continue;
    }
    if (name === "InlineCode") {
      const marks = c.getChildren("CodeMark");
      if (marks.length < 2) {
        text(c.from, c.to);
        continue;
      }
      const code = unescapePipes(doc.sliceString(marks[0].to, marks[1].from));
      out.push({ kind: "code", children: [{ kind: "text", text: code }] });
      continue;
    }
    if (name === "Link" || name === "Image") {
      const url = c.getChild("URL");
      const marks = c.getChildren("LinkMark");
      if (url === null || marks.length < 2 || marks[1].from <= marks[0].to) {
        text(c.from, c.to); // reference-style or label-less: source
        continue;
      }
      const href = doc.sliceString(url.from, url.to);
      if (name === "Image") {
        out.push({
          kind: "image",
          url: href,
          alt: doc.sliceString(marks[0].to, marks[1].from),
          source: doc.sliceString(c.from, c.to),
        });
      } else {
        out.push({ kind: "link", url: href, children: inlineOf(c, marks[0].to, marks[1].from, doc) });
      }
      continue;
    }
    if (name === "Autolink" || name === "URL") {
      // `<https://…>` with its brackets, or GFM's bare `www.`/`https://` URL.
      const url = name === "URL" ? c : c.getChild("URL");
      const href = url === null ? doc.sliceString(c.from, c.to) : doc.sliceString(url.from, url.to);
      out.push({ kind: "link", url: href, children: [{ kind: "text", text: href }] });
      continue;
    }
    if (name === "Escape") {
      out.push({ kind: "text", text: doc.sliceString(c.from + 1, c.to) });
      continue;
    }
    if (name === "Entity") {
      out.push({ kind: "entity", source: doc.sliceString(c.from, c.to) });
      continue;
    }
    if (name === "Comment") continue; // `<!-- … -->` renders as nothing
    if (isMath(c.type)) {
      const source = mathSource(c, doc);
      if (source === null) text(c.from, c.to);
      else out.push({ kind: "math", source: unescapePipes(source), display: isDisplayMath(c.type) });
      continue;
    }
    text(c.from, c.to);
  }
  text(pos, to);
  return out;
}

/** A cell with nothing in it, placed where its text would begin. */
function emptyCell(at: number, doc: Text): CellModel {
  return { from: doc.sliceString(at, at + 1) === " " ? at + 1 : at, inline: [] };
}

/** One row's cells in column order, padded or trimmed to `columns` (the
 *  header passes null: its count IS the column count). Walking the row's
 *  children keeps the columns honest: a `|` closes a column whether or not
 *  a TableCell was seen in it (an empty cell has none), the row's leading
 *  pipe opens nothing, and the trailing one closes the last column. */
function rowModel(row: SyntaxNode, doc: Text, columns: number | null): RowModel {
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
    if (c.name === "TableCell") cells.push({ from: c.from, inline: inlineOf(c, c.from, c.to, doc) });
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

/** The model of a Table node, or null for a header-less parse. */
export function tableModel(table: SyntaxNode, doc: Text): TableModel | null {
  const headerNode = table.getChild("TableHeader");
  if (headerNode === null) return null;
  const delimiter = table.getChild("TableDelimiter");
  const align = delimiter === null ? [] : alignments(doc.sliceString(delimiter.from, delimiter.to));
  const header = rowModel(headerNode, doc, null);
  const columns = header.cells.length;
  const rows = table.getChildren("TableRow").map((r) => rowModel(r, doc, columns));
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
