/**
 * LaTeX → DOM-safe MathML: the ONE policy every surface renders equations
 * under — chat prose and user bubbles (`chat/math.ts` builds its tokenizers
 * on it) and the markdown file previews (live widgets + the reading view).
 * KaTeX runs with trust off and MathML output (no inline geometry styles for
 * the sanitizer to strip; screen-reader friendly), then the result still
 * passes DOMPurify with `<style>`/`style=` forbidden — every fragment that
 * reaches innerHTML does, regardless of what produced it.
 *
 * A leaf on purpose (katex + dompurify only): the previews load it on demand
 * so a document without an equation never pays for KaTeX, and vite.config
 * pins this module and its two libraries to their own chunk — otherwise
 * Rollup would fold them into the chat bundle (their static importer) and
 * that on-demand load would drag the whole chat surface in.
 */
import DOMPurify from "dompurify";
import katex from "katex";

export const mathOptions = {
  throwOnError: false,
  strict: "ignore" as const,
  trust: false,
  output: "mathml" as const,
  maxSize: 20,
  maxExpand: 1000,
};

export function renderMath(source: string, displayMode: boolean): string {
  return katex.renderToString(source, { ...mathOptions, displayMode });
}

// Rendered equations are re-requested constantly — a live-mode widget every
// time its line re-enters the viewport, the reading view on every server
// refresh (an agent rewriting the file ticks at ~4 Hz) — so the sanitized
// markup is memoized. Bounded: insertion order is eviction order.
const CACHE_MAX = 512;
const cache = new Map<string, string>();

/** One equation as sanitized KaTeX MathML, memoized. */
export function safeMathHtml(source: string, displayMode: boolean): string {
  const key = (displayMode ? "D" : "I") + source;
  const hit = cache.get(key);
  if (hit !== undefined) return hit;
  const html = DOMPurify.sanitize(renderMath(source, displayMode), {
    FORBID_TAGS: ["style"],
    FORBID_ATTR: ["style"],
  });
  if (cache.size >= CACHE_MAX) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  cache.set(key, html);
  return html;
}
