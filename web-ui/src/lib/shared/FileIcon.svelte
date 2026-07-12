<script lang="ts">
  /**
   * The one file-type glyph, used everywhere a file appears (tree, tabs, pane
   * bars, quick-open). Vendored Tabler paths (see icons.ts) re-drawn at 14–16px
   * with a single currentColor stroke and a quiet per-category tint, so the set
   * reads as one family with the hand-made session glyphs. Unknown types fall
   * back to a generic-file outline.
   */
  import { iconFor } from "../previews/files";
  import type { Glyph } from "./icons";

  interface Props {
    path: string;
    /** Pixel size (14–16 in practice). */
    size?: number;
    /** Override the tint (e.g. inherit the active-tab color). */
    plain?: boolean;
    /** Draw the symlink alias-arrow badge (an entry that is a symlink). */
    link?: boolean;
    /** A broken (dangling) symlink: badge + glyph tinted with the error color. */
    broken?: boolean;
  }

  let { path, size = 15, plain = false, link = false, broken = false }: Props = $props();

  const glyph = $derived<Glyph | null>(iconFor(path));
  const cat = $derived(glyph?.c ?? "generic");
</script>

<svg
  class="ficon cat-{cat}"
  class:plain
  class:broken
  viewBox="0 0 24 24"
  width={size}
  height={size}
  fill="none"
  stroke="currentColor"
  stroke-width="1.7"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#if glyph !== null}
    {#each glyph.d as d (d)}
      <path {d} />
    {/each}
  {:else}
    <!-- generic file: a quiet folded-corner sheet -->
    <path d="M14 3v4a1 1 0 0 0 1 1h4" />
    <path d="M17 21h-10a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2h7l5 5v11a2 2 0 0 1 -2 2z" />
  {/if}
  {#if link}
    <!-- Symlink alias arrow, bottom-left: a knockout disc (so the glyph
         behind doesn't muddle it) + a tiny up-right arrow. -->
    <g class="link-badge">
      <circle cx="6" cy="18" r="5" class="link-knockout" stroke="none" />
      <path d="M4 20l4 -4M8 16h-2.6M8 16v2.6" stroke-width="1.6" />
    </g>
  {/if}
</svg>

<style>
  .ficon {
    flex: none;
    color: var(--ficon-generic);
  }

  .ficon.cat-lang {
    color: var(--ficon-lang);
  }
  .ficon.cat-data {
    color: var(--ficon-data);
  }
  .ficon.cat-doc {
    color: var(--ficon-doc);
  }
  .ficon.cat-media {
    color: var(--ficon-media);
  }
  .ficon.cat-archive {
    color: var(--ficon-archive);
  }
  .ficon.cat-config {
    color: var(--ficon-config);
  }
  .ficon.cat-bio {
    color: var(--ficon-bio);
  }
  .ficon.cat-vcs {
    color: var(--ficon-vcs);
  }
  .ficon.cat-generic {
    color: var(--ficon-generic);
  }

  /* Inherit the surrounding text color (active tab emphasis, hover). */
  .ficon.plain {
    color: currentColor;
    opacity: 0.85;
  }

  /* The symlink badge knockout uses the surface behind the glyph so the arrow
     reads cleanly over the file outline. */
  .link-knockout {
    fill: var(--pane-bg, var(--bg));
  }
  .link-badge {
    color: var(--muted);
  }
  /* A broken symlink: whole glyph + badge in the error color. */
  .ficon.broken {
    color: var(--err);
  }
  .ficon.broken .link-badge {
    color: var(--err);
  }
</style>
