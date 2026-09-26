/**
 * What an embed card makes of the part of its target after `#`. The grammar
 * is the one links and references use — `shared/locator.ts` for every spot
 * (`page=`, `xywh=`, `t=`, `row=`/`col=`/`cell=`, `sheet=&range=`, `slide=`,
 * a notebook's `cell=`) and `shared/fileRef.ts` for GitHub line ranges
 * (`L10-L30`) — so a fragment an agent writes means the same spot in a card,
 * in a link, and in the pane a card opens. What lives here is only what a
 * card needs on top of it: a heading anchor (anything else), RFC 7111's
 * open end (`row=5-*`, which a Locator does not keep), the header's words
 * for the piece shown, and the rows a table slice fetches.
 *
 * Pure: no DOM, no network (fragment.test.ts).
 */

import { lineAnchor, revealOf } from "../fileRef";
import { a1Column, a1ToBlock, clockLabel, parseLocator } from "../locator";
import type { Locator, Reveal } from "../reveal";

/** A region (`#xywh=`), as the locator reads it. */
export type Region = NonNullable<Locator["region"]>;

export interface EmbedFragment {
  /** A line range (`L10-L30`): 1-based, `end` ≥ `start`; `col` from `L12C3`. */
  lines?: { start: number; end: number; col?: number };
  /** Any other spot (`page=3`, `xywh=…`, `row=5-9`, …), exactly as links
   *  and references read it. */
  at?: Locator;
  /** `row=5-*`: from row 5 to the end. The Locator keeps only the start (a
   *  reference opens there either way); a card shows rows from it. */
  rowsToEnd?: true;
  /** A heading (or other named) anchor: a fragment that is no key=value list. */
  anchor?: string;
}

function decode(s: string): string {
  try {
    return decodeURIComponent(s.replace(/\+/g, " "));
  } catch {
    return s;
  }
}

/** An RFC 7111 selection whose end is open (`5-*`; `5-` once prose peeled
 *  the asterisk as bold markup). */
const OPEN_SPAN_RE = /^\d{1,7}-\*?$/;

/** Whether the `row=` a locator took (the last one; its first selection)
 *  runs to the end. */
function rowsOpen(f: string): boolean {
  let open = false;
  for (const part of f.split("&")) {
    const eq = part.indexOf("=");
    if (eq > 0 && part.slice(0, eq).toLowerCase() === "row") open = OPEN_SPAN_RE.test(part.slice(eq + 1).split(";")[0]);
  }
  return open;
}

/**
 * Parse a fragment (with or without the leading `#`) of a target at `path`
 * — the path decides what `cell=` means (a notebook's cell in an `.ipynb`,
 * RFC 7111's `row,col` elsewhere). Unknown keys are ignored; a fragment
 * that is no key=value list is a heading anchor.
 */
export function parseEmbedFragment(fragment: string | null | undefined, path: string): EmbedFragment {
  const f = (fragment ?? "").replace(/^#/, "").trim();
  if (f === "") return {};
  const lines = lineAnchor(f);
  if (lines !== null) {
    if (lines.line === undefined) return {};
    const l: NonNullable<EmbedFragment["lines"]> = { start: lines.line, end: lines.endLine ?? lines.line };
    if (lines.col !== undefined) l.col = lines.col;
    return { lines: l };
  }
  if (!/^[a-z]+=/i.test(f)) return { anchor: decode(f) };
  const at = parseLocator(f, path);
  if (at === null) return {};
  const out: EmbedFragment = { at };
  if (at.table?.row !== undefined && at.table.endRow === undefined && rowsOpen(f)) out.rowsToEnd = true;
  return out;
}

/** Where the full viewer lands when the card is opened in a pane: the same
 *  Reveal a reference to the same fragment opens (`fileRef.ts` `revealOf`). */
export function fragmentReveal(f: EmbedFragment): Reveal | undefined {
  const l = f.lines;
  return revealOf({
    path: "",
    ...(l !== undefined ? { line: l.start } : {}),
    ...(l !== undefined && l.end > l.start ? { endLine: l.end } : {}),
    ...(l?.col !== undefined ? { col: l.col } : {}),
    ...(f.at !== undefined ? { at: f.at } : {}),
  });
}

// --- the card header's words ---------------------------------------------------------

/** `B2:F9`, `B2`, `A:C`, `3:9`: an A1 range as written (normalized). */
export function rangeLabel(r: NonNullable<Locator["range"]>): string {
  const ref = (col: number | undefined, row: number | undefined) =>
    `${col !== undefined ? a1Column(col) : ""}${row ?? ""}`;
  const a = ref(r.col, r.row);
  const b = ref(r.endCol ?? r.col, r.endRow ?? r.row);
  return a === b ? a : `${a}:${b}`;
}

/** A table block in the grid's row numbers, which the card's own row numbers
 *  and the full table view use: RFC 7111 counts a header line as row 1, the
 *  grid counts data rows, so with a header every row is one less (row 1, the
 *  header itself, shows as the first data row, as the slice does). */
function tableLabel(t: NonNullable<Locator["table"]>, toEnd: boolean, headerRow: boolean): string {
  const grid = (r: number | undefined) => (r === undefined || !headerRow ? r : Math.max(1, r - 1));
  const row = grid(t.row);
  const col = t.col;
  const endRow = grid(t.endRow) ?? row;
  const endCol = t.endCol ?? col;
  if (row !== undefined && col !== undefined) {
    return row === endRow && col === endCol ? `cell ${row},${col}` : `cells ${row},${col}–${endRow},${endCol}`;
  }
  if (row !== undefined) {
    if (toEnd) return `rows ${row}–end`;
    return row === endRow ? `row ${row}` : `rows ${row}–${endRow}`;
  }
  if (col !== undefined) return col === endCol ? `column ${col}` : `columns ${col}–${endCol}`;
  return "";
}

/** The card header's short name for the piece shown ("lines 10–30",
 *  "page 3", "rows 5–9", "S!B2:F9", "0:30–0:45"); empty for a whole file.
 *  `headerRow`: the table's first line is a header (`tableHeaderRow`), so
 *  `row=`/`cell=` rows are named as the grid numbers them. An A1 range keeps
 *  the sheet's own numbers, as a spreadsheet names it. */
export function fragmentLabel(f: EmbedFragment, headerRow = true): string {
  const parts: string[] = [];
  if (f.lines !== undefined) {
    parts.push(
      f.lines.end > f.lines.start ? `lines ${f.lines.start}–${f.lines.end}` : `line ${f.lines.start}`,
    );
  }
  const at = f.at ?? {};
  if (at.page !== undefined) parts.push(`page ${at.page}`);
  if (at.slide !== undefined) parts.push(`slide ${at.slide}`);
  if (at.cell !== undefined) parts.push(`cell ${at.cell}`);
  if (at.table !== undefined) parts.push(tableLabel(at.table, f.rowsToEnd === true, headerRow));
  if (at.sheet !== undefined || at.range !== undefined) {
    const range = at.range !== undefined ? rangeLabel(at.range) : "";
    parts.push(at.sheet !== undefined ? (range !== "" ? `${at.sheet}!${range}` : at.sheet) : range);
  }
  if (at.time !== undefined) {
    const { start, end } = at.time;
    parts.push(end !== undefined ? `${clockLabel(start)}–${clockLabel(end)}` : `from ${clockLabel(start)}`);
  }
  if (at.region !== undefined) parts.push("region");
  if (f.anchor !== undefined) parts.push(`# ${f.anchor}`);
  return parts.filter((p) => p !== "").join(" · ");
}

// --- a table slice ---------------------------------------------------------------------

/** A block of a table grid as a card shows it: RFC 7111 rows (the header
 *  row is row 1), 1-based grid columns, and `row=5-*`'s open end. */
export type TableSlice = NonNullable<Locator["table"]> & { toEnd?: true };

/**
 * The block of a table grid a fragment selects, or undefined for the whole
 * table: `row=`/`col=`/`cell=` as written; an A1 `range=` through the used
 * range's `origin` (0-based [row, column] of its first cell — the grid's
 * header row; `[0, 0]` for a delimited file, whose header line is A1's
 * row 1), the way the spreadsheet view reveals one (`a1ToBlock`).
 */
export function tableSlice(f: EmbedFragment, origin: readonly [number, number]): TableSlice | undefined {
  const at = f.at;
  if (at?.range !== undefined) return a1ToBlock(at.range, origin);
  if (at?.table === undefined) return undefined;
  return f.rowsToEnd === true ? { ...at.table, toEnd: true } : at.table;
}

/** Whether an A1 range lies wholly above or left of a used range starting
 *  at `origin` (0-based): none of its cells is on the grid. */
export function rangeOutside(r: NonNullable<Locator["range"]>, origin: readonly [number, number]): boolean {
  const lastRow = r.endRow ?? r.row;
  const lastCol = r.endCol ?? r.col;
  return (lastRow !== undefined && lastRow <= origin[0]) || (lastCol !== undefined && lastCol <= origin[1]);
}

/**
 * The data rows to fetch for a slice, as fs/table's (and fs/xlsx's) window.
 * RFC 7111 counts the header line as row 1 when the file has one, so
 * `row=2` is the first data row there; a header-less file (BED, SAM)
 * starts its data at row 1. No rows selected → the first `peek` rows; an
 * open end → `peek` rows from its start. At most `cap` rows either way.
 */
export function tableWindow(
  slice: TableSlice | undefined,
  hasHeader: boolean,
  peek = 10,
  cap = 50,
): { offset: number; limit: number } {
  if (slice?.row === undefined) return { offset: 0, limit: peek };
  const toData = (r: number) => (hasHeader ? r - 2 : r - 1);
  const first = Math.max(0, toData(slice.row));
  const last = slice.toEnd === true ? first + peek - 1 : toData(slice.endRow ?? slice.row);
  const limit = Math.min(cap, Math.max(1, last - first + 1));
  return { offset: first, limit };
}
