<script lang="ts">
  /**
   * PDF preview via pdf.js (worker bundled locally — no CDN, air-gapped rule).
   * Pages render lazily as they scroll into view; fit-width by default with
   * fit/100%/±  zoom controls and a page count in the bar. Bytes come through
   * the ticketed /raw/ URL (range requests supported server-side), so the
   * bearer token never lands in a fetchable URL.
   */
  import { onMount } from "svelte";
  import * as pdfjs from "pdfjs-dist";
  import type { PDFDocumentProxy, PDFPageProxy } from "pdfjs-dist";
  // Vite bundles the worker as a local asset; nothing is fetched from a CDN.
  import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
  import { fsRawUrl } from "./files";

  pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  interface PageInfo {
    num: number;
    /** Natural size at scale 1 (PDF points). */
    w: number;
    h: number;
  }

  let scroller = $state<HTMLDivElement | null>(null);
  let containerWidth = $state(0);
  let pages = $state<PageInfo[]>([]);
  let numPages = $state(0);
  let error = $state<string | null>(null);
  let loading = $state(true);
  /** "fit" (fit width) or an explicit CSS scale factor. */
  let zoom = $state<"fit" | number>("fit");

  let doc: PDFDocumentProxy | null = null;
  let task: ReturnType<typeof pdfjs.getDocument> | null = null;
  const rendered = new Map<number, HTMLCanvasElement>();
  const renderingPages = new Set<number>();
  let observer: IntersectionObserver | null = null;
  const dpr = typeof window !== "undefined" ? Math.min(window.devicePixelRatio || 1, 2) : 1;

  // Effective CSS scale: fit-width divides the container by the widest page,
  // clamped so a tiny pane doesn't render illegibly small.
  const fitScale = $derived.by(() => {
    if (pages.length === 0 || containerWidth === 0) return 1;
    const widest = Math.max(...pages.map((p) => p.w));
    // 32px accounts for page horizontal margins in the column.
    return Math.max((containerWidth - 32) / widest, 0.1);
  });
  const scale = $derived(zoom === "fit" ? fitScale : zoom);

  onMount(() => {
    let cancelled = false;
    void (async () => {
      try {
        const url = await fsRawUrl(path);
        if (cancelled) return;
        task = pdfjs.getDocument({ url });
        const d = await task.promise;
        if (cancelled) {
          void task.destroy();
          return;
        }
        doc = d;
        numPages = d.numPages;
        const infos: PageInfo[] = [];
        for (let n = 1; n <= d.numPages; n++) {
          const page = await d.getPage(n);
          if (cancelled) return;
          const vp = page.getViewport({ scale: 1 });
          infos.push({ num: n, w: vp.width, h: vp.height });
          page.cleanup();
        }
        pages = infos;
        loading = false;
      } catch (e) {
        if (!cancelled) {
          error = e instanceof Error ? e.message : "failed to open pdf";
          loading = false;
        }
      }
    })();

    return () => {
      cancelled = true;
      observer?.disconnect();
      rendered.clear();
      renderingPages.clear();
      doc = null;
      const tk = task;
      task = null;
      if (tk !== null) void tk.destroy();
    };
  });

  // Track container width for fit-scaling.
  $effect(() => {
    const el = scroller;
    if (el === null) return;
    const ro = new ResizeObserver((entries) => {
      for (const e of entries) containerWidth = e.contentRect.width;
    });
    ro.observe(el);
    containerWidth = el.clientWidth;
    return () => ro.disconnect();
  });

  // Lazy-render observer: render a page's canvas as its slot nears the viewport.
  $effect(() => {
    const el = scroller;
    if (el === null || pages.length === 0) return;
    const io = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          const n = Number((entry.target as HTMLElement).dataset.page);
          if (Number.isFinite(n)) void renderPage(n, entry.target as HTMLElement);
        }
      },
      { root: el, rootMargin: "600px 0px" },
    );
    observer = io;
    for (const slot of el.querySelectorAll<HTMLElement>("[data-page]")) io.observe(slot);
    return () => {
      io.disconnect();
      observer = null;
    };
  });

  // Re-render already-shown pages when the scale changes (crisp at any zoom).
  $effect(() => {
    void scale;
    for (const [n] of rendered) {
      const slot = scroller?.querySelector<HTMLElement>(`[data-page="${n}"]`);
      if (slot !== null && slot !== undefined) void renderPage(n, slot, true);
    }
  });

  async function renderPage(n: number, slot: HTMLElement, force = false): Promise<void> {
    const d = doc;
    if (d === null) return;
    if (!force && (rendered.has(n) || renderingPages.has(n))) return;
    renderingPages.add(n);
    try {
      const page: PDFPageProxy = await d.getPage(n);
      const s = zoom === "fit" ? fitScale : zoom;
      const viewport = page.getViewport({ scale: s * dpr });
      let canvas = rendered.get(n) ?? null;
      if (canvas === null) {
        canvas = document.createElement("canvas");
        canvas.className = "pdf-canvas";
        slot.querySelector(".pdf-canvas")?.remove();
        slot.appendChild(canvas);
        rendered.set(n, canvas);
      }
      const ctx = canvas.getContext("2d");
      if (ctx === null) return;
      canvas.width = Math.floor(viewport.width);
      canvas.height = Math.floor(viewport.height);
      canvas.style.width = `${viewport.width / dpr}px`;
      canvas.style.height = `${viewport.height / dpr}px`;
      await page.render({ canvas, canvasContext: ctx, viewport }).promise;
      page.cleanup();
    } catch {
      // a page failed to render; leave its placeholder in place
    } finally {
      renderingPages.delete(n);
    }
  }

  function zoomIn(): void {
    const cur = zoom === "fit" ? fitScale : zoom;
    zoom = Math.min(cur + 0.25, 4);
  }
  function zoomOut(): void {
    const cur = zoom === "fit" ? fitScale : zoom;
    zoom = Math.max(cur - 0.25, 0.25);
  }
  const zoomPct = $derived(Math.round(scale * 100));
</script>

<div class="pdf-view">
  <div class="pdf-bar">
    <span class="pages" class:dim={numPages === 0}>
      {#if numPages > 0}{numPages} page{numPages === 1 ? "" : "s"}{:else}—{/if}
    </span>
    <span class="spacer"></span>
    <div class="zoom">
      <button class="zbtn" class:on={zoom === "fit"} onclick={() => (zoom = "fit")} title="fit width">fit</button>
      <button class="zbtn" class:on={zoom === 1} onclick={() => (zoom = 1)} title="actual size">100%</button>
      <button class="zbtn ic" onclick={zoomOut} aria-label="zoom out" title="zoom out">−</button>
      <span class="zpct">{zoomPct}%</span>
      <button class="zbtn ic" onclick={zoomIn} aria-label="zoom in" title="zoom in">+</button>
    </div>
  </div>

  <div class="pdf-scroll" bind:this={scroller}>
    {#if error !== null}
      <div class="file-error">{error}</div>
    {:else if loading}
      <div class="file-loading">opening…</div>
    {:else}
      {#each pages as p (p.num)}
        <div
          class="pdf-slot"
          data-page={p.num}
          style:width={`${p.w * scale}px`}
          style:height={`${p.h * scale}px`}
        ></div>
      {/each}
    {/if}
  </div>
</div>

<style>
  .pdf-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .pdf-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 26px;
    padding: 0 0.7rem;
    border-bottom: 1px solid var(--edge);
    font-size: 0.68rem;
    color: var(--muted);
  }

  .pages {
    font-variant-numeric: tabular-nums;
  }

  .pages.dim {
    opacity: 0.6;
  }

  .spacer {
    flex: 1;
  }

  .zoom {
    display: flex;
    align-items: center;
    gap: 1px;
  }

  .zbtn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: 0.68rem;
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .zbtn:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .zbtn.on {
    color: var(--fg);
    background: var(--row-active);
  }

  .zbtn.ic {
    min-width: 20px;
    text-align: center;
    font-size: 0.85rem;
    line-height: 1;
  }

  .zpct {
    min-width: 3.2ch;
    text-align: center;
    font-variant-numeric: tabular-nums;
  }

  .pdf-scroll {
    flex: 1;
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    padding: 14px 0;
    background: color-mix(in srgb, var(--fg) 4%, var(--term-bg));
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }

  .pdf-slot {
    flex: none;
    background: #fff;
    box-shadow: 0 1px 6px rgba(0, 0, 0, 0.18);
    border-radius: 2px;
    overflow: hidden;
  }

  .pdf-slot :global(.pdf-canvas) {
    display: block;
  }

  .file-error,
  .file-loading {
    margin: auto;
    color: var(--muted);
    font-size: 0.8rem;
    padding: 1rem;
    text-align: center;
  }
</style>
