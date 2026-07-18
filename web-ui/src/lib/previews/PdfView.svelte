<script module lang="ts">
  /** Per-tab scroll + zoom memory, keyed by path. Module-scoped so it
   * survives the component unmount/remount that a tab switch triggers. */
  interface PdfMem {
    zoom: "fit" | number;
    scrollTop: number;
    scrollLeft: number;
  }
  const memory = new Map<string, PdfMem>();
  const MEMORY_CAP = 100;
</script>

<script lang="ts">
  /**
   * PDF preview via pdf.js (worker bundled locally — no CDN, air-gapped rule).
   * Pages render lazily as they scroll into view; fit-width by default with
   * fit/100%/± zoom controls, ctrl/⌘-wheel zoom anchored at the cursor, a
   * selectable text layer over each page, and per-tab scroll+zoom memory.
   * Bytes come through the ticketed /raw/ URL (range requests supported
   * server-side), so the bearer token never lands in a fetchable URL.
   */
  import { onMount } from "svelte";
  import * as pdfjs from "pdfjs-dist";
  import type { PDFDocumentProxy, PDFPageProxy } from "pdfjs-dist";
  // Vite bundles the worker as a local asset; nothing is fetched from a CDN.
  import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
  import { retain, release } from "./fileStore.svelte";

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
  let restored = false;

  let doc: PDFDocumentProxy | null = null;
  let task: ReturnType<typeof pdfjs.getDocument> | null = null;
  const rendered = new Map<number, HTMLCanvasElement>();
  const renderingPages = new Set<number>();
  const renderedScale = new Map<number, number>();
  let observer: IntersectionObserver | null = null;
  /** Pages inside the observer margin; never evict a canvas the user is at. */
  const nearbyPages = new Set<number>();
  const dpr = typeof window !== "undefined" ? Math.min(window.devicePixelRatio || 1, 2) : 1;
  /** Bound decoded raster memory while retaining a generous high-DPI page. */
  const MAX_CANVAS_PIXELS = 12_000_000;
  /** Canvases outside the viewport margin are an LRU, not a document-long leak. */
  const MAX_RENDERED_PAGES = 8;
  const PAGE_INFO_BATCH = 24;

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
    const mem = memory.get(path);
    if (mem !== undefined) zoom = mem.zoom;
    // Pin + share the ticketed /raw/ URL through the store (cached across a tab
    // switch — no re-mint). pdf.js still re-parses on remount; the URL cache and
    // the per-tab scroll/zoom memory are what make the return feel instant.
    const fileEntry = retain(path);
    void (async () => {
      try {
        await fileEntry.ensureRawUrl();
        if (cancelled) return;
        const url = fileEntry.rawUrl;
        if (url === null) throw new Error(fileEntry.rawError ?? "failed to open pdf");
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
          // A large PDF should paint its first pages without waiting for a
          // serial metadata walk across the whole document. Batch updates
          // avoid O(n²) array churn while letting the observer start early.
          if (n === 1 || n % PAGE_INFO_BATCH === 0 || n === d.numPages) {
            pages = [...infos];
            loading = false;
          }
        }
      } catch (e) {
        if (!cancelled) {
          error = e instanceof Error ? e.message : "failed to open pdf";
          loading = false;
        }
      }
    })();

    return () => {
      cancelled = true;
      release(path);
      saveMemory();
      observer?.disconnect();
      rendered.clear();
      renderingPages.clear();
      renderedScale.clear();
      nearbyPages.clear();
      doc = null;
      const tk = task;
      task = null;
      if (tk !== null) void tk.destroy();
    };
  });

  function saveMemory(): void {
    const el = scroller;
    if (el === null) return;
    memory.delete(path);
    memory.set(path, { zoom, scrollTop: el.scrollTop, scrollLeft: el.scrollLeft });
    while (memory.size > MEMORY_CAP) {
      const oldest = memory.keys().next().value;
      if (oldest === undefined) break;
      memory.delete(oldest);
    }
  }

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

  // Restore remembered scroll once pages have laid out.
  $effect(() => {
    if (
      restored ||
      pages.length === 0 ||
      pages.length !== numPages ||
      scroller === null ||
      containerWidth === 0
    )
      return;
    const mem = memory.get(path);
    restored = true;
    if (mem !== undefined) {
      // wait a frame so slot heights exist
      requestAnimationFrame(() => {
        if (scroller !== null) {
          scroller.scrollTop = mem.scrollTop;
          scroller.scrollLeft = mem.scrollLeft;
        }
      });
    }
  });

  // Lazy-render observer: render a page's canvas as its slot nears the viewport.
  $effect(() => {
    const el = scroller;
    if (el === null || pages.length === 0) return;
    const io = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const n = Number((entry.target as HTMLElement).dataset.page);
          if (!Number.isFinite(n)) continue;
          if (!entry.isIntersecting) {
            nearbyPages.delete(n);
            continue;
          }
          nearbyPages.add(n);
          void renderPage(n, entry.target as HTMLElement);
        }
      },
      { root: el, rootMargin: "600px 0px" },
    );
    observer = io;
    for (const slot of el.querySelectorAll<HTMLElement>("[data-page]")) io.observe(slot);
    return () => {
      io.disconnect();
      nearbyPages.clear();
      observer = null;
    };
  });

  // On scale change: immediately stretch existing canvases (smooth), then
  // re-rasterize crisply after a short settle so a zoom gesture stays fluid.
  let reRenderTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const s = scale;
    // Cheap immediate stretch of what's already drawn.
    for (const [n, canvas] of rendered) {
      const info = pages.find((p) => p.num === n);
      if (info === undefined) continue;
      canvas.style.width = `${info.w * s}px`;
      canvas.style.height = `${info.h * s}px`;
    }
    if (reRenderTimer !== null) clearTimeout(reRenderTimer);
    reRenderTimer = setTimeout(() => {
      reRenderTimer = null;
      for (const [n] of rendered) {
        const slot = scroller?.querySelector<HTMLElement>(`[data-page="${n}"]`);
        if (slot !== null && slot !== undefined) void renderPage(n, slot);
      }
    }, 140);
  });

  async function renderPage(n: number, slot: HTMLElement): Promise<void> {
    const d = doc;
    if (d === null) return;
    const s = zoom === "fit" ? fitScale : zoom;
    if (renderingPages.has(n) || renderedScale.get(n) === s) return;
    renderingPages.add(n);
    let page: PDFPageProxy | null = null;
    let renderedAtScale = false;
    try {
      page = await d.getPage(n);
      const cssViewport = page.getViewport({ scale: s });
      const desired = page.getViewport({ scale: s * dpr });
      const desiredPixels = desired.width * desired.height;
      if (!Number.isFinite(desiredPixels) || desiredPixels <= 0) {
        throw new Error("invalid PDF page dimensions");
      }
      const rasterFactor = Math.min(1, Math.sqrt(MAX_CANVAS_PIXELS / desiredPixels));
      const viewport = page.getViewport({ scale: s * dpr * rasterFactor });
      let canvas = rendered.get(n) ?? null;
      if (canvas === null) {
        canvas = document.createElement("canvas");
        canvas.className = "pdf-canvas";
        slot.querySelector(".pdf-canvas")?.remove();
        slot.insertBefore(canvas, slot.firstChild);
        rendered.set(n, canvas);
      }
      const ctx = canvas.getContext("2d");
      if (ctx === null) return;
      canvas.width = Math.max(1, Math.floor(viewport.width));
      canvas.height = Math.max(1, Math.floor(viewport.height));
      canvas.style.width = `${cssViewport.width}px`;
      canvas.style.height = `${cssViewport.height}px`;
      await page.render({ canvas, canvasContext: ctx, viewport }).promise;
      renderedScale.set(n, s);
      renderedAtScale = true;
      rememberRendered(n, canvas);
      // Selectable text layer, positioned by --total-scale-factor.
      await renderTextLayer(page, slot, s);
    } catch {
      // a page failed to render; leave its placeholder in place
    } finally {
      page?.cleanup();
      renderingPages.delete(n);
      // A zoom/fit change may land while this page is rasterizing. Never
      // render into the same canvas concurrently; finish once, then catch up.
      const currentScale = zoom === "fit" ? fitScale : zoom;
      if (renderedAtScale && currentScale !== s && slot.isConnected) {
        void renderPage(n, slot);
      }
    }
  }

  /** Touch one rendered page and evict inactive LRU canvases/text layers. */
  function rememberRendered(n: number, canvas: HTMLCanvasElement): void {
    rendered.delete(n);
    rendered.set(n, canvas);
    if (rendered.size <= MAX_RENDERED_PAGES) return;
    for (const [old, oldCanvas] of rendered) {
      if (rendered.size <= MAX_RENDERED_PAGES) break;
      if (old === n || nearbyPages.has(old) || renderingPages.has(old)) continue;
      oldCanvas.remove();
      scroller?.querySelector<HTMLElement>(`[data-page="${old}"] .textLayer`)?.remove();
      rendered.delete(old);
      renderedScale.delete(old);
    }
  }

  async function renderTextLayer(page: PDFPageProxy, slot: HTMLElement, s: number): Promise<void> {
    try {
      let layer = slot.querySelector<HTMLDivElement>(".textLayer");
      if (layer === null) {
        layer = document.createElement("div");
        layer.className = "textLayer";
        slot.appendChild(layer);
      }
      layer.replaceChildren();
      layer.style.setProperty("--total-scale-factor", String(s));
      layer.style.setProperty("--scale-round-x", "1px");
      layer.style.setProperty("--scale-round-y", "1px");
      const viewport = page.getViewport({ scale: s });
      const source = page.streamTextContent({ includeMarkedContent: true, disableNormalization: true });
      const tl = new pdfjs.TextLayer({ textContentSource: source, container: layer, viewport });
      await tl.render();
    } catch {
      // text layer is a progressive enhancement; ignore failures
    }
  }

  function zoomIn(): void {
    const cur = zoom === "fit" ? fitScale : zoom;
    zoom = Math.min(cur + 0.25, 6);
  }
  function zoomOut(): void {
    const cur = zoom === "fit" ? fitScale : zoom;
    zoom = Math.max(cur - 0.25, 0.25);
  }

  /** Ctrl/⌘-wheel zooms, anchored under the cursor; plain wheel scrolls. */
  function onWheel(e: WheelEvent): void {
    if (!e.ctrlKey && !e.metaKey) return;
    const el = scroller;
    if (el === null) return;
    e.preventDefault();
    const cur = zoom === "fit" ? fitScale : zoom;
    const factor = Math.exp(-e.deltaY * 0.0015);
    const next = Math.min(Math.max(cur * factor, 0.25), 6);
    const rect = el.getBoundingClientRect();
    const cy = e.clientY - rect.top;
    const cx = e.clientX - rect.left;
    const ratio = next / cur;
    zoom = next;
    // Keep the point under the cursor fixed as content grows/shrinks.
    el.scrollTop = (el.scrollTop + cy) * ratio - cy;
    el.scrollLeft = (el.scrollLeft + cx) * ratio - cx;
    saveMemory();
  }

  // Save synchronously on every scroll — a Map write is cheap, and this
  // guarantees the latest position is stored before a tab switch unmounts us
  // (the bound `scroller` ref can already be null by cleanup time).
  function onScroll(): void {
    saveMemory();
  }

  const zoomPct = $derived(Math.round(scale * 100));

  // Persist zoom changes made via the buttons (scroll events cover panning).
  $effect(() => {
    void zoom;
    if (restored && scroller !== null) saveMemory();
  });
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

  <div class="pdf-scroll" bind:this={scroller} onwheel={onWheel} onscroll={onScroll}>
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

  .zbtn:focus-visible {
    outline: 2px solid var(--accent, #4a90d9);
    outline-offset: 1px;
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
    position: relative;
    background: #fff;
    box-shadow: 0 1px 6px rgba(0, 0, 0, 0.18);
    border-radius: 2px;
    overflow: hidden;
  }

  .pdf-slot :global(.pdf-canvas) {
    display: block;
  }

  /* pdf.js text layer: transparent, absolutely-positioned selectable spans. */
  .pdf-slot :global(.textLayer) {
    position: absolute;
    inset: 0;
    text-align: initial;
    overflow: clip;
    opacity: 1;
    line-height: 1;
    text-size-adjust: none;
    forced-color-adjust: none;
    transform-origin: 0 0;
    caret-color: CanvasText;
    z-index: 1;
  }

  .pdf-slot :global(.textLayer span),
  .pdf-slot :global(.textLayer br) {
    color: transparent;
    position: absolute;
    white-space: pre;
    cursor: text;
    transform-origin: 0% 0%;
  }

  .pdf-slot :global(.textLayer span.markedContent) {
    top: 0;
    height: 0;
  }

  .pdf-slot :global(.textLayer ::selection) {
    background: rgba(80, 140, 220, 0.35);
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
