<script lang="ts">
  /**
   * An HTML report in a card: the daemon-served page in a frame sandboxed
   * like the full viewer (`allow-scripts`, never same-origin; the response
   * carries the same CSP), addressed through its ticket's folder so its
   * relative scripts, styles and figures load. A fixed height with an
   * expand toggle; the frame is created only once the card is near.
   */
  interface Props {
    url: string | null;
    title: string;
    compact: boolean;
    active: boolean;
  }

  let { url, title, compact, active }: Props = $props();

  let expanded = $state(false);
  const height = $derived(compact ? 200 : expanded ? 720 : 360);
</script>

<div class="html-body" class:tile={compact} style:height="{height}px">
  {#if active && url !== null}
    <iframe src={url} {title} sandbox="allow-scripts" referrerpolicy="no-referrer"></iframe>
  {/if}
  {#if !compact}
    <button class="expand" onclick={() => (expanded = !expanded)} title={expanded ? "collapse" : "show more of the page"}>
      {expanded ? "collapse" : "expand"}
    </button>
  {/if}
</div>

<style>
  .html-body {
    position: relative;
    /* Pages assume a white canvas regardless of theme. */
    background: #ffffff;
    transition: height 0.18s ease;
  }
  .html-body.tile {
    flex: 1;
    height: auto !important;
    min-height: 200px;
  }
  iframe {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    border: none;
  }
  .expand {
    position: absolute;
    right: 8px;
    bottom: 8px;
    padding: 2px 8px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    background: color-mix(in srgb, var(--bg) 92%, transparent);
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    opacity: 0.85;
  }
  .expand:hover {
    color: var(--accent);
    border-color: var(--accent);
    opacity: 1;
  }
  @media (prefers-reduced-motion: reduce) {
    .html-body {
      transition: none;
    }
  }
</style>
