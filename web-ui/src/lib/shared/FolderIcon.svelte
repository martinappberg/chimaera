<script lang="ts">
  /**
   * The one folder glyph, used wherever a directory appears (the FILES tree,
   * the Finder). Vendored Tabler paths re-drawn at the same 24-grid, single
   * `currentColor` stroke as FileIcon, so folders read as one family with the
   * file-type glyphs. An open variant (drawn while the folder is expanded)
   * gives the tree its disclosure feel.
   */
  interface Props {
    /** Draw the open-folder variant (expanded dir). */
    open?: boolean;
    /** Pixel size (13–16 in practice). */
    size?: number;
    /** Inherit the surrounding text color (active tab / hover) instead of the
     *  folder tint. */
    plain?: boolean;
    /** Draw the symlink alias-arrow badge (a symlinked directory). */
    link?: boolean;
  }

  let { open = false, size = 15, plain = false, link = false }: Props = $props();
</script>

<svg
  class="folder"
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
  {#if open}
    <path
      d="M5 19l2.757 -7.351a1 1 0 0 1 .936 -.649h12.307a1 1 0 0 1 .986 1.164l-.996 5.211a2 2 0 0 1 -1.964 1.625h-14.03a2 2 0 0 1 -2 -2v-11a2 2 0 0 1 2 -2h4l3 3h7a2 2 0 0 1 2 2v2"
    />
  {:else}
    <path
      d="M5 4h4l3 3h7a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-11a2 2 0 0 1 2 -2"
    />
  {/if}
  {#if link}
    <!-- Symlink alias arrow, bottom-left: knockout disc + tiny up-right arrow. -->
    <g class="link-badge">
      <circle cx="6" cy="18" r="5" class="link-knockout" stroke="none" />
      <path d="M4 20l4 -4M8 16h-2.6M8 16v2.6" stroke-width="1.6" />
    </g>
  {/if}
</svg>

<style>
  /* A calm folder tint — a desaturated accent so folders sit a step above
     file glyphs without shouting. Themes may override via --ficon-folder. */
  .folder {
    flex: none;
    color: var(--ficon-folder, color-mix(in srgb, var(--accent) 60%, var(--muted) 40%));
  }

  .folder.plain {
    color: currentColor;
    opacity: 0.85;
  }

  .link-knockout {
    fill: var(--pane-bg, var(--bg));
  }
  .link-badge {
    color: var(--muted);
  }
</style>
