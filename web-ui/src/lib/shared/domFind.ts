import { textMatches } from "./findText";

const SKIP = "script, style, noscript, button, textarea, input, svg, [hidden], [inert], [aria-hidden=true], .hidden";
const BLOCK = "p, div, li, pre, td, th, h1, h2, h3, h4, h5, h6, summary, blockquote";
const MAX_CHARS = 2_000_000;
const MAX_NODES = 50_000;
export const FIND_LIMIT = 1000;

/** Match across inline markup without rewriting Svelte's or the renderer's DOM. */
export function domMatches(root: HTMLElement, query: string, caseSensitive: boolean): { ranges: Range[]; capped: boolean } {
  if (!query) return { ranges: [], capped: false };
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const groups: Array<{ block: Element | null; nodes: Text[]; text: string }> = [];
  let chars = 0;
  let count = 0;
  let capped = false;
  for (let n = walker.nextNode(); n !== null; n = walker.nextNode()) {
    if (++count > MAX_NODES) { capped = true; break; }
    const node = n as Text;
    const parent = node.parentElement;
    if (!parent || parent.closest(SKIP)) continue;
    let hidden = false;
    for (let details = parent.closest("details:not([open])"); details;
      details = details.parentElement?.closest("details:not([open])") ?? null) {
      // An inner summary is still hidden inside a closed outer detail's body.
      if (!details.querySelector(":scope > summary")?.contains(node)) { hidden = true; break; }
    }
    if (hidden) continue;
    const text = node.data.slice(0, MAX_CHARS - chars);
    chars += text.length;
    const block = parent.closest(BLOCK);
    let group = groups.at(-1);
    if (!group || group.block !== block) { group = { block, nodes: [], text: "" }; groups.push(group); }
    group.nodes.push(node);
    group.text += text;
    if (text.length < node.length) { capped = true; break; }
  }
  const ranges: Range[] = [];
  for (const group of groups) {
    let index = 0;
    let offset = 0;
    for (const [from, to] of textMatches(group.text, query, caseSensitive, FIND_LIMIT + 1 - ranges.length)) {
      const range = document.createRange();
      // Matches are ordered and non-overlapping; walk nodes once, not once per match.
      while (from >= offset + group.nodes[index].length) offset += group.nodes[index++].length;
      range.setStart(group.nodes[index], from - offset);
      while (to > offset + group.nodes[index].length) offset += group.nodes[index++].length;
      range.setEnd(group.nodes[index], to - offset);
      ranges.push(range);
    }
    if (ranges.length > FIND_LIMIT) { ranges.length = FIND_LIMIT; capped = true; break; }
  }
  return { ranges, capped };
}

let nextId = 0;
export function matchPainter() {
  const name = `chimaera-find-${++nextId}`;
  const supported = typeof Highlight !== "undefined" && typeof CSS !== "undefined" && CSS.highlights !== undefined;
  const style = document.createElement("style");
  style.textContent = `::highlight(${name}) { background: color-mix(in srgb, var(--accent) 25%, transparent); }
    ::highlight(${name}-active) { background: var(--accent); color: var(--bg); }`;
  document.head.append(style);
  let fallback: HTMLElement | null = null;
  function clearFallback() { fallback?.classList.remove("find-current-message"); fallback = null; }
  return {
    paint(ranges: Range[], current: number) {
      clearFallback();
      if (supported) {
        CSS.highlights.set(name, new Highlight(...ranges));
        CSS.highlights.set(`${name}-active`, new Highlight(...(ranges[current] ? [ranges[current]] : [])));
      } else if (ranges[current]) {
        fallback = ranges[current].startContainer.parentElement;
        fallback?.classList.add("find-current-message");
      }
    },
    destroy() {
      if (supported) { CSS.highlights.delete(name); CSS.highlights.delete(`${name}-active`); }
      clearFallback();
      style.remove();
    },
  };
}

export function revealMatch(range: Range, root: HTMLElement): void {
  const rect = range.getBoundingClientRect();
  const view = root.getBoundingClientRect();
  root.scrollTop += rect.top - view.top - root.clientHeight / 2;
  if (rect.left < view.left || rect.right > view.right) root.scrollLeft += rect.left - view.left - root.clientWidth / 2;
}
