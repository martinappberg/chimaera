/**
 * Locator fragments: the one grammar for pointing INTO a file (the document
 * workbench plan, Phase 5). Both directions live here so they cannot drift:
 * parsing (a link, an embed, a mention in chat or terminal text → the spot a
 * viewer reveals) and formatting (a viewer's selection → the fragment typed
 * into an agent). Existing standards wherever one exists, so any LLM can
 * read and write them:
 *
 *   #page=4                      PDF open parameters (RFC 8118)
 *   #xywh=160,120,320,240        W3C Media Fragments (pixel: / percent: too);
 *   #page=4&xywh=…               on a PDF page, in PDF points from its top-left
 *   #t=12.5,20                   W3C Media Fragments (npt, hh:mm:ss too)
 *   #row=5-9 #col=2 #cell=5,2-9,4  RFC 7111 (the header line is row 1)
 *   #sheet=Summary&range=B2:F9   A1 notation
 *   #cell=7 (.ipynb) #slide=3    Chimaera convention
 *
 * `#L12-L20` (lines) stays in `fileRef.ts`; a heading slug is neither and
 * parses as null here.
 */

import type { Locator } from "./reveal";

/** The keys a locator fragment may carry (a heading slug never has `=`). */
const KEYS = new Set(["page", "xywh", "t", "row", "col", "cell", "sheet", "range", "slide"]);

/** `#<key>` just before an `=`: a locator is starting (token scanning). */
export function isLocatorKey(key: string): boolean {
  return KEYS.has(key.toLowerCase());
}

const INT_RE = /^\d{1,7}$/;
const NUM = String.raw`\d{1,7}(?:\.\d{1,4})?`;
const XYWH_RE = new RegExp(String.raw`^(?:(pixel|percent):)?(${NUM}),(${NUM}),(${NUM}),(${NUM})$`, "i");
/** Seconds (`12.5`) or a clock (`1:02:03.5`, `02:03`). */
const CLOCK_RE = /^(?:(\d{1,3}):)?(\d{1,2}):(\d{1,2}(?:\.\d{1,3})?)$/;
const SECONDS_RE = /^\d{1,7}(?:\.\d{1,3})?$/;
/** RFC 7111's open end (`5-*`) may reach here without its `*`: prose
 *  peeling takes a trailing asterisk for bold markup. */
const SPAN_RE = /^(\d{1,7})(?:-(\d{1,7}|\*)?)?$/;
const CELL_RE = /^(\d{1,7}),(\d{1,7})(?:-(?:(\d{1,7}),(\d{1,7})|\*)?)?$/;
const A1_RE = /^\$?([A-Za-z]{1,3})?\$?(\d{1,7})?$/;

function int(s: string | undefined): number | null {
  if (s === undefined || !INT_RE.test(s)) return null;
  const n = Number.parseInt(s, 10);
  return n >= 1 ? n : null;
}

/** A W3C npt time in seconds, or null. */
export function parseClock(s: string): number | null {
  if (SECONDS_RE.test(s)) return Number(s);
  const m = CLOCK_RE.exec(s);
  if (m === null) return null;
  const h = m[1] !== undefined ? Number(m[1]) : 0;
  const min = Number(m[2]);
  const sec = Number(m[3]);
  if (min >= 60 && m[1] !== undefined) return null;
  if (sec >= 60) return null;
  return h * 3600 + min * 60 + sec;
}

function parseTime(v: string): Locator["time"] | null {
  const body = v.replace(/^npt:/i, "");
  const comma = body.indexOf(",");
  const a = comma >= 0 ? body.slice(0, comma) : body;
  const b = comma >= 0 ? body.slice(comma + 1) : "";
  const start = a === "" ? 0 : parseClock(a);
  if (start === null) return null;
  if (b === "") return a === "" ? null : { start };
  const end = parseClock(b);
  if (end === null) return null;
  return end > start ? { start, end } : { start };
}

/** 1-based A1 column number (`A` = 1, `AA` = 27). */
export function a1ColumnNumber(letters: string): number {
  let n = 0;
  for (const ch of letters.toUpperCase()) n = n * 26 + (ch.charCodeAt(0) - 64);
  return n;
}

/** A1 column letters for a 1-based column number. */
export function a1Column(n: number): string {
  let out = "";
  let k = Math.max(1, Math.floor(n));
  while (k > 0) {
    const r = (k - 1) % 26;
    out = String.fromCharCode(65 + r) + out;
    k = Math.floor((k - 1) / 26);
  }
  return out;
}

/** `B2:F9`, `B2`, `A:C`, `3:9` → 1-based sheet coordinates (absent = all). */
export function parseA1(v: string): Locator["range"] | null {
  const [from, to, ...rest] = v.split(":");
  if (rest.length > 0 || from === undefined || from === "") return null;
  const a = A1_RE.exec(from);
  const b = to !== undefined ? A1_RE.exec(to) : null;
  if (a === null || (to !== undefined && b === null)) return null;
  const out: NonNullable<Locator["range"]> = {};
  if (a[1] !== undefined) out.col = a1ColumnNumber(a[1]);
  const r0 = int(a[2]);
  if (r0 !== null) out.row = r0;
  if (out.col === undefined && out.row === undefined) return null;
  if (b !== null) {
    if (b[1] !== undefined) {
      const c1 = a1ColumnNumber(b[1]);
      if (out.col !== undefined && c1 > out.col) out.endCol = c1;
      else if (out.col !== undefined && c1 < out.col) [out.col, out.endCol] = [c1, out.col];
    }
    const r1 = int(b[2]);
    if (r1 !== null && out.row !== undefined) {
      if (r1 > out.row) out.endRow = r1;
      else if (r1 < out.row) [out.row, out.endRow] = [r1, out.row];
    }
  }
  return out;
}

function decodeSheet(v: string): string | null {
  let s = v;
  try {
    s = decodeURIComponent(v.replace(/\+/g, "%20"));
  } catch {
    // A malformed escape: take the text as written.
  }
  s = s.trim();
  if (s.length >= 2 && s.startsWith("'") && s.endsWith("'")) s = s.slice(1, -1).replace(/''/g, "'");
  return s !== "" && s.length <= 128 ? s : null;
}

function extensionOf(path: string): string {
  const base = path.slice(path.lastIndexOf("/") + 1);
  const dot = base.lastIndexOf(".");
  return dot > 0 ? base.slice(dot + 1).toLowerCase() : "";
}

/**
 * The spot a `#fragment` (with or without the `#`) names in `path`, or null
 * when it is not a locator (a line range or a heading slug). `#cell=` is
 * decided by extension: a notebook's cell in an `.ipynb`, RFC 7111's
 * `row,col` anywhere else. The first of several RFC 7111 selections (`;`)
 * wins; unknown keys are ignored.
 */
export function parseLocator(fragment: string, path: string): Locator | null {
  const f = fragment.startsWith("#") ? fragment.slice(1) : fragment;
  if (!f.includes("=")) return null;
  const out: Locator = {};
  let any = false;
  const notebook = extensionOf(path) === "ipynb";
  for (const part of f.split("&")) {
    const eq = part.indexOf("=");
    if (eq <= 0) continue;
    const key = part.slice(0, eq).toLowerCase();
    const value = part.slice(eq + 1);
    if (!KEYS.has(key)) continue;
    const first = value.split(";")[0];
    switch (key) {
      case "page":
      case "slide": {
        const n = int(value);
        if (n === null) continue;
        out[key] = n;
        break;
      }
      case "xywh": {
        const m = XYWH_RE.exec(value);
        if (m === null) continue;
        const [x, y, w, h] = [m[2], m[3], m[4], m[5]].map(Number);
        const percent = m[1]?.toLowerCase() === "percent";
        if (w <= 0 || h <= 0 || (percent && (x > 100 || y > 100))) continue;
        out.region = percent ? { x, y, w, h, percent: true } : { x, y, w, h };
        break;
      }
      case "t": {
        const t = parseTime(value);
        if (t === null) continue;
        out.time = t;
        break;
      }
      case "row":
      case "col": {
        const m = SPAN_RE.exec(first);
        if (m === null) continue;
        const a = int(m[1]);
        const b = m[2] !== undefined && m[2] !== "*" ? int(m[2]) : null;
        if (a === null) continue;
        const t = { ...out.table };
        if (key === "row") {
          t.row = a;
          if (b !== null && b > a) t.endRow = b;
        } else {
          t.col = a;
          if (b !== null && b > a) t.endCol = b;
        }
        out.table = t;
        break;
      }
      case "cell": {
        if (notebook) {
          const n = int(value);
          if (n === null) continue;
          out.cell = n;
          break;
        }
        const m = CELL_RE.exec(first);
        if (m === null) continue;
        const r0 = int(m[1]);
        const c0 = int(m[2]);
        if (r0 === null || c0 === null) continue;
        const t: NonNullable<Locator["table"]> = { row: r0, col: c0 };
        const r1 = int(m[3]);
        const c1 = int(m[4]);
        if (r1 !== null && c1 !== null) {
          if (r1 > r0) t.endRow = r1;
          if (c1 > c0) t.endCol = c1;
        }
        out.table = t;
        break;
      }
      case "sheet": {
        const s = decodeSheet(value);
        if (s === null) continue;
        out.sheet = s;
        break;
      }
      case "range": {
        let v = value;
        const bang = v.lastIndexOf("!");
        if (bang >= 0) {
          const s = decodeSheet(v.slice(0, bang));
          if (s !== null && out.sheet === undefined) out.sheet = s;
          v = v.slice(bang + 1);
        }
        const r = parseA1(v);
        if (r === null) continue;
        out.range = r;
        break;
      }
    }
    any = true;
  }
  return any ? out : null;
}

// --- formatting (a viewer's selection → the fragment an agent receives) ---------

/** Seconds as a fragment number: at most two decimals, no trailing zeros. */
export function formatSeconds(s: number): string {
  const v = Math.max(0, Math.round(s * 100) / 100);
  return String(v);
}

/** `t=12.5` or `t=12.5,20`. */
export function timeFragment(start: number, end?: number | null): string {
  const a = formatSeconds(start);
  if (end === undefined || end === null || !(end > start)) return `t=${a}`;
  return `t=${a},${formatSeconds(end)}`;
}

/** `m:ss` (or `h:mm:ss`), with the fraction the fragment carries (two
 *  decimals at most, trailing zeros dropped). */
export function clockLabel(s: number): string {
  const cents = Math.round(Math.max(0, s) * 100);
  const whole = Math.floor(cents / 100);
  const frac = cents % 100;
  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  const sec = String(whole % 60).padStart(2, "0");
  const base = h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
  return frac > 0 ? `${base}.${String(frac).padStart(2, "0").replace(/0$/, "")}` : base;
}

/** `xywh=x,y,w,h` in whole units (pixels, or PDF points). */
export function xywhFragment(r: { x: number; y: number; w: number; h: number }): string {
  const x = Math.round(r.x);
  const y = Math.round(r.y);
  const w = Math.max(1, Math.round(r.x + r.w) - x);
  const h = Math.max(1, Math.round(r.y + r.h) - y);
  return `xywh=${x},${y},${w},${h}`;
}

/** A block of a table grid: 1-based DATA rows (the grid's own row numbers,
 *  the header not counted) and 1-based columns. */
export interface TableBlock {
  r0: number;
  r1: number;
  c0: number;
  c1: number;
  /** Whole rows (the gutter was dragged): `row=` rather than `cell=`. */
  wholeRows: boolean;
}

/**
 * RFC 7111 for a block: `row=6-10`, `cell=6,2`, `cell=6,2-10,4`. RFC 7111
 * counts the header line as row 1 when the file has one (`header`), so the
 * grid's data row N is row N + 1 there; a header-less format (BED, SAM)
 * counts from its first record. Embed cards read rows the same way.
 */
export function tableFragment(b: TableBlock, header: boolean): string {
  const k = header ? 1 : 0;
  const r0 = b.r0 + k;
  const r1 = b.r1 + k;
  if (b.wholeRows) return r1 > r0 ? `row=${r0}-${r1}` : `row=${r0}`;
  if (r0 === r1 && b.c0 === b.c1) return `cell=${r0},${b.c0}`;
  return `cell=${r0},${b.c0}-${r1},${b.c1}`;
}

/** The grid's data row for an RFC 7111 row (the header line clamps to the
 *  first data row). */
export function rfcToDataRow(row: number, header: boolean): number {
  return header ? Math.max(1, row - 1) : row;
}

/** Words for a table block in the grid's own numbers ("rows 5–9",
 *  "cell 5,2"), for tooltips. */
export function tableLabel(b: TableBlock): string {
  if (b.wholeRows) return b.r1 > b.r0 ? `rows ${b.r0}–${b.r1}` : `row ${b.r0}`;
  if (b.r0 === b.r1 && b.c0 === b.c1) return `cell ${b.r0},${b.c0}`;
  return `cells ${b.r0},${b.c0}–${b.r1},${b.c1}`;
}

/** `B2` / `B2:F9` from 1-based sheet coordinates. */
export function a1Range(row0: number, col0: number, row1: number, col1: number): string {
  const a = `${a1Column(col0)}${row0}`;
  return row0 === row1 && col0 === col1 ? a : `${a}:${a1Column(col1)}${row1}`;
}

/**
 * A spreadsheet grid shows the used range's first row as its header and the
 * rows under it as data rows 1, 2, …; `origin` is that range's first cell
 * (0-based row, column). A block of the grid in A1 terms…
 */
export function blockToA1(b: TableBlock, origin: readonly [number, number]): string {
  const [or, oc] = origin;
  return a1Range(or + 1 + b.r0, oc + b.c0, or + 1 + b.r1, oc + b.c1);
}

/** …and an A1 range back on the grid, as RFC 7111 rows of it (its header
 *  row is row 1, like a delimited file's header line). */
export function a1ToBlock(
  r: NonNullable<Locator["range"]>,
  origin: readonly [number, number],
): NonNullable<Locator["table"]> {
  const [or, oc] = origin;
  const row = (sheetRow: number) => Math.max(1, sheetRow - or);
  const col = (sheetCol: number) => Math.max(1, sheetCol - oc);
  const t: NonNullable<Locator["table"]> = {};
  if (r.row !== undefined) {
    t.row = row(r.row);
    if (r.endRow !== undefined && row(r.endRow) > t.row) t.endRow = row(r.endRow);
  }
  if (r.col !== undefined) {
    t.col = col(r.col);
    if (r.endCol !== undefined && col(r.endCol) > t.col) t.endCol = col(r.endCol);
  }
  return t;
}

/** `sheet=Q1%20Summary&range=B2:F9` (the sheet name URL-encoded, so the
 *  locator stays one token in any prose or terminal line). */
export function sheetFragment(sheet: string, range: string): string {
  return `sheet=${encodeURIComponent(sheet)}&range=${range}`;
}

// --- a table block as a quote --------------------------------------------------------

export interface TsvLimits {
  maxRows: number;
  maxCols: number;
  /** UTF-16 units of the finished quote (≈ bytes for the ASCII this is). */
  maxChars: number;
}

/** The reference budget: 50 rows × 20 columns, 8 KB. */
export const TSV_LIMITS: TsvLimits = { maxRows: 50, maxCols: 20, maxChars: 8 * 1024 };

/** One cell, escaped so the quote stays one line: `\t`, `\n`, `\r`, `\\`,
 *  `\"` as their escapes, any other control character as a space. */
export function escapeCell(v: string): string {
  return (
    v
      .replace(/\\/g, "\\\\")
      .replace(/"/g, '\\"')
      .replace(/\t/g, "\\t")
      .replace(/\r/g, "\\r")
      .replace(/\n/g, "\\n")
      // eslint-disable-next-line no-control-regex
      .replace(/[\u0000-\u001f\u007f]/g, " ")
  );
}

/**
 * A table block as a one-line TSV quote: the header first, `\t` between
 * cells and `\n` between rows as literal escapes (a real tab or newline
 * would drive a terminal agent's input). Capped honestly: past the limits
 * a row ends in `\t…`, the block in `\n…`; rows of the block that are not
 * loaded open it with `…\n` (`clippedBefore`) or close it with `\n…`
 * (`clippedAfter`).
 */
export function tsvQuote(
  header: readonly string[] | null,
  rows: readonly (readonly string[])[],
  limits: TsvLimits = TSV_LIMITS,
  clippedBefore = false,
  clippedAfter = false,
): string {
  const line = (cells: readonly string[]): string => {
    const shown = cells.slice(0, limits.maxCols).map(escapeCell);
    if (cells.length > limits.maxCols) shown.push("…");
    return shown.join("\\t");
  };
  const lines: string[] = [];
  if (header !== null) lines.push(line(header));
  if (clippedBefore) lines.push("…");
  let used = lines.reduce((n, l) => n + l.length + 2, 0);
  let cut = rows.length > limits.maxRows || clippedAfter;
  for (const row of rows.slice(0, limits.maxRows)) {
    const l = line(row);
    // Room for this line plus the closing `\n…`.
    if (used + l.length + 2 + 3 > limits.maxChars) {
      cut = true;
      break;
    }
    lines.push(l);
    used += l.length + 2;
  }
  if (cut) lines.push("…");
  return lines.join("\\n");
}
