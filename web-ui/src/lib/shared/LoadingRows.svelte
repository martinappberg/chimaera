<script lang="ts">
  /**
   * A quiet placeholder for content that is on its way: rows without their
   * text, shimmering slowly (a static tint under reduced motion). Only
   * transform and opacity animate, so it costs no layout while it runs.
   */
  let { rows = 6, variant = "tree", label = "Loading" }: {
    rows?: number;
    /** `tree`: indented file rows with a glyph; `transcript`: prose lines in paragraphs. */
    variant?: "tree" | "transcript";
    /** For assistive technology only. */
    label?: string;
  } = $props();

  // Fixed widths and indents read as real content without being random,
  // so the placeholder never jumps between renders.
  const TREE = [[0, 52], [1, 64], [1, 40], [2, 58], [2, 46], [1, 70], [0, 36], [1, 55], [1, 62], [0, 44]];
  const TRANSCRIPT = [92, 86, 64, 0, 96, 90, 72, 0, 40];
  const shown = $derived(Math.max(1, Math.min(rows, 24)));
</script>

<div class="loading-rows {variant}" role="status" aria-busy="true" aria-label={label}>
  {#each { length: shown } as _, i (i)}
    {#if variant === "tree"}
      {@const [depth, width] = TREE[i % TREE.length]}
      <div class="row" style:--depth={depth} aria-hidden="true">
        <span class="bar glyph"></span><span class="bar" style:width="{width}%"></span>
      </div>
    {:else}
      {@const width = TRANSCRIPT[i % TRANSCRIPT.length]}
      <div class="line" class:gap={width === 0} aria-hidden="true">
        {#if width > 0}<span class="bar" style:width="{width}%"></span>{/if}
      </div>
    {/if}
  {/each}
</div>

<style>
  .loading-rows { display: flex; flex-direction: column; pointer-events: none; user-select: none; }
  .tree { padding: 2px 0; }
  .row { display: flex; align-items: center; gap: 6px; height: 22px; padding-left: calc(10px + var(--depth, 0) * 12px); padding-right: 12px; }
  .transcript { gap: 10px; padding: 20px 0; }
  .line { height: 10px; }
  .line.gap { height: 6px; }
  .bar {
    position: relative;
    display: block;
    height: 9px;
    border-radius: 4px;
    overflow: hidden;
    background: color-mix(in srgb, var(--fg) 9%, transparent);
  }
  .line .bar { height: 10px; }
  .bar.glyph { flex: none; width: 12px; height: 12px; border-radius: 3px; }
  /* One slow sweep of a lighter band, the same phase on every bar so the
     column reads as one surface catching light. */
  .bar::after {
    content: "";
    position: absolute;
    inset: 0;
    background: linear-gradient(90deg, transparent, color-mix(in srgb, var(--fg) 8%, transparent), transparent);
    transform: translateX(-100%);
    animation: loading-sweep 1.8s ease-in-out infinite;
  }
  @keyframes loading-sweep { to { transform: translateX(100%); } }
  /* Nobody sees a hidden window: the sweep stops with it (app.css). */
  :global(html.app-hidden) .bar::after { animation-play-state: paused; }
  @media (prefers-reduced-motion: reduce) {
    .bar::after { animation: none; opacity: 0; }
  }
</style>
