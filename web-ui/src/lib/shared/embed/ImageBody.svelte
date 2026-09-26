<script lang="ts">
  /**
   * An image in a card: its box reserved from the header dimensions the
   * resolve answer carried (width + aspect-ratio, capped to a height), a
   * `#xywh=` region drawn cropped by positioning the whole image inside a
   * clipped frame, a click opens the full viewer. The `src` is set only once
   * the card is near the viewport.
   */
  import { cropLayout, frameWidth } from "./embed";
  import type { Region } from "./fragment";

  interface Props {
    url: string | null;
    /** Pixel size from the header, when known. */
    natural: { w: number; h: number } | null;
    region?: Region;
    alt: string;
    hint: number | null;
    compact: boolean;
    active: boolean;
    onOpen: () => void;
  }

  let { url, natural, region, alt, hint, compact, active, onOpen }: Props = $props();

  /** Tallest a picture is drawn inline / in a gallery tile. */
  const MAX_H = 420;
  const TILE_H = 200;

  /** Measured on load when the header did not say (AVIF, ICO…). */
  let measured = $state<{ w: number; h: number } | null>(null);
  let failed = $state(false);
  let loaded = $state(false);
  $effect(() => {
    void url;
    failed = false;
    loaded = false;
  });

  const size = $derived(natural ?? measured);
  const crop = $derived(size !== null && region !== undefined ? cropLayout(size, region) : null);
  /** The frame's own natural size: the region's, or the picture's. */
  const frame = $derived.by(() => {
    if (crop !== null && size !== null) return { w: (size.w * 100) / crop.width, h: (size.h * 100) / crop.height };
    return size;
  });
  const maxH = $derived(compact ? TILE_H : MAX_H);
  const boxWidth = $derived(frame !== null ? frameWidth(frame, maxH, hint) : null);
  const aspect = $derived(frame !== null ? `${frame.w} / ${frame.h}` : "4 / 3");

  function onLoad(e: Event): void {
    const img = e.currentTarget as HTMLImageElement;
    loaded = true;
    if (natural === null && img.naturalWidth > 0) measured = { w: img.naturalWidth, h: img.naturalHeight };
  }
</script>

<div class="image-body" class:tile={compact}>
  <button
    class="frame"
    class:loaded
    title="open in a pane"
    style:width={boxWidth !== null ? `${boxWidth}px` : compact ? "100%" : `${Math.round((maxH * 4) / 3)}px`}
    style:aspect-ratio={aspect}
    onclick={onOpen}
  >
    {#if failed}
      <span class="note">couldn't load this image</span>
    {:else if active && url !== null}
      <img
        class:cropped={crop !== null}
        src={url}
        {alt}
        decoding="async"
        draggable="false"
        style:width={crop !== null ? `${crop.width}%` : null}
        style:height={crop !== null ? `${crop.height}%` : null}
        style:left={crop !== null ? `${crop.left}%` : null}
        style:top={crop !== null ? `${crop.top}%` : null}
        onload={onLoad}
        onerror={() => (failed = true)}
      />
    {/if}
  </button>
</div>

<style>
  .image-body {
    padding: 8px;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }
  .image-body.tile {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 0;
  }
  /* An explicit width (the reserved box, set inline) capped to the card:
     aspect-ratio then fixes the height before a byte loads, and the card
     around a lone picture can shrink to it. */
  .frame {
    position: relative;
    display: block;
    max-width: 100%;
    max-height: 100%;
    padding: 0;
    border: none;
    border-radius: 4px;
    overflow: hidden;
    background: color-mix(in srgb, var(--fg) 5%, transparent);
    cursor: zoom-in;
  }
  .tile .frame {
    margin: 0 auto;
  }
  .frame.loaded {
    background: transparent;
  }
  img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  img.cropped {
    position: absolute;
    max-width: none;
    object-fit: fill;
  }
  .note {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 8px;
    color: var(--muted);
    font-size: var(--text-xs);
  }
</style>
