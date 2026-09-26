/**
 * Pure arithmetic behind the table grid (`TableView.svelte`): which rows of
 * the loaded window the DOM holds, how wide each column starts, and how the
 * footer names a row count. No DOM here, so Vitest covers it headless.
 */

/** The slice of rows a virtualized grid renders, and the spacer heights that
 *  stand in for everything above and below it. */
export interface RowWindow {
  /** First rendered row (index into the loaded rows). */
  start: number;
  /** One past the last rendered row. */
  end: number;
  padTop: number;
  padBottom: number;
}

/**
 * Rows visible at `scrollTop` in a `viewport`-tall scroller of fixed-height
 * rows, plus `overscan` rows either side so a fast scroll never shows a gap
 * before the next frame renders.
 */
export function virtualWindow(
  scrollTop: number,
  viewport: number,
  rowHeight: number,
  count: number,
  overscan: number,
): RowWindow {
  if (count <= 0 || rowHeight <= 0) return { start: 0, end: 0, padTop: 0, padBottom: 0 };
  const first = Math.floor(Math.max(0, scrollTop) / rowHeight);
  const visible = Math.ceil(Math.max(0, viewport) / rowHeight) + 1;
  const start = Math.min(count, Math.max(0, first - overscan));
  const end = Math.min(count, Math.max(start, first + visible + overscan));
  return { start, end, padTop: start * rowHeight, padBottom: (count - end) * rowHeight };
}

/** The first loaded row showing at `scrollTop` (0-based, clamped). */
export function rowAt(scrollTop: number, rowHeight: number, count: number): number {
  if (count <= 0 || rowHeight <= 0) return 0;
  return Math.min(count - 1, Math.max(0, Math.floor(scrollTop / rowHeight)));
}

/**
 * Starting column widths (px) for a monospace grid: the longest of the header
 * and a sample of cells, in characters, times the font's advance, plus cell
 * padding — clamped so one long cell cannot claim the pane. A virtualized
 * grid needs fixed widths up front: content-sized columns would jump as rows
 * scroll in and out.
 */
export function autoColumnWidths(
  columns: readonly string[],
  sample: readonly (readonly string[])[],
  charWidth: number,
  opts: { pad: number; min: number; maxChars: number },
): number[] {
  const width = Math.max(columns.length, ...sample.map((r) => r.length));
  const out: number[] = [];
  for (let c = 0; c < width; c++) {
    let chars = columns[c]?.length ?? 0;
    for (const row of sample) {
      const len = row[c]?.length ?? 0;
      if (len > chars) chars = len;
    }
    const px = Math.ceil(Math.min(chars, opts.maxChars) * charWidth) + opts.pad;
    out.push(Math.max(opts.min, px));
  }
  return out;
}

/** A row count for the footer: exact counts in full ("12,345"), estimates
 *  compact and marked ("~1.2M"). */
export function formatRowCount(n: number, exact: boolean): string {
  if (exact) return n.toLocaleString("en-US");
  if (n < 10_000) return `~${n.toLocaleString("en-US")}`;
  const [value, unit] = n >= 1e9 ? [n / 1e9, "B"] : n >= 1e6 ? [n / 1e6, "M"] : [n / 1e3, "k"];
  const digits = value >= 100 ? 0 : 1;
  return `~${Number(value.toFixed(digits)).toLocaleString("en-US")}${unit}`;
}

/** Rows fetched above a jump target, so it lands with context above it. */
export const JUMP_CONTEXT = 20;

/** The 0-based offset to fetch for a jump to 1-based `row`. */
export function jumpOffset(row: number): number {
  return Math.max(0, Math.floor(row) - 1 - JUMP_CONTEXT);
}

/** Parse a typed row number ("1,500,000", " 42 ") into a 1-based row, or
 *  null when it is not a positive whole number. */
export function parseRowNumber(text: string): number | null {
  const digits = text.replace(/[\s,_]/g, "");
  if (!/^\d+$/.test(digits)) return null;
  const n = Number.parseInt(digits, 10);
  return Number.isSafeInteger(n) && n >= 1 ? n : null;
}
