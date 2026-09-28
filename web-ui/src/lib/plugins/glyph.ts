/**
 * The Extensions surface's glyph — the "extensions" idiom: a 2×2 grid of
 * squares, three outlined (top-left, bottom-left, bottom-right) and the
 * top-right cell a plus. One definition for every place it is drawn (the
 * pane tab, the rail row, the quick-open row), all through
 * `ExtensionsGlyph.svelte`.
 *
 * Drawn for the pixel grid, at exactly `EXTENSIONS_SIZE` CSS px (never
 * scaled — a scaled grid lands between pixels and blurs): a 12-unit box, a
 * 1-unit stroke on half-unit centre lines so every edge is a whole pixel.
 * Each cell is 5 px with a 2 px gap between cells (5 + 2 + 5 = 12). The
 * plus needs an odd cell to sit exactly in its middle: its bars run down
 * column 9 and along row 2 of the 7–11 cell, butt-capped so they end on
 * the cell's edges. Its caller places the box on whole pixels (the
 * surrounding flex rows are sized so it does).
 *
 * Not in shared/icons.ts: that file is regenerated from Tabler on every
 * build (`prebuild`), and holds 24×24 file-type glyphs.
 */

/** The box: CSS px, and the viewBox's units (one unit = one pixel). */
export const EXTENSIONS_SIZE = 12;

/** The three outlined cells, then the plus (vertical, horizontal). */
export const EXTENSIONS_GLYPH = "M.5 .5h4v4h-4zM.5 7.5h4v4h-4zM7.5 7.5h4v4h-4zM9.5 0v5M7 2.5h5";

export const EXTENSIONS_STROKE = 1;
