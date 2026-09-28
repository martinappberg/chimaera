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
  node: HTMLElement;
  /** Block uid of the row (a group's or fold's first row) — survives remounts. */
  uid: number | null;
  /** Source index range the row covers; the fallback once its uid is gone. */
  index: number | null;
  end: number | null;
  /** Layout offset of the row's top inside the column. */
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
  return {
    node,
    uid: numberAttr(node, "data-block-uid"),
    index,
    end: numberAttr(node, "data-block-end") ?? index,
    offset: offsetWithin(node, column),
  };
}

/** Re-find the anchored row after the column re-rendered: node identity
 *  (uid-keyed rows survive range writes), then its uid (a row folded into an
 *  activity fold, or a remounted group, keeps its first row's uid), then any
 *  top-level row covering its source index. */
function resolve(anchor: ReadingAnchor, column: HTMLElement): HTMLElement | null {
  if (anchor.node.parentElement === column) return anchor.node;
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

/** How far the anchored row moved inside the column since it was recorded
 *  (positive: pushed down by growth above it), with the anchor re-pinned at
 *  its current node and offset. Null when the row is gone entirely. */
export function measureShift(
  anchor: ReadingAnchor,
  column: HTMLElement,
): { shift: number; anchor: ReadingAnchor } | null {
  const node = resolve(anchor, column);
  if (node === null) return null;
  const offset = offsetWithin(node, column);
  return { shift: offset - anchor.offset, anchor: { ...anchor, node, offset } };
}
