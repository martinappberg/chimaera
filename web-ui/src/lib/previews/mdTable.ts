/**
 * The GFM table model the live preview's table widget renders — built from
 * the editor's own syntax tree at collection time, as plain data: every
 * cell's source range (so a press on the rendered cell can land the cursor
 * in that cell's text) and an inline tree of what the cell holds. Nothing
 * here touches the DOM; the widget in mdLive.ts turns the model into
 * elements with createElement, so the live preview's no-injected-HTML
 * boundary holds for tables too (an equation goes through the shared KaTeX
 * policy like any other).
 *
 * Shapes, from lezer's GFM extension: a Table holds a TableHeader, ONE
 * TableDelimiter node for the whole `|:--|--:|` row, and TableRows; cells
 * are TableCell nodes between `|` TableDelimiters; an EMPTY cell emits no
 * node at all (only its pipes); a leading or trailing pipe is optional; a
 * row shorter than the header is padded with empty cells and extra cells
 * are dropped (GFM). A quoted table carries its per-line QuoteMarks as
 * children of the Table, beside the rows.
 */
import type { Text } from "@codemirror/state";
import type { SyntaxNode } from "@lezer/common";
import { isDisplayMath, isMath, mathSource } from "./mdMath";

export type Align = "left" | "center" | "right" | null;

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "strong" | "em" | "strike" | "code"; children: Inline[] }
  | { kind: "link"; url: string; children: Inline[] }
  | { kind: "math"; source: string; display: boolean };

export interface CellModel {
  /** The cell's text range in the document — an empty cell points just past
   *  its opening pipe (and the space after it), where typing would go. */
  from: number;
  to: number;
  inline: Inline[];
}

export interface TableModel {
  from: number;
  to: number;
  /** The table's source text, the widget's identity: a table whose text is
   *  unchanged keeps its rendered DOM across rebuilds. */
  source: string;
  align: Align[];
  header: CellModel[];
  rows: CellModel[][];
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

/** The inline tree of [from, to] inside `node`: the syntax children the
 *  decorator styles, with the plain text between them. Delimiter marks are
 *  dropped, an escape yields its character, and anything the widget has no
 *  element for (an image, an HTML tag) stays as its source text. */
function inlineOf(node: SyntaxNode, from: number, to: number, doc: Text): Inline[] {
  const out: Inline[] = [];
  const text = (a: number, b: number): void => {
    if (b > a) out.push({ kind: "text", text: doc.sliceString(a, b) });
  };
  let pos = from;
  for (let c = node.firstChild; c !== null; c = c.nextSibling) {
    if (c.to <= from || c.from >= to) continue;
    if (c.from < pos) continue; // a mark already consumed (delimiters)
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
      out.push({
        kind: "code",
        children: [{ kind: "text", text: doc.sliceString(marks[0].to, marks[1].from) }],
      });
      continue;
    }
    if (name === "Link") {
      const url = c.getChild("URL");
      const marks = c.getChildren("LinkMark");
      if (url === null || marks.length < 2 || marks[1].from <= marks[0].to) {
        text(c.from, c.to); // reference-style or label-less: source
        continue;
      }
      out.push({
        kind: "link",
        url: doc.sliceString(url.from, url.to),
        children: inlineOf(c, marks[0].to, marks[1].from, doc),
      });
      continue;
    }
    if (name === "Autolink") {
      const url = c.getChild("URL");
      const href = url === null ? doc.sliceString(c.from, c.to) : doc.sliceString(url.from, url.to);
      out.push({ kind: "link", url: href, children: [{ kind: "text", text: href }] });
      continue;
    }
    if (name === "Escape") {
      out.push({ kind: "text", text: doc.sliceString(c.from + 1, c.to) });
      continue;
    }
    if (isMath(c.type)) {
      const source = mathSource(c, doc);
      if (source === null) text(c.from, c.to);
      else out.push({ kind: "math", source, display: isDisplayMath(c.type) });
      continue;
    }
    text(c.from, c.to);
  }
  text(pos, to);
  return out;
}

/** A cell with nothing in it, placed where its text would begin. */
function emptyCell(at: number, doc: Text): CellModel {
  const from = doc.sliceString(at, at + 1) === " " ? at + 1 : at;
  return { from, to: from, inline: [] };
}

/** The cells of one row in column order. Walking the row's children keeps
 *  the columns honest: a `|` closes a column whether or not a TableCell was
 *  seen in it (an empty cell has none), the row's leading pipe opens nothing,
 *  and the trailing one closes the last column. */
function rowCells(row: SyntaxNode, doc: Text): CellModel[] {
  const cells: CellModel[] = [];
  let closed = 0;
  let after = row.from;
  for (let c = row.firstChild; c !== null; c = c.nextSibling) {
    if (c.name === "TableDelimiter") {
      if (c.from === row.from) {
        after = c.to;
        continue;
      }
      if (cells.length === closed) cells.push(emptyCell(after, doc));
      closed++;
      after = c.to;
      continue;
    }
    if (c.name === "TableCell") cells.push({ from: c.from, to: c.to, inline: inlineOf(c, c.from, c.to, doc) });
  }
  return cells;
}

/** The model of a Table node, or null for a header-less parse. */
export function tableModel(table: SyntaxNode, doc: Text): TableModel | null {
  const header = table.getChild("TableHeader");
  if (header === null) return null;
  const delimiter = table.getChild("TableDelimiter");
  const align = delimiter === null ? [] : alignments(doc.sliceString(delimiter.from, delimiter.to));
  const head = rowCells(header, doc);
  const columns = head.length;
  const rows = table.getChildren("TableRow").map((r) => {
    const cells = rowCells(r, doc).slice(0, columns);
    while (cells.length < columns) cells.push({ from: r.to, to: r.to, inline: [] });
    return cells;
  });
  while (align.length < columns) align.push(null);
  return {
    from: table.from,
    to: table.to,
    source: doc.sliceString(table.from, table.to),
    align: align.slice(0, columns),
    header: head,
    rows,
  };
}
