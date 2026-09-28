<script lang="ts">
  /**
   * One PDF page in a card (`#page=N`, default 1), drawn by pdf.js at the
   * card's width over ranged `/raw` reads — only that page's bytes cross
   * the tunnel. A `#page=N&xywh=` region (PDF points from the page's
   * top-left) crops to that box. The frame holds a US-letter ratio until
   * the page's own size is known, then settles once; wider panes redraw
   * sharper only when the card has grown by a third.
   */
  import { cropLayout } from "./embed";
  import type { Region } from "./fragment";
  import type { OpenedPdf } from "./pdfEmbed";

  interface Props {
    url: string | null;
    page: number;
    region?: Region;
    compact: boolean;
    active: boolean;
    onOpen: () => void;
  }

  let { url, page, region, compact, active, onOpen }: Props = $props();

  const MAX_H = 560;
  const TILE_H = 200;

  let boxW = $state(0);
  let canvas = $state<HTMLCanvasElement | null>(null);
  /** The page at scale 1 (PDF points). */
  let pageSize = $state<{ w: number; h: number } | null>(null);
  let shown = $state(0);
  let total = $state(0);
  let error = $state<string | null>(null);

  const crop = $derived(pageSize !== null && region !== undefined ? cropLayout(pageSize, region) : null);
  const frame = $derived.by(() => {
    if (pageSize === null) return { w: 612, h: 792 };
    if (crop !== null) return { w: (pageSize.w * 100) / crop.width, h: (pageSize.h * 100) / crop.height };
    return pageSize;
  });
  const maxH = $derived(compact ? TILE_H : MAX_H);
  const frameMax = $derived(Math.round((maxH * frame.w) / frame.h));

  /** The open document, kept per URL so a sharper redraw reuses it (a new
   *  file version is a new URL, and reopens). */
  let opened: { url: string; pdf: OpenedPdf } | null = null;
  let gen = 0;
  /** What the last draw was asked for (not what finished): a redraw is
   *  only worth a new URL, a new page, or a much wider card. */
  let asked: { url: string; page: number; width: number } | null = null;

  async function draw(u: string, p: number, width: number): Promise<void> {
    const mine = ++gen;
    error = null;
    try {
      if (opened?.url !== u) {
        const { openPdf } = await import("./pdfEmbed");
        if (mine !== gen) return;
        opened?.pdf.destroy();
        opened = { url: u, pdf: openPdf(u) };
      }
      const doc = await opened.pdf.doc;
      if (mine !== gen) return;
      total = doc.numPages;
      const n = Math.min(Math.max(1, p), doc.numPages);
      shown = n;
      const pg = await doc.getPage(n);
      if (mine !== gen) return;
      const base = pg.getViewport({ scale: 1 });
      pageSize = { w: base.width, h: base.height };
      const target = canvas;
      if (target === null) return;
      // Render the whole page at the resolution the frame needs: for a
      // crop, the full page is drawn larger and positioned inside it.
      const zoom = crop !== null ? crop.width / 100 : 1;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const scale = Math.max(0.1, (width * zoom * dpr) / base.width);
      const vp = pg.getViewport({ scale });
      target.width = Math.floor(vp.width);
      target.height = Math.floor(vp.height);
      const ctx = target.getContext("2d");
      if (ctx === null) return;
      await pg.render({ canvas: target, canvasContext: ctx, viewport: vp }).promise;
    } catch (e) {
      if (mine !== gen) return;
      asked = null;
      error = e instanceof Error && e.message !== "" ? e.message : "couldn't draw this page";
    }
  }

  // Draw once near, again on a new file version (a new URL) or page, and
  // sharper when the card has grown well past the width it was drawn at.
  $effect(() => {
    const u = url;
    const p = page;
    const w = Math.min(boxW, frameMax);
    if (!active || u === null || w <= 0) return;
    if (asked !== null && asked.url === u && asked.page === p && w <= asked.width * 1.33) return;
    asked = { url: u, page: p, width: w };
    void draw(u, p, w);
  });

  $effect(() => () => {
    gen += 1;
    opened?.pdf.destroy();
    opened = null;
  });
</script>

<div class="pdf-body" class:tile={compact} bind:clientWidth={boxW}>
  <button
    class="frame"
    title="open in a pane"
    style:max-width="{frameMax}px"
    style:aspect-ratio="{frame.w} / {frame.h}"
    onclick={onOpen}
  >
    <canvas
      bind:this={canvas}
      class:cropped={crop !== null}
      class:hidden={error !== null || pageSize === null}
      style:width={crop !== null ? `${crop.width}%` : null}
      style:height={crop !== null ? `${crop.height}%` : null}
      style:left={crop !== null ? `${crop.left}%` : null}
      style:top={crop !== null ? `${crop.top}%` : null}
    ></canvas>
    {#if error !== null}
      <span class="note">{error}</span>
    {:else if pageSize === null}
      <span class="note">{active ? "loading page…" : ""}</span>
    {/if}
  </button>
  {#if total > 1}
    <span class="pages">{shown} / {total}</span>
  {/if}
</div>

<style>
  .pdf-body {
    position: relative;
    padding: 8px;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .pdf-body.tile {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 0;
  }
  .frame {
    position: relative;
    display: block;
    width: 100%;
    max-height: 100%;
    margin: 0 auto;
    padding: 0;
    border: none;
    border-radius: 2px;
    overflow: hidden;
    /* Paper: PDFs assume a white page in either theme. */
    background: #ffffff;
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--edge) 80%, transparent);
    cursor: zoom-in;
  }
  canvas {
    display: block;
    width: 100%;
    height: 100%;
  }
  canvas.cropped {
    position: absolute;
  }
  canvas.hidden {
    visibility: hidden;
  }
  .note {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 8px;
    color: #6b6b6b;
    font-size: var(--text-xs);
  }
  .pages {
    position: absolute;
    right: 14px;
    bottom: 14px;
    padding: 1px 6px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--bg) 85%, transparent);
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
</style>
