/**
 * The DOM half of quoting a transcript passage (the context bridge's chat
 * source): which live selection a transcript can quote, and where its chip
 * floats. ChatView owns the state and the listeners.
 */

/** The live selection's range when it lies in `column` (and not in a card's
 *  text field), else null. */
export function quotableRange(column: HTMLElement): Range | null {
  const sel = document.getSelection();
  if (sel === null || sel.rangeCount === 0 || sel.isCollapsed) return null;
  const range = sel.getRangeAt(0);
  const common = range.commonAncestorContainer;
  if (!column.contains(common)) return null;
  const el = common instanceof Element ? common : common.parentElement;
  return el?.closest("input, textarea, [contenteditable]") == null ? range : null;
}

/**
 * Where the chip floats for `range`, relative to `host`: just past the
 * selection's last line of text, kept inside `scroller`'s visible box (a
 * selection running off screen still offers its chip at the edge). `size`
 * is the chip's rendered box.
 */
export function quoteChipPosition(
  range: Range,
  host: HTMLElement,
  scroller: HTMLElement,
  size: { width: number; height: number },
): { x: number; y: number } {
  const rects = range.getClientRects();
  // A selection ending at the start of the next block ends in an empty rect.
  let last: DOMRect | null = null;
  for (let i = rects.length - 1; i >= 0 && last === null; i--) {
    if (rects[i].width > 0) last = rects[i];
  }
  const end = last ?? range.getBoundingClientRect();
  const box = host.getBoundingClientRect();
  const view = scroller.getBoundingClientRect();
  const clamp = (n: number, lo: number, hi: number) => Math.min(Math.max(n, lo), Math.max(lo, hi));
  return {
    x: clamp(end.right - box.left + 4, view.left - box.left + 4, view.right - box.left - size.width - 4),
    y: clamp(end.bottom - box.top + 6, view.top - box.top + 4, view.bottom - box.top - size.height - 4),
  };
}
