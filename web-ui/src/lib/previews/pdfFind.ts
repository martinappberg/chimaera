/**
 * Find-in-PDF, the pure half: one page's text as pdf.js lays it out, a
 * query compiled to a forgiving pattern, and each match mapped back onto the
 * text items the text layer turns into spans (so a match can be highlighted
 * in place). `PdfView.svelte` owns the page walk and the painting.
 */

/** One page's searchable text: every text item's string in order — the same
 *  items, in the same order, that pdf.js's TextLayer renders as spans — with
 *  a space standing in for each end of line. */
export interface PageText {
  text: string;
  /** Offset of item i's string in `text`. */
  starts: number[];
  lengths: number[];
}

/** A slice of one text item (an index among the items that carry a string). */
export interface ItemRange {
  item: number;
  from: number;
  to: number;
}

export function pageText(items: readonly { str: string; hasEOL?: boolean }[]): PageText {
  let text = "";
  const starts: number[] = [];
  const lengths: number[] = [];
  for (const it of items) {
    starts.push(text.length);
    lengths.push(it.str.length);
    text += it.str;
    if (it.hasEOL === true) text += " ";
  }
  return { text, starts, lengths };
}

/** Typographic ligatures PDFs keep as single glyphs ("ﬁgure"): a query's
 *  plain letters match either spelling. Longest first. */
const LIGATURES: [string, string][] = [
  ["ffi", "ﬃ"],
  ["ffl", "ﬄ"],
  ["ff", "ﬀ"],
  ["fi", "ﬁ"],
  ["fl", "ﬂ"],
];

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function wordPattern(word: string): string {
  let out = "";
  let i = 0;
  outer: while (i < word.length) {
    for (const [plain, lig] of LIGATURES) {
      if (word.slice(i, i + plain.length).toLowerCase() === plain) {
        out += `(?:${escapeRe(word.slice(i, i + plain.length))}|${lig})`;
        i += plain.length;
        continue outer;
      }
    }
    out += escapeRe(word[i]);
    i += 1;
  }
  return out;
}

/**
 * The pattern for `query`: case-insensitive, and forgiving about spacing —
 * PDF text often splits words into items with no space between them, or
 * breaks a phrase across lines — so the words may be separated by any
 * whitespace or none. Null for a blank query.
 */
export function findPattern(query: string): RegExp | null {
  const words = query.trim().split(/\s+/).filter((w) => w !== "");
  if (words.length === 0) return null;
  return new RegExp(words.map(wordPattern).join("\\s*"), "giu");
}

/** The item slices covering text offsets [start, end). */
export function itemRanges(pt: PageText, start: number, end: number): ItemRange[] {
  // First item whose string ends after `start` (binary search on starts).
  let lo = 0;
  let hi = pt.starts.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (pt.starts[mid] + pt.lengths[mid] <= start) lo = mid + 1;
    else hi = mid;
  }
  const out: ItemRange[] = [];
  for (let i = lo; i < pt.starts.length && pt.starts[i] < end; i++) {
    const from = Math.max(start, pt.starts[i]) - pt.starts[i];
    const to = Math.min(end, pt.starts[i] + pt.lengths[i]) - pt.starts[i];
    if (to > from) out.push({ item: i, from, to });
  }
  return out;
}

/** Every match of `re` in one page (at most `limit`), as item slices. A
 *  match that covers only the synthetic end-of-line spaces is skipped. */
export function findInPage(pt: PageText, re: RegExp, limit: number): ItemRange[][] {
  const out: ItemRange[][] = [];
  if (limit <= 0) return out;
  re.lastIndex = 0;
  for (const m of pt.text.matchAll(re)) {
    if (m[0].length === 0) continue;
    const ranges = itemRanges(pt, m.index, m.index + m[0].length);
    if (ranges.length > 0) out.push(ranges);
    if (out.length >= limit) break;
  }
  return out;
}
