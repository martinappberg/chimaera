/**
 * The reader's anchor in a scrolled-up transcript: the top-level row at the
 * viewport's top edge, and where it sits inside the transcript column. A
 * change of that position while the reader is not following the tail is a
 * layout shift above them (a page mounted, a trim, a preview decoding, a fold
 * regrouping) — ChatView absorbs it with the history spacer so the text under
 * the reader stays put. WebKit has no native scroll anchoring, and the
 * transcript opts out of Chromium's (`overflow-anchor: none`), so this is the
 * one anchoring mechanism on every engine.
 *
 * Positions are layout offsets (`offsetTop`), not client rects: they ignore
 * transforms, so a row's 3px mount-rise animation never reads as a shift.
 */
export interface ReadingAnchor {
  /** The top-level row at the viewport's top edge. */
  row: HTMLElement;
  /** Block uid of the row (a group's or fold's first row) — survives remounts. */
  uid: number | null;
  /** Source index range the row covers; the fallback once its uid is gone. */
  index: number | null;
  end: number | null;
  /** Layout offset of the row's top inside the column. */
  rowOffset: number;
  /** The element inside the row at the viewport's top edge (the row itself
   *  when it has none): a figure resolving ABOVE it within the same row
   *  moves the text being read, which the row's own top never shows — one
   *  reply can hold several figures and run thousands of px. */
  node: HTMLElement;
  /** Layout offset of `node`'s top inside the column. */
  offset: number;
}

function numberAttr(node: HTMLElement, name: string): number | null {
  const raw = node.getAttribute(name);
  if (raw === null) return null;
  const value = Number(raw);
  return Number.isFinite(value) ? value : null;
}

/** `node`'s top relative to `column`'s top, transform-free. Walks the
 *  offsetParent chain so a positioned wrapper between them cannot skew it. */
function offsetWithin(node: HTMLElement, column: HTMLElement): number {
  const stop = column.offsetParent;
  let y = 0;
  let el: Element | null = node;
  while (el instanceof HTMLElement && el !== stop) {
    y += el.offsetTop;
    el = el.offsetParent;
  }
  return y - column.offsetTop;
}

function isRow(el: Element): el is HTMLElement {
  return el instanceof HTMLElement && el.hasAttribute("data-block-uid");
}

/** Elements the anchor never descends into: cards re-render their insides
 *  (a picture's frame, a PDF canvas), and math, code and tables are read
 *  as one piece. */
const LEAF = ".md-embed, .embed-card, pre, table, .md-table, .katex, details, button, img, canvas, iframe, video";

/** `el`'s laid-out children, looking through `display: contents` wrappers
 *  (Markdown's shell and its streaming segments have no box of their own). */
function* boxedChildren(el: HTMLElement): Generator<HTMLElement> {
  for (const child of el.children) {
    if (!(child instanceof HTMLElement)) continue;
    if (child.getClientRects().length === 0 && child.children.length > 0) yield* boxedChildren(child);
    else yield child;
  }
}

/** The element inside `row` at the viewport's top edge: the first child
 *  reaching below it, descended into while it straddles the edge. */
function edgeNode(row: HTMLElement, viewportTop: number): HTMLElement {
  let node = row;
  for (let depth = 0; depth < 6 && !node.matches(LEAF); depth++) {
    let next: HTMLElement | null = null;
    for (const child of boxedChildren(node)) {
      const rect = child.getBoundingClientRect();
      if (rect.bottom <= viewportTop) continue;
      next = child;
      break;
    }
    if (next === null) break;
    node = next;
    if (node.getBoundingClientRect().top >= viewportTop) break;
  }
  return node;
}

/** The first top-level row whose bottom is below the viewport's top edge
 *  (binary search: the column stacks its children vertically). */
export function selectAnchor(scroller: HTMLElement, column: HTMLElement): ReadingAnchor | null {
  const children = column.children;
  const viewportTop = scroller.getBoundingClientRect().top;
  let lo = 0;
  let hi = children.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (children[mid].getBoundingClientRect().bottom > viewportTop) hi = mid;
    else lo = mid + 1;
  }
  // Chrome (sentinel, tail cards) carries no uid; take the nearest row below,
  // else the nearest above.
  let node: HTMLElement | null = null;
  for (let i = lo; i < children.length && node === null; i++) {
    if (isRow(children[i])) node = children[i] as HTMLElement;
  }
  for (let i = lo - 1; i >= 0 && node === null; i--) {
    if (isRow(children[i])) node = children[i] as HTMLElement;
  }
  if (node === null) return null;
  const index = numberAttr(node, "data-block-index");
  const rowOffset = offsetWithin(node, column);
  const edge = node.getBoundingClientRect().top < viewportTop ? edgeNode(node, viewportTop) : node;
  return {
    row: node,
    uid: numberAttr(node, "data-block-uid"),
    index,
    end: numberAttr(node, "data-block-end") ?? index,
    rowOffset,
    node: edge,
    offset: edge === node ? rowOffset : offsetWithin(edge, column),
  };
}

/** The source-index range (end exclusive) of the rows that come within
 *  `reach` px of the viewport, above its top edge or below its bottom edge.
 *  Null when no row is mounted there. */
export function rowsInReach(
  scroller: HTMLElement,
  column: HTMLElement,
  reach: number,
): { start: number; end: number } | null {
  const view = scroller.getBoundingClientRect();
  const top = view.top - reach;
  const bottom = view.bottom + reach;
  let start: number | null = null;
  let end: number | null = null;
  for (const child of column.children) {
    if (!isRow(child)) continue;
    const index = numberAttr(child, "data-block-index");
    if (index === null) continue;
    const rect = child.getBoundingClientRect();
    if (rect.bottom <= top) continue;
    if (rect.top >= bottom) break;
    start ??= index;
    end = numberAttr(child, "data-block-end") ?? index;
  }
  return start === null || end === null ? null : { start, end: end + 1 };
}

/** Re-find the anchored row after the column re-rendered: node identity
 *  (uid-keyed rows survive range writes), then its uid (a row folded into an
 *  activity fold, or a remounted group, keeps its first row's uid), then any
 *  top-level row covering its source index. */
function resolve(anchor: ReadingAnchor, column: HTMLElement): HTMLElement | null {
  if (anchor.row.parentElement === column) return anchor.row;
  const rows = Array.from(column.children).filter(isRow);
  if (anchor.uid !== null) {
    const byUid = rows.find((row) => numberAttr(row, "data-block-uid") === anchor.uid);
    if (byUid !== undefined) return byUid;
  }
  if (anchor.index !== null) {
    const target = anchor.index;
    const byIndex = rows.find((row) => {
      const start = numberAttr(row, "data-block-index");
      const end = numberAttr(row, "data-block-end") ?? start;
      return start !== null && end !== null && start <= target && end >= target;
    });
    if (byIndex !== undefined) return byIndex;
  }
  return null;
}

/** How far the anchored text moved inside the column since it was recorded
 *  (positive: pushed down by growth above it), with the anchor re-pinned at
 *  its current nodes and offsets: the element at the edge while it is still
 *  in the row, else the row (a re-render replaced its insides). Null when
 *  the row is gone entirely. */
export function measureShift(
  anchor: ReadingAnchor,
  column: HTMLElement,
): { shift: number; anchor: ReadingAnchor } | null {
  const row = resolve(anchor, column);
  if (row === null) return null;
  const rowOffset = offsetWithin(row, column);
  if (anchor.node !== anchor.row && row.contains(anchor.node)) {
    const offset = offsetWithin(anchor.node, column);
    return { shift: offset - anchor.offset, anchor: { ...anchor, row, rowOffset, offset } };
  }
  return {
    shift: rowOffset - anchor.rowOffset,
    anchor: { ...anchor, row, rowOffset, node: row, offset: rowOffset },
  };
}
