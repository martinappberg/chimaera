/**
 * The Extensions surface's glyph — the "extensions" idiom: a 2×2 grid of
 * rounded squares, three outlined (top-left, bottom-left, bottom-right) and
 * the top-right cell a plus. One definition for every place it is drawn
 * (the pane tab, the dock row), each wrapping it in its own <svg>.
 *
 * Drawn on the hand-made surface glyphs' grid (PaneTabs / the dock): a
 * 16×16 viewBox, currentColor stroke, round caps and joins, stroke width
 * `EXTENSIONS_STROKE`. The cells are 4 units with a 2.6 gap (Tabler's
 * layout-grid-add proportions at 16), so the ink spans ~2–14 like its
 * neighbours; 1.3 (the diff glyph's width, not the hexagon's 1.4) keeps a
 * four-cell grid from reading heavier than a single outline and leaves a
 * visible gap between the cells at 11–12px.
 *
 * Not in shared/icons.ts: that file is regenerated from Tabler on every
 * build (`prebuild`), and holds 24×24 file-type glyphs.
 */
/** The path `d`: three cells (4 units, corner radius .9, at 2.7 and 9.3),
 *  then the plus centred in the fourth. */
export const EXTENSIONS_GLYPH =
  "M3.6 2.7h2.2a.9 .9 0 0 1 .9 .9v2.2a.9 .9 0 0 1-.9 .9h-2.2a.9 .9 0 0 1-.9-.9v-2.2a.9 .9 0 0 1 .9-.9z" +
  "M3.6 9.3h2.2a.9 .9 0 0 1 .9 .9v2.2a.9 .9 0 0 1-.9 .9h-2.2a.9 .9 0 0 1-.9-.9v-2.2a.9 .9 0 0 1 .9-.9z" +
  "M10.2 9.3h2.2a.9 .9 0 0 1 .9 .9v2.2a.9 .9 0 0 1-.9 .9h-2.2a.9 .9 0 0 1-.9-.9v-2.2a.9 .9 0 0 1 .9-.9z" +
  "M11.3 2.7v4M9.3 4.7h4";

export const EXTENSIONS_STROKE = 1.3;
