/**
 * The Parquet viewer's pure arithmetic, kept out of the IO module so it can be
 * tested headless: which row groups a page of rows touches, how much of a
 * column chunk's page list a read needs, the byte-range cache the ranged reads
 * go through, and how a decoded value reads as a grid cell.
 */

/** A row group's place in the file's row space. */
export interface GroupSpan {
  index: number;
  /** First row of the group (file-wide, 0-based). */
  start: number;
  rows: number;
}

/** Row-group spans from their row counts. */
export function groupSpans(rowCounts: readonly number[]): GroupSpan[] {
  const out: GroupSpan[] = [];
  let start = 0;
  rowCounts.forEach((rows, index) => {
    out.push({ index, start, rows });
    start += rows;
  });
  return out;
}

/** The part of each row group a read of `[offset, offset + limit)` touches,
 *  as group-relative `[from, to)`. */
export function groupsForRange(
  spans: readonly GroupSpan[],
  offset: number,
  limit: number,
): { span: GroupSpan; from: number; to: number }[] {
  const end = offset + limit;
  const out: { span: GroupSpan; from: number; to: number }[] = [];
  for (const span of spans) {
    const gEnd = span.start + span.rows;
    if (span.rows <= 0 || gEnd <= offset || span.start >= end) continue;
    out.push({ span, from: Math.max(0, offset - span.start), to: Math.min(span.rows, end - span.start) });
  }
  return out;
}

/** The clamped `[start, end)` rows a grid page asks for (`end` may be short). */
export function pageBounds(offset: number, limit: number, total: number): { start: number; end: number } {
  const start = Math.min(Math.max(0, Math.floor(offset)), total);
  return { start, end: Math.min(total, start + Math.max(0, Math.floor(limit))) };
}

/**
 * A walked column chunk's page list is enough for a read ending at
 * group-relative row `to` once the pages seen so far reach past it (or the
 * chunk is exhausted): hyparquet takes a page's end from the next page's
 * first row, so every page the read touches must have its end known.
 */
export function walkCovers(walkedRows: number, done: boolean, to: number): boolean {
  return done || walkedRows >= to;
}

/** `Content-Range: bytes a-b/total` → total (null when absent or `*`). */
export function contentRangeTotal(header: string | null): number | null {
  if (header === null) return null;
  const m = /^bytes\s+(?:\d+-\d+|\*)\/(\d+)\s*$/i.exec(header.trim());
  return m === null ? null : Number(m[1]);
}

/**
 * A byte-range cache with a total-size cap: a request is served from any
 * cached range that contains it, so the footer read covers the metadata and a
 * page's bytes serve the next grid page that lands on it. Least recently used
 * ranges go first once the cap is passed.
 */
export class ByteCache {
  private entries: { start: number; end: number; buf: ArrayBuffer }[] = [];
  private bytes = 0;
  constructor(private readonly cap: number) {}

  get size(): number {
    return this.bytes;
  }

  /** `[start, end)` from a cached range holding all of it, or null. */
  get(start: number, end: number): ArrayBuffer | null {
    for (let i = this.entries.length - 1; i >= 0; i--) {
      const e = this.entries[i];
      if (e.start <= start && e.end >= end) {
        // Most recently used goes last.
        if (i !== this.entries.length - 1) {
          this.entries.splice(i, 1);
          this.entries.push(e);
        }
        return start === e.start && end === e.end ? e.buf : e.buf.slice(start - e.start, end - e.start);
      }
    }
    return null;
  }

  put(start: number, buf: ArrayBuffer): void {
    const end = start + buf.byteLength;
    if (buf.byteLength > this.cap) return;
    // A new range swallows any it contains.
    this.entries = this.entries.filter((e) => {
      const inside = e.start >= start && e.end <= end;
      if (inside) this.bytes -= e.buf.byteLength;
      return !inside;
    });
    this.entries.push({ start, end, buf });
    this.bytes += buf.byteLength;
    while (this.bytes > this.cap && this.entries.length > 1) {
      const old = this.entries.shift();
      if (old !== undefined) this.bytes -= old.buf.byteLength;
    }
  }
}

/** How a column's values read: dates without a clock, 32-bit floats at their
 *  own precision (not float64's widened digits), the rest by value. */
export type CellKind = "date" | "float32" | "value";

/** Longest text a single cell carries (the expand popover shows this much). */
export const CELL_MAX_CHARS = 4000;

function pad(n: number, w = 2): string {
  return String(n).padStart(w, "0");
}

/** A UTC timestamp as `YYYY-MM-DD HH:MM:SS[.mmm]`, or the date alone. */
export function formatDate(d: Date, kind: CellKind): string {
  const t = d.getTime();
  if (!Number.isFinite(t)) return "invalid date";
  const day = `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`;
  if (kind === "date") return day;
  const ms = d.getUTCMilliseconds();
  const clock = `${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}`;
  return `${day} ${clock}${ms === 0 ? "" : `.${pad(ms, 3)}`}`;
}

function bytesPreview(b: Uint8Array): string {
  const hex = Array.from(b.subarray(0, 32), (x) => x.toString(16).padStart(2, "0")).join(" ");
  return b.length > 32 ? `${hex} … (${b.length} bytes)` : hex;
}

/** JSON for nested values, with bigints, dates and bytes made readable. */
function nestedJson(v: unknown): string {
  return JSON.stringify(v, (_k, x: unknown) => {
    // A 64-bit integer that fits reads as a number, not a quoted string.
    if (typeof x === "bigint") return Number.isSafeInteger(Number(x)) ? Number(x) : x.toString();
    if (x instanceof Uint8Array) return bytesPreview(x);
    return x;
  });
}

/** Control characters drawn as their Unicode pictures (a binary column read
 *  as text would otherwise show as blanks). */
function visible(s: string): string {
  return /[\u0000-\u0008\u000b-\u001f\u007f]/.test(s)
    ? s.replace(/[\u0000-\u0008\u000b-\u001f]/g, (c) => String.fromCharCode(0x2400 + c.charCodeAt(0))).replace(/\u007f/g, "\u2421")
    : s;
}

/** A decoded Parquet value as grid text. Null reads as empty, like CSV. */
export function formatCell(v: unknown, kind: CellKind = "value"): string {
  let s: string;
  if (v === null || v === undefined) return "";
  if (typeof v === "string") s = visible(v);
  else if (typeof v === "number") s = kind === "float32" && Number.isFinite(v) ? String(Number(v.toPrecision(7))) : String(v);
  else if (typeof v === "bigint" || typeof v === "boolean") s = String(v);
  else if (v instanceof Date) s = formatDate(v, kind);
  else if (v instanceof Uint8Array) s = bytesPreview(v);
  else {
    try {
      s = nestedJson(v);
    } catch {
      s = String(v);
    }
  }
  return s.length > CELL_MAX_CHARS ? `${s.slice(0, CELL_MAX_CHARS)}…` : s;
}
