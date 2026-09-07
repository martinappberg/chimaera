import { Renderer, type MarkedExtension, type Tokens } from "marked";

/**
 * Marked renderer override that hosts every GFM table in a scroll container.
 *
 * A `<table>` can't scroll its own content, and the chat's `{@html}` hosts are
 * append-only after render (a post-render DOM wrap would strand Svelte's
 * teardown walk — see shared/copyDecor.ts), so the host is emitted in the HTML
 * string itself, ahead of sanitization; DOMPurify keeps `div` and `class`.
 * The default table markup is reused verbatim (`Renderer.prototype.table`
 * still dispatches `tablerow`/`tablecell` through `this`, so a later cell
 * override composes), which keeps cell rendering and the streaming ⇄ settled
 * parity untouched. The prototype call also steps OUTSIDE marked's fall-through
 * chain: a `table` renderer override registered before this one is skipped
 * and one registered after it replaces the host outright — keep `table`
 * overridden here only (cell/row overrides compose fine). Agent HTML can forge
 * the class; that buys scroll styling around whatever it wraps and nothing more.
 */
export const markdownTables: MarkedExtension = {
  renderer: {
    table(token: Tokens.Table) {
      return `<div class="md-table">${Renderer.prototype.table.call(this, token)}</div>\n`;
    },
  },
};
