<script lang="ts">
  /**
   * One line of a list: a title (inline markdown, sanitized), an optional
   * subtitle clamped to two lines, badges, and room at either end (an id,
   * a priority mark; a date). `meta` renders under the title for anything
   * richer than badges. A button when it opens something.
   */
  import type { Snippet } from "svelte";
  import { inlineMarkdown } from "../inlineMarkdown";
  import Badge from "./Badge.svelte";
  import type { BadgeSpec } from "./tone";

  interface Props {
    title: string;
    subtitle?: string;
    badges?: BadgeSpec[];
    selected?: boolean;
    /** Drawn muted and struck through (a superseded entry). */
    retired?: boolean;
    onclick?: () => void;
    leading?: Snippet;
    trailing?: Snippet;
    meta?: Snippet;
    /** Extra attributes for the row element (e.g. `data-ekey`). */
    attrs?: Record<string, string>;
    role?: string;
  }

  let { title, subtitle, badges = [], selected = false, retired = false, onclick, leading, trailing, meta, attrs = {}, role }: Props =
    $props();
</script>

<svelte:element
  this={onclick ? "button" : "div"}
  class="row"
  class:sel={selected}
  class:retired
  class:lead={leading !== undefined}
  {onclick}
  {role}
  aria-selected={role === "option" ? selected : undefined}
  {...attrs}
>
  {#if leading}<span class="leading">{@render leading()}</span>{/if}
  <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
  <span class="title">{@html inlineMarkdown(title)}</span>
  {#if trailing}<span class="trailing">{@render trailing()}</span>{/if}
  {#if subtitle}
    <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
    <span class="subtitle">{@html inlineMarkdown(subtitle)}</span>
  {/if}
  {#if badges.length > 0 || meta}
    <span class="meta">
      {#each badges as b, i (i)}<Badge text={b.text} tone={b.tone} />{/each}
      {@render meta?.()}
    </span>
  {/if}
</svelte:element>

<style>
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 2px 10px;
    align-items: baseline;
    width: 100%;
    padding: 7px 14px 7px 12px;
    border: 0;
    border-left: 2px solid transparent;
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
    /* Long lists: the browser skips laying out what's off screen. */
    content-visibility: auto;
    contain-intrinsic-size: auto 46px;
  }
  .row.lead {
    grid-template-columns: var(--row-lead, 58px) minmax(0, 1fr) auto;
  }
  button.row {
    cursor: pointer;
  }
  button.row:hover {
    background: var(--row-hover);
  }
  .row.sel {
    background: var(--row-active);
    border-left-color: var(--accent);
  }
  .row:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: -2px;
  }
  .leading {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: 11.5px;
  }
  .title {
    font-size: var(--text-sm);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .title :global(code),
  .subtitle :global(code) {
    font-family: var(--mono);
    font-size: 0.9em;
  }
  .retired .title {
    color: var(--muted);
    text-decoration: line-through;
    text-decoration-color: color-mix(in srgb, var(--muted) 60%, transparent);
  }
  .trailing {
    font-family: var(--mono);
    font-size: 10.5px;
    color: var(--muted);
    white-space: nowrap;
  }
  .subtitle,
  .meta {
    grid-column: 1 / -1;
  }
  .lead .subtitle,
  .lead .meta {
    grid-column: 2 / -1;
  }
  .subtitle {
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.45;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    overflow-wrap: anywhere;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 3px 8px;
    font-size: 11.5px;
    color: var(--muted);
    min-width: 0;
  }
</style>
