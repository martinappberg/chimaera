/**
 * Keyboard-reachable horizontal scrollers: `tabindex="0"` (plus, for a host
 * that wraps a table, a group role and a name) applied ONLY while the box
 * actually scrolls sideways, so a table that fits is never a tab stop.
 * WebKit never makes an overflow container keyboard-focusable on its own
 * (Chromium 130+ and Firefox do, but Chromium withholds it when the scroller
 * holds a focusable child such as a link), so without this a keyboard-only
 * user can't reach a wide table's off-screen columns — WCAG 2.1.1.
 *
 * Attributes only: both markdown hosts render via `{@html}` and are
 * append-only after render for STRUCTURAL changes (copyDecor.ts); attribute
 * writes on existing nodes are within that contract. A mark never overwrites
 * an attribute the content brought along (sanitized agent HTML may carry a
 * role, a name, even a tabindex of its own) and only ever removes what it
 * set, remembered per element in a WeakMap — a data attribute would be as
 * forgeable as the class it keys on. A `<table>` that is its own scroller
 * gets NO role: an explicit role would replace its native table role and
 * orphan its rows for a screen reader; the table names itself.
 *
 * Overflow is `scrollWidth > clientWidth`, strict: measured across ~10k
 * fractional layouts, a fitting box never reads wider, while a 1px tolerance
 * would hide real 1px overflows fifteen times as often as it removed a
 * phantom one. Reads layout — mark after render, and read every element
 * before writing any (a tabindex write invalidates style for the next read).
 *
 * One ResizeObserver for the whole app, keyed by element with a set of
 * callbacks each, so a host and its content can share a watcher and two
 * watchers on one element never cancel each other. Overflow moves when the
 * host's width changes (a pane resize — no window resize fires) OR when the
 * content's width changes (a text-size change, a late image or equation), so
 * callers watch both: the scroll box and a content proxy — the inner
 * `<table>` of a host div, or a row group of a self-scrolling table, whose
 * box is the anonymous table's width. Only width is compared, so a streaming
 * message growing line by line re-measures nothing.
 */

/** The role/name pair a host div gets; `null` for an element that already
 *  carries its own semantics (a `<table>`, a code block, an equation). */
export type Region = { role: string; label: string } | null;

/** What to mark under a root: a selector and the region its matches get. */
export type Target = readonly [selector: string, region: Region];

/** Attribute names this module set on an element — the only ones it removes. */
const ours = new WeakMap<Element, string[]>();

function overflowsX(el: Element): boolean {
  return el.scrollWidth > el.clientWidth;
}

function apply(el: HTMLElement, overflows: boolean, region: Region): void {
  const set = ours.get(el);
  if (overflows) {
    if (set !== undefined) return;
    const added: string[] = [];
    const add = (name: string, value: string) => {
      if (el.hasAttribute(name)) return;
      el.setAttribute(name, value);
      added.push(name);
    };
    add("tabindex", "0");
    if (region !== null) {
      add("role", region.role);
      add("aria-label", region.label);
    }
    ours.set(el, added);
  } else if (set !== undefined) {
    // Dropping tabindex under the focused element would send focus to
    // <body>: clear the mark once focus has moved on instead.
    if (typeof document !== "undefined" && document.activeElement === el) {
      el.addEventListener("focusout", () => apply(el, overflowsX(el), region), { once: true });
      return;
    }
    for (const name of set) el.removeAttribute(name);
    ours.delete(el);
  }
}

/** Mark or unmark one scroller by its current overflow. */
export function markScrollRegion(el: HTMLElement, region: Region): void {
  apply(el, overflowsX(el), region);
}

/** Mark every target's matches under `root` — all reads, then all writes,
 *  across every target, so a root costs one layout however many kinds of
 *  scroller it holds. */
export function markScrollRegions(root: ParentNode, targets: readonly Target[]): void {
  const found: Array<[HTMLElement, Region]> = [];
  for (const [selector, region] of targets) {
    for (const el of root.querySelectorAll<HTMLElement>(selector)) found.push([el, region]);
  }
  const states = found.map(([el]) => overflowsX(el));
  found.forEach(([el, region], i) => apply(el, states[i], region));
}

const watchers = new WeakMap<Element, Set<() => void>>();
const lastWidth = new WeakMap<Element, number>();
let observer: ResizeObserver | null = null;

/** Run `recheck` whenever `el`'s content-box width changes; every watcher
 *  gets one check at its current width first (the observer's initial
 *  delivery for a fresh element, a direct call for one already observed).
 *  Callbacks due in one frame run once each. Returns the teardown; the
 *  element stays observed while any watcher remains. */
export function watchWidth(el: Element, recheck: () => void): () => void {
  if (typeof ResizeObserver === "undefined") return () => {};
  observer ??= new ResizeObserver((entries) => {
    const due = new Set<() => void>();
    for (const entry of entries) {
      const width = entry.contentRect.width;
      if (lastWidth.get(entry.target) === width) continue;
      lastWidth.set(entry.target, width);
      watchers.get(entry.target)?.forEach((fn) => due.add(fn));
    }
    due.forEach((fn) => fn());
  });
  let set = watchers.get(el);
  if (set === undefined) {
    set = new Set();
    watchers.set(el, set);
    observer.observe(el);
  } else if (lastWidth.has(el)) {
    recheck();
  }
  set.add(recheck);
  return () => {
    const s = watchers.get(el);
    if (s === undefined) return;
    s.delete(recheck);
    if (s.size > 0) return;
    watchers.delete(el);
    lastWidth.delete(el);
    observer?.unobserve(el);
  };
}
