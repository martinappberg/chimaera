/**
 * Keyboard-reachable scroll hosts: the scrollable-region pattern (tabindex=0 +
 * role=region + a label) applied ONLY while a scroller actually overflows, so
 * a table that fits is never a tab stop. WebKit never makes an overflow
 * container keyboard-focusable on its own (Chromium 130+ and Firefox do, but
 * Chromium withholds it when the scroller holds a focusable child such as a
 * link), so without this a keyboard-only user can't reach a wide table's
 * off-screen columns — WCAG 2.1.1.
 *
 * Attributes only: both markdown hosts render via `{@html}` and are
 * append-only after render for STRUCTURAL changes (copyDecor.ts); attribute
 * writes on existing nodes are within that contract. A mark this module set
 * is remembered in `data-scroll-region` so only its own attributes are ever
 * removed — sanitized agent HTML may carry a role of its own.
 *
 * One ResizeObserver for the whole app: a host's width changes on pane
 * resizes (no window resize fires), and a per-host observer would mean one
 * per chat message. Width-axis hosts ignore height-only entries, so a
 * streaming message growing line by line re-measures nothing.
 */

export type ScrollAxis = "x" | "y";

const MARK = "scrollRegion";

/** Mark `el` as a focusable region iff it overflows on `axis`; clear a mark
 *  this module set once it no longer does. Reads layout — call after render. */
export function markScrollRegion(el: HTMLElement, axis: ScrollAxis, label: string): void {
  const overflows =
    axis === "x" ? el.scrollWidth > el.clientWidth : el.scrollHeight > el.clientHeight;
  if (overflows) {
    if (el.getAttribute("tabindex") !== "0") el.setAttribute("tabindex", "0");
    if (el.getAttribute("role") !== "region") el.setAttribute("role", "region");
    if (el.getAttribute("aria-label") !== label) el.setAttribute("aria-label", label);
    el.dataset[MARK] = "1";
  } else if (el.dataset[MARK] !== undefined) {
    el.removeAttribute("tabindex");
    el.removeAttribute("role");
    el.removeAttribute("aria-label");
    delete el.dataset[MARK];
  }
}

/** Mark every `selector` match under `root` (see markScrollRegion). */
export function markScrollRegions(
  root: ParentNode,
  selector: string,
  axis: ScrollAxis,
  label: string,
): void {
  for (const el of root.querySelectorAll<HTMLElement>(selector)) markScrollRegion(el, axis, label);
}

type Watched = { size: number; recheck: () => void; axis: ScrollAxis };
const watched = new WeakMap<Element, Watched>();
let observer: ResizeObserver | null = null;

/** Re-run `recheck` whenever `host`'s size changes on the axis that matters
 *  (its width for x-scrollers inside it; any dimension for a y-scroller).
 *  The first observation fires with the current size, so a fresh host is
 *  checked once after layout. Returns the teardown. */
export function watchScrollRegions(host: HTMLElement, axis: ScrollAxis, recheck: () => void): () => void {
  if (typeof ResizeObserver === "undefined") return () => {};
  observer ??= new ResizeObserver((entries) => {
    for (const entry of entries) {
      const w = watched.get(entry.target);
      if (w === undefined) continue;
      const { width, height } = entry.contentRect;
      const size = w.axis === "x" ? width : width * 1e6 + height;
      if (size === w.size) continue;
      w.size = size;
      w.recheck();
    }
  });
  watched.set(host, { size: -1, recheck, axis });
  observer.observe(host);
  return () => {
    watched.delete(host);
    observer?.unobserve(host);
  };
}
