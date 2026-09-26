/**
 * The part of an embed target after `#`: which piece of the file a card
 * shows. One grammar with links and references (docs/document-workbench-
 * plan.md, "One address format"), built on existing standards so any LLM
 * reads and writes it:
 *
 *   `L10-L30`              GitHub line ranges (`L12C3-L14`, `L5-20` too)
 *   `page=3`               PDF open parameters (RFC 8118)
 *   `xywh=160,120,320,240` W3C Media Fragments (`pixel:` / `percent:` units;
 *                          on a PDF, points from the page's top-left)
 *   `t=30,45`, `t=1:02`    W3C Media Fragments (npt seconds or h:m:s)
 *   `row=5-9`              RFC 7111 — row 1 is the header line when there is one
 *   `sheet=S&range=A1:F20` spreadsheet A1 notation
 *   `cell=7`, `slide=3`    Chimaera convention, 1-based
 *   anything else          a heading anchor (`Results`, `my-section`)
 *
 * Pure: no DOM, no network (fragment.test.ts).
 */

import type { Reveal } from "../reveal";

export interface Region {
  x: number;
  y: number;
  w: number;
  h: number;
  /** Media Fragments units: pixels (the default) or percent of the frame. */
  unit: "pixel" | "percent";
}

/** A spreadsheet range, 1-based and inclusive; a column or row side may be
 *  open (`A:C` whole columns, `2:9` whole rows). */
export interface CellRange {
  c1: number | null;
  r1: number | null;
  c2: number | null;
  r2: number | null;
}

export interface EmbedFragment {
  lines?: { start: number; end: number };
  page?: number;
  region?: Region;
  time?: { start: number; end?: number };
  /** RFC 7111 rows; `end` null = to the end (`row=5-*`). */
  rows?: { start: number; end: number | null };
  sheet?: string;
  range?: CellRange;
  cell?: number;
  slide?: number;
  /** A heading (or other named) anchor. */
  anchor?: string;
}

function decode(s: string): string {
  try {
    return decodeURIComponent(s.replace(/\+/g, " "));
  } catch {
    return s;
  }
}

function positiveInt(s: string | undefined): number | null {
  if (s === undefined || !/^\d{1,9}$/.test(s)) return null;
  const n = Number(s);
  return n >= 1 ? n : null;
}

/** npt: `12.5`, `1:02`, `1:02:03.5`, optionally `npt:`-prefixed. */
export function parseClock(s: string): number | null {
  const t = s.trim().replace(/^npt:/, "");
  if (/^\d+(?:\.\d+)?$/.test(t)) return Number(t);
  const m = /^(?:(\d+):)?(\d{1,2}):(\d{1,2}(?:\.\d+)?)$/.exec(t);
  if (m === null) return null;
  return Number(m[1] ?? 0) * 3600 + Number(m[2]) * 60 + Number(m[3]);
}

function parseRegion(v: string): Region | null {
  let unit: Region["unit"] = "pixel";
  let body = v;
  const m = /^(pixel|percent):(.*)$/.exec(v);
  if (m !== null) {
    unit = m[1] as Region["unit"];
    body = m[2];
  }
  const parts = body.split(",").map((p) => Number(p.trim()));
  if (parts.length !== 4 || parts.some((n) => !Number.isFinite(n) || n < 0)) return null;
  const [x, y, w, h] = parts;
  if (w <= 0 || h <= 0) return null;
  return { x, y, w, h, unit };
}

/** A1 column letters → 1-based index (`A` → 1, `AA` → 27). */
function columnIndex(letters: string): number {
  let n = 0;
  for (const ch of letters.toUpperCase()) n = n * 26 + (ch.charCodeAt(0) - 64);
  return n;
}

function parseCellRef(s: string): { c: number | null; r: number | null } | null {
  const m = /^\$?([A-Za-z]{1,3})?\$?(\d{1,7})?$/.exec(s.trim());
  if (m === null || (m[1] === undefined && m[2] === undefined)) return null;
  return {
    c: m[1] !== undefined ? columnIndex(m[1]) : null,
    r: m[2] !== undefined ? Number(m[2]) : null,
  };
}

export function parseRange(v: string): CellRange | null {
  const [a, b] = v.split(":");
  const first = parseCellRef(a ?? "");
  if (first === null) return null;
  const second = b === undefined ? first : parseCellRef(b);
  if (second === null) return null;
  return { c1: first.c, r1: first.r, c2: second.c, r2: second.r };
}

/** Parse a fragment (with or without the leading `#`). Unknown keys are
 *  ignored; a fragment that is no key=value list is a heading anchor. */
export function parseEmbedFragment(fragment: string | null | undefined): EmbedFragment {
  const f = (fragment ?? "").replace(/^#/, "").trim();
  if (f === "") return {};
  const lines = /^L(\d+)(?:C\d+)?(?:-L?(\d+)(?:C\d+)?)?$/.exec(f);
  if (lines !== null) {
    const start = Number(lines[1]);
    const end = lines[2] !== undefined ? Number(lines[2]) : start;
    if (start >= 1) return { lines: { start, end: Math.max(start, end) } };
    return {};
  }
  if (!/^[a-z]+=/i.test(f)) return { anchor: decode(f) };
  const out: EmbedFragment = {};
  for (const pair of f.split("&")) {
    const eq = pair.indexOf("=");
    if (eq <= 0) continue;
    const key = pair.slice(0, eq).toLowerCase();
    const value = decode(pair.slice(eq + 1));
    switch (key) {
      case "page": {
        const n = positiveInt(value);
        if (n !== null) out.page = n;
        break;
      }
      case "xywh": {
        const r = parseRegion(value);
        if (r !== null) out.region = r;
        break;
      }
      case "t": {
        const [a, b] = value.split(",");
        const start = a === undefined || a === "" ? 0 : parseClock(a);
        const end = b === undefined || b === "" ? null : parseClock(b);
        if (start !== null) out.time = end !== null && end > start ? { start, end } : { start };
        break;
      }
      case "row": {
        const m = /^(\d+)(?:-(\d+|\*))?$/.exec(value);
        if (m === null) break;
        const start = Number(m[1]);
        const end = m[2] === undefined ? start : m[2] === "*" ? null : Number(m[2]);
        if (start >= 1 && (end === null || end >= start)) out.rows = { start, end };
        break;
      }
      case "sheet":
        if (value !== "") out.sheet = value;
        break;
      case "range": {
        const r = parseRange(value);
        if (r !== null) out.range = r;
        break;
      }
      case "cell": {
        const n = positiveInt(value);
        if (n !== null) out.cell = n;
        break;
      }
      case "slide": {
        const n = positiveInt(value);
        if (n !== null) out.slide = n;
        break;
      }
      default:
        break;
    }
  }
  return out;
}

function clock(s: number): string {
  const t = Math.round(s * 10) / 10;
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const sec = t % 60;
  const ss = (Number.isInteger(sec) ? String(sec) : sec.toFixed(1)).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

function columnLetters(n: number): string {
  let s = "";
  let v = n;
  while (v > 0) {
    const r = (v - 1) % 26;
    s = String.fromCharCode(65 + r) + s;
    v = Math.floor((v - 1) / 26);
  }
  return s;
}

function rangeLabel(r: CellRange): string {
  const ref = (c: number | null, row: number | null) =>
    `${c !== null ? columnLetters(c) : ""}${row !== null ? row : ""}`;
  const a = ref(r.c1, r.r1);
  const b = ref(r.c2, r.r2);
  return a === b ? a : `${a}:${b}`;
}

/** The card header's short name for the piece shown ("lines 10–30",
 *  "page 3", "rows 5–9", "0:30–0:45"); empty for a whole file. */
export function fragmentLabel(f: EmbedFragment): string {
  const parts: string[] = [];
  if (f.lines !== undefined) {
    parts.push(
      f.lines.end > f.lines.start ? `lines ${f.lines.start}–${f.lines.end}` : `line ${f.lines.start}`,
    );
  }
  if (f.page !== undefined) parts.push(`page ${f.page}`);
  if (f.slide !== undefined) parts.push(`slide ${f.slide}`);
  if (f.cell !== undefined) parts.push(`cell ${f.cell}`);
  if (f.rows !== undefined) {
    const { start, end } = f.rows;
    parts.push(end === start ? `row ${start}` : `rows ${start}–${end ?? "end"}`);
  }
  if (f.sheet !== undefined || f.range !== undefined) {
    const range = f.range !== undefined ? rangeLabel(f.range) : "";
    parts.push(f.sheet !== undefined ? (range !== "" ? `${f.sheet}!${range}` : f.sheet) : range);
  }
  if (f.time !== undefined) {
    parts.push(f.time.end !== undefined ? `${clock(f.time.start)}–${clock(f.time.end)}` : `from ${clock(f.time.start)}`);
  }
  if (f.region !== undefined) parts.push("region");
  if (f.anchor !== undefined) parts.push(`# ${f.anchor}`);
  return parts.join(" · ");
}

/** Where the full viewer should land when the card is opened in a pane. */
export function fragmentReveal(f: EmbedFragment): Reveal | undefined {
  const r: Reveal = { line: f.lines?.start ?? f.rows?.start ?? 1 };
  let any = f.lines !== undefined || f.rows !== undefined;
  if (f.lines !== undefined && f.lines.end > f.lines.start) r.endLine = f.lines.end;
  if (f.page !== undefined) {
    r.page = f.page;
    any = true;
  }
  if (f.cell !== undefined) {
    r.cell = f.cell;
    any = true;
  }
  if (f.slide !== undefined) {
    r.slide = f.slide;
    any = true;
  }
  if (f.time !== undefined) {
    r.time = f.time.end !== undefined ? { start: f.time.start, end: f.time.end } : { start: f.time.start };
    any = true;
  }
  if (f.region !== undefined && f.region.unit === "pixel") {
    r.region = { x: f.region.x, y: f.region.y, w: f.region.w, h: f.region.h };
    any = true;
  }
  return any ? r : undefined;
}

/**
 * The rows of a delimited file to fetch for `rows`, as fs/table's data-row
 * window. RFC 7111 counts the header line as row 1 when the file has one,
 * so `row=2` is the first data row there; a header-less file (BED, SAM)
 * starts its data at row 1. No selection → the first `peek` rows. At most
 * `cap` rows either way.
 */
export function tableWindow(
  rows: EmbedFragment["rows"],
  hasHeader: boolean,
  peek = 10,
  cap = 50,
): { offset: number; limit: number } {
  if (rows === undefined) return { offset: 0, limit: peek };
  const toData = (r: number) => (hasHeader ? r - 2 : r - 1);
  const first = Math.max(0, toData(rows.start));
  const last = rows.end === null ? first + peek - 1 : toData(rows.end);
  const limit = Math.min(cap, Math.max(1, last - first + 1));
  return { offset: first, limit };
}
