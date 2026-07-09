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
  }

  let { path, size = 15, plain = false }: Props = $props();

  const glyph = $derived<Glyph | null>(iconFor(path));
  const cat = $derived(glyph?.c ?? "generic");
</script>

<svg
  class="ficon cat-{cat}"
  class:plain
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
</style>
