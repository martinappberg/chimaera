<script module lang="ts">
  /** Per-tab scroll + zoom + sidebar memory, keyed by path. Module-scoped so
   * it survives the component unmount/remount that a tab switch triggers. */
  interface PdfMem {
    zoom: "fit" | number;
    scrollTop: number;
    scrollLeft: number;
    outline: boolean;
  }
  const memory = new Map<string, PdfMem>();
  const MEMORY_CAP = 100;
</script>

<script lang="ts">
  /**
   * PDF preview via pdf.js (worker, fonts, CMaps and wasm bundled locally —
   * no CDN, air-gapped rule). Pages render lazily as they scroll into view;
   * fit-width by default with fit/100%/± zoom controls, ctrl/⌘-wheel zoom
   * anchored at the cursor, a selectable text layer, clickable links, an
   * outline sidebar, find (⌘/Ctrl+F), per-tab scroll+zoom memory, `page` /
   * `region` reveals, and a "p / N" indicator that follows the scroll and
   * takes a page to jump to. Bytes come through the ticketed /raw/ URL in
   * ranges, fetched only as pages need them (a remote tunnel never streams
   * the whole file up front); the bearer token never lands in a URL.
   */
  import { onMount, untrack } from "svelte";
  // The legacy build: pdf.js 6's modern build calls `Map.prototype.
  // getOrInsertComputed` on every render and range read, which WebKit (the
  // macOS app) and Chromium before 145 lack — pages stayed blank, and ranged
  // loading threw. The legacy build carries the polyfills, worker included.
  import * as pdfjs from "pdfjs-dist/legacy/build/pdf.mjs";
  import type { PDFDocumentProxy, PDFPageProxy } from "pdfjs-dist";
  // Vite bundles the worker as a local asset; nothing is fetched from a CDN.
  import workerUrl from "pdfjs-dist/legacy/build/pdf.worker.min.mjs?url";
  import { retain, release } from "./fileStore.svelte";
  import { revealRequest, takeReveal, type Reveal } from "../shared/reveal";
  import { activateUrl, isWebUrl } from "../shared/urlOpen";
  import { findInPage, findPattern, pageText, type ItemRange, type PageText } from "./pdfFind";
  import { clampRegion, cropSize, screenToPage, type Point, type Region } from "./imageRegion";
  import { activeSelection, clearSelection, setSelection, type FileSelection } from "../shared/reference";
  import { xywhFragment } from "../shared/locator";
  import ReferenceChip from "../shared/ReferenceChip.svelte";

  pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

  /** Where the build ships pdf.js's font, CMap, wasm and ICC data (see
   *  `pdfjsAssets` in vite.config.ts). Without the standard fonts a PDF
   *  that references Helvetica without embedding it paints no text at all. */
  const PDFJS_DATA = new URL(`${import.meta.env.BASE_URL}assets/pdfjs-${pdfjs.version}/`, location.href).href;

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

  interface OutlineNode {
    title: string;
    dest: string | unknown[] | null;
    url: string | null;
    bold: boolean;
    italic: boolean;
    items: OutlineNode[];
  }

  /** One find match: its page, the text-item slices it covers, and where it
   *  sits on the page (CSS px at scale 1, from the page top). */
  interface FindHit {
    page: number;
    ranges: ItemRange[];
    y: number;
  }

  type PdfTextContent = Awaited<ReturnType<PDFPageProxy["getTextContent"]>>;
  type PdfTextReader = ReadableStreamDefaultReader<PdfTextContent>;
  type PdfTextItem = Extract<PdfTextContent["items"][number], { str: string }>;

  let scroller = $state<HTMLDivElement | null>(null);
  let containerWidth = $state(0);
  let containerHeight = $state(0);
  /** Mirrors the scroller's scrollTop for the page indicator. */
  let scrollY = $state(0);
  /** The page field's text while the user is typing in it (null = follow
   *  the scroll). */
  let pageDraft = $state<string | null>(null);
  let pages = $state.raw<PageInfo[]>([]);
  let numPages = $state(0);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let selectionLimited = $state(false);
  /** "fit" (fit width) or an explicit CSS scale factor. */
  let zoom = $state<"fit" | number>("fit");
  let restored = false;

  let outline = $state.raw<OutlineNode[]>([]);
  let outlineOpen = $state(false);
  /** Expanded outline entries, by their path of indices ("0.3.1"). */
  let outlineExpanded = $state<Set<string>>(new Set());

  let findOpen = $state(false);
  let findQuery = $state("");
  let findInput = $state<HTMLInputElement | null>(null);
  let matches = $state.raw<FindHit[]>([]);
  let current = $state(-1);
  let searching = $state(false);
  /** Pages whose text is over the item ceiling: not searched, and said so. */
  let unsearched = $state(0);
  let matchesCapped = $state(false);
  let findGen = 0;
  let findTimer: ReturnType<typeof setTimeout> | null = null;

  /** A revealed region (PDF points from the page's top-left, at 100%). */
  let region = $state<{ page: number; x: number; y: number; w: number; h: number } | null>(null);
  let regionFlash = $state(0);

  /** The area tool is on: a drag draws a box instead of selecting text
   *  (Shift-drag draws one either way). */
  let areaMode = $state(false);
  /** The box being dragged, then the one pointed at (PDF points). */
  let pick = $state.raw<({ page: number } & Region) | null>(null);
  /** Where the "reference in agent" chip sits (px in .pdf-body), for a
   *  text selection or a finished box. */
  let chipPos = $state<{ x: number; y: number } | null>(null);
  let chipLabel = $state<string | undefined>(undefined);
  let body = $state<HTMLDivElement | null>(null);

  let doc: PDFDocumentProxy | null = null;
  let task: ReturnType<typeof pdfjs.getDocument> | null = null;
  const rendered = new Map<number, HTMLCanvasElement>();
  const renderingPages = new Set<number>();
  const renderedScale = new Map<number, number>();
  /** pdf.js work survives ordinary DOM removal unless explicitly cancelled.
   *  Keep handles so closing a tab cannot leave raster/text work burning the
   *  UI thread after its pane is gone. */
  const renderTasks = new Map<number, ReturnType<PDFPageProxy["render"]>>();
  const textLayers = new Map<number, InstanceType<typeof pdfjs.TextLayer>>();
  const textReaders = new Set<PdfTextReader>();
  /** Completed text layers reserve from one viewer-wide DOM budget. */
  const textItemCounts = new Map<number, number>();
  /** Pages over the per-page ceiling stay canvas-only across zoom rerenders. */
  const complexTextPages = new Set<number>();
  /** Each rendered text layer's spans and their strings, for find highlights
   *  (span i shows item i's string — pdf.js's TextLayer contract). */
  const pageSpans = new Map<number, { spans: HTMLElement[]; strs: string[]; painted: number[] }>();
  /** Pages whose link layer is built (positions are zoom-independent). */
  const linkedPages = new Set<number>();
  /** Searchable text per page, kept within a character budget. */
  const findText = new Map<number, { pt: PageText; ys: Float32Array }>();
  let findTextChars = 0;
  let observer: IntersectionObserver | null = null;
  let disposed = false;
  let restoreFrame: number | null = null;
  /** Pages inside the observer margin; never evict a canvas the user is at. */
  const nearbyPages = new Set<number>();
  const dpr = typeof window !== "undefined" ? Math.min(window.devicePixelRatio || 1, 2) : 1;
  /** Bound decoded raster memory while retaining a generous high-DPI page. */
  const MAX_CANVAS_PIXELS = 12_000_000;
  /** Canvases outside the viewport margin are an LRU, not a document-long leak. */
  const MAX_RENDERED_PAGES = 8;
  /** pdf.js creates roughly one selectable DOM run per item. Scientific plots
   * can encode every point as text: the Sherlock UMAP repro has 36k items and
   * produced 60k DOM nodes on one page. Keep selection a bounded enhancement;
   * the canvas remains the authoritative preview. */
  const MAX_TEXT_ITEMS_PER_PAGE = 5_000;
  const MAX_TEXT_ITEMS_TOTAL = 12_000;
  const PAGE_INFO_BATCH = 24;
  /** Find stops counting here; the bar says so. */
  const MAX_MATCHES = 5_000;
  /** Page text kept for repeat searches (UTF-16 units, ~2 bytes each). */
  const FIND_TEXT_BUDGET = 8_000_000;

  // Effective CSS scale: fit-width divides the container by the widest page,
  // clamped so a tiny pane doesn't render illegibly small.
  const fitScale = $derived.by(() => {
    if (pages.length === 0 || containerWidth === 0) return 1;
    let widest = 0;
    for (const p of pages) if (p.w > widest) widest = p.w;
    // 32px accounts for page horizontal margins in the column.
    return Math.max((containerWidth - 32) / widest, 0.1);
  });
  const scale = $derived(zoom === "fit" ? fitScale : zoom);

  /** Layout of the page column, from the slot sizes alone (no DOM reads):
   *  it mirrors .pdf-scroll's padding and gap below. */
  const SCROLL_PAD = 14;
  const PAGE_GAP = 12;
  const pageTops = $derived.by(() => {
    const tops: number[] = [];
    let y = SCROLL_PAD;
    for (const p of pages) {
      tops.push(y);
      y += p.h * scale + PAGE_GAP;
    }
    return tops;
  });
  /** The page under a reading line a third of the way down the viewport —
   *  the one being read, not the sliver of the next one at the bottom. */
  const currentPage = $derived.by(() => {
    const tops = pageTops;
    if (tops.length === 0) return 0;
    const y = scrollY + containerHeight / 3;
    let lo = 0;
    let hi = tops.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (tops[mid] <= y) lo = mid;
      else hi = mid - 1;
    }
    return lo + 1;
  });

  function jumpToPage(n: number): void {
    const el = scroller;
    if (el === null || pageTops.length === 0 || !Number.isFinite(n)) return;
    const i = Math.min(Math.max(Math.round(n), 1), pageTops.length) - 1;
    el.scrollTop = pageTops[i] - SCROLL_PAD / 2;
  }

  /** Scroll so point `y` (CSS px at scale 1, from the page top) of page `n`
   *  sits a quarter of the way down the view; null `y` = the page's top. */
  function scrollToPagePoint(n: number, y: number | null, x: number | null = null): void {
    const el = scroller;
    if (el === null || pageTops.length === 0) return;
    const i = Math.min(Math.max(Math.round(n), 1), pageTops.length) - 1;
    if (y === null) {
      el.scrollTop = pageTops[i] - SCROLL_PAD / 2;
    } else {
      el.scrollTop = Math.max(0, pageTops[i] + y * scale - containerHeight / 4);
    }
    if (x !== null && el.scrollWidth > el.clientWidth) {
      const pageLeft = Math.max(0, (el.scrollWidth - pages[i].w * scale) / 2);
      el.scrollLeft = Math.max(0, pageLeft + x * scale - containerWidth / 3);
    }
  }

  function onPageKey(e: KeyboardEvent): void {
    const input = e.currentTarget as HTMLInputElement;
    if (e.key === "Enter") {
      e.preventDefault();
      jumpToPage(Number.parseInt(input.value, 10));
      pageDraft = null;
      input.select();
    } else if (e.key === "Escape") {
      pageDraft = null;
      input.blur();
    }
  }

  function errorText(e: unknown, fallback: string): string {
    return e instanceof Error
      ? e.message
      : typeof e === "object" && e !== null && "message" in e
        ? String(e.message)
        : e === undefined
          ? fallback
          : String(e);
  }

  onMount(() => {
    disposed = false;
    const mem = memory.get(path);
    if (mem !== undefined) {
      zoom = mem.zoom;
      outlineOpen = mem.outline;
    }
    // Pin + share the ticketed /raw/ URL through the store (cached across a tab
    // switch — no re-mint). pdf.js still re-parses on remount; the URL cache and
    // the per-tab scroll/zoom memory are what make the return feel instant.
    const fileEntry = retain(path);
    void (async () => {
      try {
        await fileEntry.ensureRawUrl();
        if (disposed) return;
        const url = fileEntry.rawUrl;
        if (url === null) throw new Error(fileEntry.rawError ?? "failed to open pdf");
        const loadingTask = pdfjs.getDocument({
          url,
          // Fetch only the byte ranges the visible pages need (the daemon
          // serves ranges); streaming would pull the whole file down a slow
          // tunnel before the first page could use it.
          disableAutoFetch: true,
          disableStream: true,
          cMapUrl: `${PDFJS_DATA}cmaps/`,
          cMapPacked: true,
          standardFontDataUrl: `${PDFJS_DATA}standard_fonts/`,
          wasmUrl: `${PDFJS_DATA}wasm/`,
          iccUrl: `${PDFJS_DATA}iccs/`,
        });
        task = loadingTask;
        const d = await loadingTask.promise;
        if (disposed) {
          void loadingTask.destroy();
          return;
        }
        doc = d;
        numPages = d.numPages;
        // Every page gets a placeholder sized like page 1 at once, so the
        // scrollbar, jumps and reveals work before the size walk below has
        // visited later pages (each visit is a ranged read on a remote host).
        const first = await d.getPage(1);
        if (disposed) return;
        const vp1 = first.getViewport({ scale: 1 });
        first.cleanup();
        const infos: PageInfo[] = Array.from({ length: d.numPages }, (_, i) => ({
          num: i + 1,
          w: vp1.width,
          h: vp1.height,
        }));
        pages = [...infos];
        loading = false;
        void loadOutline(d);
        let changed = false;
        for (let n = 2; n <= d.numPages; n++) {
          const page = await d.getPage(n);
          if (disposed) {
            page.cleanup();
            return;
          }
          const vp = page.getViewport({ scale: 1 });
          page.cleanup();
          if (vp.width !== infos[n - 1].w || vp.height !== infos[n - 1].h) {
            infos[n - 1] = { num: n, w: vp.width, h: vp.height };
            changed = true;
          }
          // Batch updates avoid O(n²) array churn on a long document.
          if (changed && (n % PAGE_INFO_BATCH === 0 || n === d.numPages)) {
            pages = [...infos];
            changed = false;
          }
        }
      } catch (e) {
        if (!disposed) {
          error = errorText(e, "failed to open pdf");
          loading = false;
        }
      }
    })();

    return () => {
      disposed = true;
      findGen += 1;
      release(path);
      saveMemory();
      observer?.disconnect();
      if (restoreFrame !== null) cancelAnimationFrame(restoreFrame);
      restoreFrame = null;
      if (reRenderTimer !== null) clearTimeout(reRenderTimer);
      reRenderTimer = null;
      if (findTimer !== null) clearTimeout(findTimer);
      findTimer = null;
      for (const renderTask of renderTasks.values()) renderTask.cancel();
      for (const textLayer of textLayers.values()) textLayer.cancel();
      for (const reader of textReaders) {
        void reader.cancel(new Error("PDF view closed")).catch(() => {});
      }
      renderTasks.clear();
      textLayers.clear();
      textReaders.clear();
      textItemCounts.clear();
      complexTextPages.clear();
      pageSpans.clear();
      linkedPages.clear();
      findText.clear();
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
    memory.set(path, { zoom, scrollTop: el.scrollTop, scrollLeft: el.scrollLeft, outline: outlineOpen });
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
      for (const e of entries) {
        containerWidth = e.contentRect.width;
        containerHeight = e.contentRect.height;
      }
    });
    ro.observe(el);
    containerWidth = el.clientWidth;
    containerHeight = el.clientHeight;
    return () => ro.disconnect();
  });

  // Restore remembered scroll once pages have laid out (a reveal wins).
  $effect(() => {
    if (restored || loading || pages.length === 0 || scroller === null || containerWidth === 0) return;
    const mem = memory.get(path);
    restored = true;
    if (mem !== undefined) {
      // wait a frame so slot heights exist
      restoreFrame = requestAnimationFrame(() => {
        restoreFrame = null;
        if (!disposed && scroller !== null) {
          scroller.scrollTop = mem.scrollTop;
          scroller.scrollLeft = mem.scrollLeft;
        }
      });
    }
  });

  // Lazy-render observer: render a page's canvas as its slot nears the viewport.
  // Slots are keyed by page number, so a size refinement keeps them observed.
  $effect(() => {
    const el = scroller;
    if (el === null || loading || numPages === 0) return;
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
  // Text, link and highlight layers follow at once: they size from the
  // slot's --total-scale-factor.
  let reRenderTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const s = scale;
    const list = pages;
    // Cheap immediate stretch of what's already drawn.
    for (const [n, canvas] of rendered) {
      const info = list[n - 1];
      if (info === undefined) continue;
      canvas.style.width = `${info.w * s}px`;
      canvas.style.height = `${info.h * s}px`;
    }
    if (reRenderTimer !== null) clearTimeout(reRenderTimer);
    reRenderTimer = setTimeout(() => {
      reRenderTimer = null;
      if (disposed) return;
      for (const [n] of rendered) {
        const slot = scroller?.querySelector<HTMLElement>(`[data-page="${n}"]`);
        if (slot !== null && slot !== undefined) void renderPage(n, slot);
      }
    }, 140);
  });

  async function renderPage(n: number, slot: HTMLElement): Promise<void> {
    const d = doc;
    if (d === null || disposed || !slot.isConnected) return;
    const s = zoom === "fit" ? fitScale : zoom;
    if (renderingPages.has(n) || renderedScale.get(n) === s) return;
    renderingPages.add(n);
    let page: PDFPageProxy | null = null;
    let renderedAtScale = false;
    try {
      page = await d.getPage(n);
      if (disposed || !slot.isConnected) return;
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
      const renderTask = page.render({ canvas, canvasContext: ctx, viewport });
      renderTasks.set(n, renderTask);
      await renderTask.promise;
      if (renderTasks.get(n) === renderTask) renderTasks.delete(n);
      if (disposed || !slot.isConnected) return;
      renderedScale.set(n, s);
      renderedAtScale = true;
      rememberRendered(n, canvas);
      // Selectable text layer, positioned by --total-scale-factor.
      await renderTextLayer(page, slot, s);
      if (!linkedPages.has(n)) await renderLinkLayer(page, slot, s);
    } catch {
      // a page failed to render; leave its placeholder in place
    } finally {
      renderTasks.delete(n);
      page?.cleanup();
      renderingPages.delete(n);
      // A zoom/fit change may land while this page is rasterizing. Never
      // render into the same canvas concurrently; finish once, then catch up.
      const currentScale = zoom === "fit" ? fitScale : zoom;
      if (!disposed && renderedAtScale && currentScale !== s && slot.isConnected) {
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
      const oldSlot = scroller?.querySelector<HTMLElement>(`[data-page="${old}"]`);
      oldSlot?.querySelector(".textLayer")?.remove();
      oldSlot?.querySelector(".annotationLayer")?.remove();
      rendered.delete(old);
      renderedScale.delete(old);
      textItemCounts.delete(old);
      pageSpans.delete(old);
      linkedPages.delete(old);
    }
  }

  /** Read only enough streamed text to decide whether it is safe to use.
   * Cancelling at the ceiling prevents a tiny, highly-compressed vector PDF
   * from expanding into an unbounded main-thread object graph. */
  async function readBoundedTextContent(page: PDFPageProxy): Promise<PdfTextContent | null | undefined> {
    const stream = page.streamTextContent({
      includeMarkedContent: true,
      disableNormalization: true,
    }) as ReadableStream<PdfTextContent>;
    const reader = stream.getReader();
    textReaders.add(reader);
    const items: PdfTextContent["items"] = [];
    const styles: PdfTextContent["styles"] = {};
    let lang: string | null = null;
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) return { items, styles, lang };
        if (disposed) {
          await reader.cancel(new Error("PDF view closed"));
          return undefined;
        }
        if (items.length + value.items.length > MAX_TEXT_ITEMS_PER_PAGE) {
          await reader.cancel(new Error("PDF text-layer item limit"));
          return null;
        }
        items.push(...value.items);
        Object.assign(styles, value.styles);
        lang ??= value.lang;
      }
    } catch {
      // Cancellation and malformed text content do not affect the canvas.
      return undefined;
    } finally {
      textReaders.delete(reader);
      reader.releaseLock();
    }
  }

  function omitTextLayer(pageNumber: number, slot: HTMLElement, remember: boolean): void {
    textLayers.get(pageNumber)?.cancel();
    textLayers.delete(pageNumber);
    slot.querySelector(".textLayer")?.remove();
    textItemCounts.delete(pageNumber);
    pageSpans.delete(pageNumber);
    if (remember) complexTextPages.add(pageNumber);
    selectionLimited = true;
  }

  async function renderTextLayer(page: PDFPageProxy, slot: HTMLElement, s: number): Promise<void> {
    let layer: HTMLDivElement | null = null;
    try {
      if (disposed || !slot.isConnected) return;
      if (complexTextPages.has(page.pageNumber)) {
        omitTextLayer(page.pageNumber, slot, true);
        return;
      }
      const source = await readBoundedTextContent(page);
      if (source === undefined || disposed || !slot.isConnected) return;
      if (source === null) {
        omitTextLayer(page.pageNumber, slot, true);
        return;
      }
      const reservedElsewhere = [...textItemCounts.entries()].reduce(
        (total, [n, count]) => total + (n === page.pageNumber ? 0 : count),
        0,
      );
      if (reservedElsewhere + source.items.length > MAX_TEXT_ITEMS_TOTAL) {
        omitTextLayer(page.pageNumber, slot, false);
        return;
      }
      textItemCounts.set(page.pageNumber, source.items.length);
      layer = slot.querySelector<HTMLDivElement>(".textLayer");
      if (layer === null) {
        layer = document.createElement("div");
        layer.className = "textLayer";
        slot.appendChild(layer);
      }
      layer.replaceChildren();
      const viewport = page.getViewport({ scale: s });
      const tl = new pdfjs.TextLayer({ textContentSource: source, container: layer, viewport });
      textLayers.set(page.pageNumber, tl);
      await tl.render();
      pageSpans.set(page.pageNumber, {
        spans: tl.textDivs as HTMLElement[],
        strs: tl.textContentItemsStr as string[],
        painted: [],
      });
      paintHighlights(page.pageNumber);
    } catch {
      // text layer is a progressive enhancement; ignore failures
      layer?.remove();
      textItemCounts.delete(page.pageNumber);
      pageSpans.delete(page.pageNumber);
    } finally {
      textLayers.delete(page.pageNumber);
    }
  }

  // --- links ---------------------------------------------------------------------

  /** The slice of pdf.js's link-service contract that link annotations use.
   *  External URLs go through the app's one link policy (`activateUrl`: a
   *  live local app opens in a pane, anything else in the real browser);
   *  anything but http(s) is inert. */
  const linkService = {
    externalLinkEnabled: true,
    isInPresentationMode: false,
    rotation: 0,
    eventBus: null,
    get pagesCount(): number {
      return numPages;
    },
    get page(): number {
      return currentPage;
    },
    addLinkAttributes(link: HTMLAnchorElement, url: string): void {
      if (!isWebUrl(url)) {
        link.removeAttribute("href");
        return;
      }
      link.href = url;
      link.rel = "noopener noreferrer";
      link.title = url;
      link.addEventListener("click", (e) => {
        e.preventDefault();
        activateUrl(url, e.metaKey || e.ctrlKey);
      });
    },
    getDestinationHash: (): string => "#",
    getAnchorUrl: (): string => "#",
    goToDestination(dest: string | unknown[]): void {
      void goToDestination(dest);
    },
    goToPage(n: number): void {
      jumpToPage(n);
    },
    executeNamedAction(action: string): void {
      const n = currentPage;
      if (action === "NextPage") jumpToPage(n + 1);
      else if (action === "PrevPage") jumpToPage(n - 1);
      else if (action === "FirstPage") jumpToPage(1);
      else if (action === "LastPage") jumpToPage(numPages);
    },
    executeSetOCGState(): void {},
    getAttachmentContent: async (): Promise<null> => null,
  };

  /** Build the page's link layer (links only: no forms, no scripting). */
  async function renderLinkLayer(page: PDFPageProxy, slot: HTMLElement, s: number): Promise<void> {
    try {
      const all = await page.getAnnotations({ intent: "display" });
      if (disposed || !slot.isConnected) return;
      linkedPages.add(page.pageNumber);
      const links = all.filter((a) => a.subtype === "Link");
      if (links.length === 0) return;
      slot.querySelector(".annotationLayer")?.remove();
      const div = document.createElement("div");
      div.className = "annotationLayer";
      slot.appendChild(div);
      const viewport = page.getViewport({ scale: s });
      const layer = new pdfjs.AnnotationLayer({
        div,
        page,
        viewport,
        linkService,
        accessibilityManager: null,
        annotationCanvasMap: null,
        annotationEditorUIManager: null,
        structTreeLayer: null,
        commentManager: null,
        annotationStorage: null,
      });
      await layer.render({
        div,
        page,
        viewport: viewport.clone({ dontFlip: true }),
        annotations: links,
        linkService: linkService as unknown as Parameters<typeof layer.render>[0]["linkService"],
        renderForms: false,
        enableScripting: false,
      });
    } catch {
      // links are an enhancement; the page itself is already drawn
    }
  }

  /** Resolve a PDF destination (named, or explicit `[ref, {name}, …args]`)
   *  and scroll to its page and point. */
  async function goToDestination(dest: string | unknown[] | null): Promise<void> {
    const d = doc;
    if (d === null || dest === null) return;
    try {
      const explicit = typeof dest === "string" ? await d.getDestination(dest) : dest;
      if (!Array.isArray(explicit) || explicit.length === 0) return;
      const [ref, kind, ...args] = explicit as [unknown, { name?: string } | undefined, ...unknown[]];
      let index: number;
      if (typeof ref === "object" && ref !== null) {
        index = await d.getPageIndex(ref as Parameters<PDFDocumentProxy["getPageIndex"]>[0]);
      } else if (Number.isInteger(ref)) {
        index = ref as number;
      } else {
        return;
      }
      const n = index + 1;
      if (disposed || n < 1 || n > numPages) return;
      const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
      let left: number | null = null;
      let top: number | null = null;
      switch (kind?.name) {
        case "XYZ":
          left = num(args[0]);
          top = num(args[1]);
          break;
        case "FitH":
        case "FitBH":
          top = num(args[0]);
          break;
        case "FitR":
          left = num(args[0]);
          top = num(args[3]);
          break;
        default:
          break;
      }
      if (top === null) {
        scrollToPagePoint(n, null);
        return;
      }
      const page = await d.getPage(n);
      const [x, y] = page.getViewport({ scale: 1 }).convertToViewportPoint(left ?? 0, top);
      page.cleanup();
      if (disposed) return;
      scrollToPagePoint(n, Math.max(0, y), left === null ? null : x);
    } catch {
      // a broken destination goes nowhere
    }
  }

  // --- outline -------------------------------------------------------------------

  async function loadOutline(d: PDFDocumentProxy): Promise<void> {
    try {
      const raw = await d.getOutline();
      if (disposed || raw === null) return;
      const convert = (items: typeof raw): OutlineNode[] =>
        items.map((it) => ({
          title: it.title,
          dest: it.dest,
          url: it.url,
          bold: it.bold,
          italic: it.italic,
          items: convert(it.items ?? []),
        }));
      outline = convert(raw);
      // The first level opens expanded; deeper levels on demand.
      outlineExpanded = new Set(outline.map((_, i) => String(i)));
    } catch {
      outline = [];
    }
  }

  function toggleOutlineNode(key: string): void {
    const next = new Set(outlineExpanded);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    outlineExpanded = next;
  }

  function openOutlineNode(node: OutlineNode, e: MouseEvent): void {
    if (node.url !== null) {
      if (isWebUrl(node.url)) activateUrl(node.url, e.metaKey || e.ctrlKey);
      return;
    }
    void goToDestination(node.dest);
  }

  function toggleOutline(): void {
    outlineOpen = !outlineOpen;
    saveMemory();
  }

  // --- find ----------------------------------------------------------------------

  function openFind(): void {
    findOpen = true;
    queueMicrotask(() => {
      findInput?.focus();
      findInput?.select();
    });
    if (findQuery.trim() !== "" && matches.length === 0 && !searching) startSearch();
  }

  function closeFind(): void {
    findOpen = false;
    findGen += 1;
    searching = false;
    if (findTimer !== null) clearTimeout(findTimer);
    findTimer = null;
    matches = [];
    current = -1;
    paintAll();
    scroller?.focus();
  }

  function onFindInput(e: Event): void {
    findQuery = (e.currentTarget as HTMLInputElement).value;
    if (findTimer !== null) clearTimeout(findTimer);
    findTimer = setTimeout(() => {
      findTimer = null;
      startSearch();
    }, 180);
  }

  function onFindKey(e: KeyboardEvent): void {
    if (e.key === "Enter") {
      e.preventDefault();
      if (findTimer !== null) {
        clearTimeout(findTimer);
        findTimer = null;
        startSearch();
        return;
      }
      step(e.shiftKey ? -1 : 1);
    } else if (e.key === "Escape") {
      e.preventDefault();
      closeFind();
    }
  }

  /** ⌘/Ctrl+F anywhere in the view opens find (the browser's own find
   *  cannot see pages that are not rendered). */
  function onViewKey(e: KeyboardEvent): void {
    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f") {
      e.preventDefault();
      openFind();
    } else if (e.key === "Escape" && !findOpen) {
      if (pick !== null) clearPick();
      else if (region !== null) region = null;
      else if (areaMode) areaMode = false;
    }
  }

  /** Searchable text for page `n`: null when the page is over the item
   *  ceiling (never searched), undefined when it could not be read. */
  async function pageSearchText(n: number): Promise<{ pt: PageText; ys: Float32Array } | null | undefined> {
    const hit = findText.get(n);
    if (hit !== undefined) return hit;
    if (complexTextPages.has(n)) return null;
    const d = doc;
    if (d === null) return undefined;
    const page = await d.getPage(n);
    try {
      const content = await readBoundedTextContent(page);
      if (content === null) {
        complexTextPages.add(n);
        return null;
      }
      if (content === undefined) return undefined;
      const items = content.items.filter((it): it is PdfTextItem => "str" in it);
      const pt = pageText(items);
      // Each item's top edge, for scrolling to a match on a page whose text
      // layer is not rendered yet.
      const vp = page.getViewport({ scale: 1 });
      const ys = new Float32Array(items.length);
      items.forEach((it, i) => {
        const [, y] = vp.convertToViewportPoint(it.transform[4], it.transform[5]);
        ys[i] = y - (it.height || Math.abs(it.transform[3]) || 0);
      });
      const entry = { pt, ys };
      if (findTextChars + pt.text.length <= FIND_TEXT_BUDGET) {
        findText.set(n, entry);
        findTextChars += pt.text.length;
      }
      return entry;
    } finally {
      page.cleanup();
    }
  }

  /** Walk every page for the query, one page per task so the UI stays
   *  responsive; results arrive progressively. The walk starts at the page
   *  being read and wraps, so the first match is the nearest one ahead. */
  function startSearch(): void {
    const gen = ++findGen;
    const re = findPattern(findQuery);
    matches = [];
    current = -1;
    unsearched = 0;
    matchesCapped = false;
    paintAll();
    if (re === null || doc === null || numPages === 0) {
      searching = false;
      return;
    }
    searching = true;
    const start = Math.max(1, currentPage);
    void (async () => {
      const found: FindHit[] = [];
      let skipped = 0;
      for (let k = 0; k < numPages; k++) {
        const n = ((start - 1 + k) % numPages) + 1;
        let text: Awaited<ReturnType<typeof pageSearchText>>;
        try {
          text = await pageSearchText(n);
        } catch {
          text = undefined;
        }
        if (gen !== findGen || disposed) return;
        if (text === null || text === undefined) {
          skipped += 1;
        } else {
          for (const ranges of findInPage(text.pt, re, MAX_MATCHES - found.length)) {
            found.push({ page: n, ranges, y: Math.max(0, text.ys[ranges[0].item] ?? 0) });
          }
        }
        if (found.length > 0 && current === -1) {
          current = 0;
          matches = [...found];
          revealMatch(0);
        }
        if (found.length >= MAX_MATCHES) {
          matchesCapped = true;
          break;
        }
        // Publish every few pages (and at the end) so the count climbs.
        if (k % 8 === 7) {
          matches = [...found];
          unsearched = skipped;
          paintAll();
        }
        await new Promise<void>((r) => setTimeout(r, 0));
        if (gen !== findGen || disposed) return;
      }
      // Present matches in document order, keeping the current one current.
      const selected = current >= 0 ? found[current] : undefined;
      found.sort((a, b) => a.page - b.page || a.y - b.y);
      matches = found;
      current = selected !== undefined ? found.indexOf(selected) : found.length > 0 ? 0 : -1;
      unsearched = skipped;
      searching = false;
      paintAll();
    })();
  }

  function step(dir: 1 | -1): void {
    if (matches.length === 0) {
      if (!searching) startSearch();
      return;
    }
    current = (current + dir + matches.length) % matches.length;
    revealMatch(current);
  }

  function revealMatch(i: number): void {
    const hit = matches[i];
    if (hit === undefined) return;
    scrollToPagePoint(hit.page, hit.y);
    paintAll();
  }

  /** Repaint find highlights on every rendered text layer. */
  function paintAll(): void {
    for (const n of pageSpans.keys()) paintHighlights(n);
  }

  /** Wrap this page's matched substrings in highlight spans (the current
   *  match marked), restoring any spans painted before. */
  function paintHighlights(n: number): void {
    const layer = pageSpans.get(n);
    if (layer === undefined) return;
    for (const i of layer.painted) {
      const span = layer.spans[i];
      if (span !== undefined) span.textContent = layer.strs[i];
    }
    layer.painted = [];
    if (!findOpen || matches.length === 0) return;
    const byItem = new Map<number, { from: number; to: number; selected: boolean }[]>();
    matches.forEach((hit, m) => {
      if (hit.page !== n) return;
      for (const r of hit.ranges) {
        const list = byItem.get(r.item) ?? [];
        list.push({ from: r.from, to: r.to, selected: m === current });
        byItem.set(r.item, list);
      }
    });
    for (const [i, ranges] of byItem) {
      const span = layer.spans[i];
      const str = layer.strs[i];
      if (span === undefined || str === undefined) continue;
      ranges.sort((a, b) => a.from - b.from);
      const frag = document.createDocumentFragment();
      let pos = 0;
      for (const r of ranges) {
        const from = Math.max(r.from, pos);
        if (from > pos) frag.append(str.slice(pos, from));
        if (r.to <= from) continue;
        const mark = document.createElement("span");
        mark.className = r.selected ? "highlight selected" : "highlight";
        mark.textContent = str.slice(from, r.to);
        frag.append(mark);
        pos = r.to;
      }
      if (pos < str.length) frag.append(str.slice(pos));
      span.replaceChildren(frag);
      layer.painted.push(i);
    }
  }

  const findStatus = $derived.by(() => {
    if (findQuery.trim() === "") return "";
    if (matches.length === 0) return searching ? "searching…" : "no matches";
    const count = `${matches.length.toLocaleString("en-US")}${searching || matchesCapped ? "+" : ""}`;
    return `${current + 1} / ${count}`;
  });

  // --- reveals ---------------------------------------------------------------------

  // `#page=N` (and `#xywh=` on that page) from a link or an agent: taken once
  // the page column exists; it wins over the remembered scroll position.
  $effect(() => {
    void $revealRequest;
    if (loading || pages.length === 0 || scroller === null) return;
    const req = takeReveal(path);
    if (req === null) return;
    untrack(() => applyReveal(req));
  });

  function applyReveal(req: Reveal): void {
    const n = req.page ?? (req.region !== undefined ? 1 : null);
    if (n === null) return;
    restored = true;
    if (restoreFrame !== null) cancelAnimationFrame(restoreFrame);
    restoreFrame = null;
    const page = Math.min(Math.max(Math.round(n), 1), numPages);
    let r = req.region;
    const info = pages[page - 1];
    if (r?.percent === true && info !== undefined) {
      // `#xywh=percent:…` is relative to the page's own size.
      r = { x: (r.x / 100) * info.w, y: (r.y / 100) * info.h, w: (r.w / 100) * info.w, h: (r.h / 100) * info.h };
    }
    if (r !== undefined && [r.x, r.y, r.w, r.h].every(Number.isFinite) && r.w > 0 && r.h > 0) {
      region = { page, x: r.x, y: r.y, w: r.w, h: r.h };
      regionFlash += 1;
      scrollToPagePoint(page, r.y, r.x);
    } else {
      region = null;
      scrollToPagePoint(page, null);
    }
  }

  // --- pointing at part of a page (context bridge) -----------------------------------
  //
  // A text selection sends its page and quote (`#page=3 "…"`); a box drawn
  // with the area tool (or Shift-drag) sends `#page=3&xywh=…` in PDF points
  // plus a PNG crop rendered from the page's vectors. Both publish through
  // the shared reference bridge and show the shared chip.

  const selOwner = {};
  /** What this view last published, and which kind: it clears only its
   *  own, and a newer selection elsewhere is told apart by identity. */
  let published: FileSelection | null = null;
  let publishedKind: "text" | "area" | null = null;
  /** An in-flight box drag (page-point anchor + client start). */
  let dragFrom: { page: number; at: Point; client: Point; pointer: number } | null = null;
  let dragging = $state(false);
  let chipFrame = 0;
  /** Crops wait on a render; only the newest box's lands. */
  let pickGen = 0;
  /** Crop raster: 2 px per point, the long side capped where models downsample. */
  const CROP_FACTOR = 2;
  const CROP_CAP = 1568;
  /** Text taken from under a box, before the composer's own excerpt cut. */
  const REGION_TEXT_MAX = 2000;
  /** Room the chip needs inside .pdf-body (px). */
  const CHIP_W = 170;
  const CHIP_H = 28;

  function publish(sel: FileSelection, kind: "text" | "area"): void {
    published = sel;
    publishedKind = kind;
    setSelection(selOwner, sel);
  }

  function unpublish(kind: "text" | "area" | null = null): void {
    if (kind !== null && publishedKind !== kind) return;
    published = null;
    publishedKind = null;
    chipPos = null;
    clearSelection(selOwner);
  }

  // A newer selection elsewhere (another view, a terminal) replaces this
  // one: the chip goes, and so does a finished box.
  $effect(() => {
    const a = $activeSelection;
    if (published === null || a === published) return;
    const wasArea = publishedKind === "area";
    published = null;
    publishedKind = null;
    chipPos = null;
    if (wasArea && dragFrom === null) pick = null;
  });

  $effect(() => {
    document.addEventListener("selectionchange", syncTextSelection);
    return () => {
      document.removeEventListener("selectionchange", syncTextSelection);
      if (chipFrame !== 0) cancelAnimationFrame(chipFrame);
      chipFrame = 0;
      unpublish();
    };
  });

  /** The page a DOM node sits on, or null outside every page. */
  function nodePage(node: Node): number | null {
    const el = node instanceof Element ? node : node.parentElement;
    const n = Number(el?.closest<HTMLElement>("[data-page]")?.dataset.page);
    return Number.isFinite(n) && n >= 1 ? n : null;
  }

  function syncTextSelection(): void {
    const el = scroller;
    const s = document.getSelection();
    if (el === null || s === null || s.rangeCount === 0 || s.isCollapsed) {
      unpublish("text");
      return;
    }
    const range = s.getRangeAt(0);
    const text = s.toString();
    if (!el.contains(range.commonAncestorContainer) || text.trim() === "") {
      unpublish("text");
      return;
    }
    const first = nodePage(range.startContainer) ?? nodePage(range.endContainer);
    if (first === null) return;
    const last = nodePage(range.endContainer) ?? first;
    // A text selection replaces a box.
    if (pick !== null && dragFrom === null) {
      pick = null;
      pickGen += 1;
    }
    const label = last > first ? `pp. ${first}–${last}` : `p. ${first}`;
    publish({ kind: "file", path, startLine: null, endLine: null, text, fragment: `page=${first}`, label }, "text");
    chipLabel = label;
    placeChip();
  }

  /** The chip's spot in .pdf-body for a client point, kept inside it. */
  function chipAt(clientX: number, clientY: number): { x: number; y: number } | null {
    const b = body;
    if (b === null) return null;
    const r = b.getBoundingClientRect();
    const clamp = (n: number, lo: number, hi: number) => Math.min(Math.max(n, lo), Math.max(lo, hi));
    return {
      x: clamp(clientX - r.left, 4, r.width - CHIP_W),
      y: clamp(clientY - r.top, 4, r.height - CHIP_H - 4),
    };
  }

  /** Geometry only: re-anchor the chip to its selection or box (a scroll
   *  or a zoom moves them; neither changes what is selected). */
  function placeChip(): void {
    if (publishedKind === "text") {
      const s = document.getSelection();
      if (s === null || s.rangeCount === 0) return;
      const range = s.getRangeAt(0);
      const rects = range.getClientRects();
      const last = rects.length > 0 ? rects[rects.length - 1] : range.getBoundingClientRect();
      chipPos = chipAt(last.right + 4, last.bottom + 6);
    } else if (publishedKind === "area" && pick !== null) {
      const slot = scroller?.querySelector<HTMLElement>(`[data-page="${pick.page}"]`);
      if (slot === null || slot === undefined) return;
      const r = slot.getBoundingClientRect();
      const left = r.left + pick.x * scale;
      const right = r.left + (pick.x + pick.w) * scale;
      chipPos = chipAt(Math.max(left, right - CHIP_W), r.top + (pick.y + pick.h) * scale + 6);
    }
  }

  function schedulePlaceChip(): void {
    if (chipPos === null || chipFrame !== 0) return;
    chipFrame = requestAnimationFrame(() => {
      chipFrame = 0;
      placeChip();
    });
  }

  // A zoom re-lays the pages out: re-anchor once they have their new size.
  $effect(() => {
    void scale;
    untrack(schedulePlaceChip);
  });

  function clearPick(): void {
    pick = null;
    pickGen += 1;
    unpublish("area");
  }

  function toggleArea(): void {
    areaMode = !areaMode;
    if (areaMode) document.getSelection()?.removeAllRanges();
    scroller?.focus({ preventScroll: true });
  }

  function slotPoint(page: number, clientX: number, clientY: number): Point | null {
    const slot = scroller?.querySelector<HTMLElement>(`[data-page="${page}"]`);
    if (slot === null || slot === undefined) return null;
    const r = slot.getBoundingClientRect();
    return screenToPage(clientX, clientY, { x: r.left, y: r.top }, scale);
  }

  function onPagePointerDown(e: PointerEvent): void {
    if (e.button !== 0 || !(areaMode || e.shiftKey)) return;
    const slot = e.target instanceof Element ? e.target.closest<HTMLElement>(".pdf-slot") : null;
    const n = Number(slot?.dataset.page);
    if (slot === null || !Number.isFinite(n) || pages[n - 1] === undefined) return;
    const at = slotPoint(n, e.clientX, e.clientY);
    if (at === null) return;
    // The press draws a box: no text selection, no link.
    e.preventDefault();
    document.getSelection()?.removeAllRanges();
    if (pick !== null) clearPick();
    dragFrom = { page: n, at, client: { x: e.clientX, y: e.clientY }, pointer: e.pointerId };
    dragging = true;
    scroller?.setPointerCapture(e.pointerId);
    scroller?.focus({ preventScroll: true });
  }

  function onPagePointerMove(e: PointerEvent): void {
    const from = dragFrom;
    if (from === null || e.pointerId !== from.pointer) return;
    // A few pixels of jitter is a click, not a box.
    if (pick === null && Math.hypot(e.clientX - from.client.x, e.clientY - from.client.y) < 4) return;
    const info = pages[from.page - 1];
    const to = slotPoint(from.page, e.clientX, e.clientY);
    if (info === undefined || to === null) return;
    // Points are continuous (the locator rounds each edge): no pixel snapping.
    const box = clampRegion({ x: from.at.x, y: from.at.y, w: to.x - from.at.x, h: to.y - from.at.y }, info.w, info.h);
    pick = box === null ? null : { page: from.page, ...box };
  }

  function onPagePointerUp(e: PointerEvent): void {
    const from = dragFrom;
    if (from === null || e.pointerId !== from.pointer) return;
    dragFrom = null;
    dragging = false;
    if (scroller?.hasPointerCapture(e.pointerId) === true) scroller.releasePointerCapture(e.pointerId);
    const p = pick;
    if (p === null) {
      clearPick();
      return;
    }
    void finishPick(p);
  }

  /** Publish a finished box once its crop is drawn. */
  async function finishPick(p: { page: number } & Region): Promise<void> {
    const gen = ++pickGen;
    const text = regionText(p);
    const crop = await renderCrop(p);
    if (gen !== pickGen || disposed) return;
    const label = `p. ${p.page} region`;
    const sel: FileSelection = {
      kind: "file",
      path,
      startLine: null,
      endLine: null,
      text,
      fragment: `page=${p.page}&${xywhFragment(p)}`,
      label,
    };
    if (crop !== null) sel.crop = crop;
    publish(sel, "area");
    chipLabel = label;
    placeChip();
  }

  /** The text under a box, in reading order, from the rendered text layer
   *  (a span counts when its middle is inside); "" when the page has none. */
  function regionText(p: { page: number } & Region): string {
    const layer = pageSpans.get(p.page);
    const slot = scroller?.querySelector<HTMLElement>(`[data-page="${p.page}"]`);
    if (layer === undefined || slot === null || slot === undefined) return "";
    const r = slot.getBoundingClientRect();
    const x0 = r.left + p.x * scale;
    const y0 = r.top + p.y * scale;
    const x1 = x0 + p.w * scale;
    const y1 = y0 + p.h * scale;
    const parts: string[] = [];
    let chars = 0;
    for (let i = 0; i < layer.spans.length && chars < REGION_TEXT_MAX; i++) {
      const str = layer.strs[i];
      if (str === undefined || str.trim() === "") continue;
      const b = layer.spans[i].getBoundingClientRect();
      const cx = b.left + b.width / 2;
      const cy = b.top + b.height / 2;
      if (cx < x0 || cx > x1 || cy < y0 || cy > y1) continue;
      parts.push(str);
      chars += str.length + 1;
    }
    return parts.join(" ").replace(/\s+/g, " ").trim().slice(0, REGION_TEXT_MAX);
  }

  /** The box as a PNG, drawn from the page's vectors at 2× (capped). */
  async function renderCrop(p: { page: number } & Region): Promise<Blob | null> {
    const d = doc;
    if (d === null) return null;
    let page: PDFPageProxy | null = null;
    try {
      page = await d.getPage(p.page);
      if (disposed) return null;
      const size = cropSize(p, CROP_FACTOR, CROP_CAP);
      // The offsets move the box's corner to the canvas origin; the canvas
      // clips the rest of the page away.
      const viewport = page.getViewport({
        scale: size.scale,
        offsetX: -p.x * size.scale,
        offsetY: -p.y * size.scale,
      });
      const canvas = document.createElement("canvas");
      canvas.width = size.w;
      canvas.height = size.h;
      const ctx = canvas.getContext("2d");
      if (ctx === null) return null;
      await page.render({ canvas, canvasContext: ctx, viewport }).promise;
      return await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/png"));
    } catch {
      // No pixels: the locator alone still goes.
      return null;
    } finally {
      page?.cleanup();
    }
  }

  // --- zoom ------------------------------------------------------------------------

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
    if (scroller !== null) scrollY = scroller.scrollTop;
    schedulePlaceChip();
  }

  const zoomPct = $derived(Math.round(scale * 100));

  // Persist zoom changes made via the buttons (scroll events cover panning).
  $effect(() => {
    void zoom;
    if (restored && scroller !== null) saveMemory();
  });
</script>

{#snippet outlineItems(nodes: OutlineNode[], prefix: string, depth: number)}
  {#each nodes as node, i (i)}
    {@const key = prefix === "" ? String(i) : `${prefix}.${i}`}
    {@const open = outlineExpanded.has(key)}
    <li>
      <div class="ol-row" style:padding-left={`${0.35 + depth * 0.85}rem`}>
        {#if node.items.length > 0}
          <button
            class="ol-twist"
            class:open
            aria-label={open ? "collapse" : "expand"}
            aria-expanded={open}
            onclick={() => toggleOutlineNode(key)}
          >
            <svg viewBox="0 0 16 16" width="10" height="10" aria-hidden="true"
              ><path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg
            >
          </button>
        {:else}
          <span class="ol-twist-pad"></span>
        {/if}
        <button
          class="ol-title"
          class:bold={node.bold}
          class:italic={node.italic}
          title={node.url ?? node.title}
          onclick={(e) => openOutlineNode(node, e)}>{node.title || "(untitled)"}</button
        >
      </div>
      {#if open && node.items.length > 0}
        <ul>
          {@render outlineItems(node.items, key, depth + 1)}
        </ul>
      {/if}
    </li>
  {/each}
{/snippet}

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="pdf-view" class:area={areaMode} onkeydown={onViewKey}>
  <div class="pdf-bar">
    {#if outline.length > 0}
      <button
        class="zbtn ic"
        class:on={outlineOpen}
        onclick={toggleOutline}
        aria-label="outline"
        aria-pressed={outlineOpen}
        title="outline"
      >
        <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"
          ><path
            d="M3 4h10M5.5 8H13M5.5 12H13"
            fill="none"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linecap="round"
          /></svg
        >
      </button>
    {/if}
    {#if numPages > 0 && pages.length > 0}
      <span class="pages">
        <input
          class="page-input"
          type="text"
          inputmode="numeric"
          aria-label="page (enter a number to jump)"
          title="page — type a number and press Enter to jump"
          size={Math.max(String(numPages).length, 1)}
          value={pageDraft ?? String(currentPage)}
          onfocus={(e) => (e.currentTarget as HTMLInputElement).select()}
          oninput={(e) => (pageDraft = (e.currentTarget as HTMLInputElement).value)}
          onkeydown={onPageKey}
          onblur={() => (pageDraft = null)}
        />
        <span class="page-total">/ {numPages}</span>
      </span>
    {:else}
      <span class="pages dim">—</span>
    {/if}
    {#if selectionLimited}
      <span
        class="selection-note"
        title="selectable text was disabled on a complex page to keep this preview responsive"
        >selection limited</span
      >
    {/if}
    {#if pick !== null && !dragging}
      <span class="selection-note region-note" title="the area you pointed at, in PDF points">
        area on p. {pick.page} · {Math.round(pick.w)}×{Math.round(pick.h)} pt
        <button class="zbtn ic sm" aria-label="clear the area" title="clear the area (Esc)" onclick={clearPick}
          >×</button
        >
      </span>
    {/if}
    {#if region !== null}
      <span class="selection-note region-note">
        region on p. {region.page}
        <button class="zbtn ic sm" aria-label="clear the region" title="clear the region" onclick={() => (region = null)}
          >×</button
        >
      </span>
    {/if}
    <span class="spacer"></span>
    <button
      class="zbtn ic"
      class:on={areaMode}
      onclick={toggleArea}
      aria-label="select an area"
      aria-pressed={areaMode}
      title={areaMode
        ? "drag a box on a page to reference it in an agent — click to stop (Esc)"
        : "select an area to reference in an agent (or Shift-drag)"}
    >
      <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"
        ><path
          d="M2.5 5V3.5a1 1 0 0 1 1-1H5M11 2.5h1.5a1 1 0 0 1 1 1V5M13.5 11v1.5a1 1 0 0 1-1 1H11M5 13.5H3.5a1 1 0 0 1-1-1V11M7.2 2.5h1.6M7.2 13.5h1.6M2.5 7.2v1.6M13.5 7.2v1.6"
          fill="none"
          stroke="currentColor"
          stroke-width="1.3"
          stroke-linecap="round"
        /></svg
      >
    </button>
    <button
      class="zbtn ic"
      class:on={findOpen}
      onclick={() => (findOpen ? closeFind() : openFind())}
      aria-label="find"
      title="find (⌘/Ctrl+F)"
    >
      <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"
        ><circle cx="7" cy="7" r="4.2" fill="none" stroke="currentColor" stroke-width="1.4" /><path
          d="M10.2 10.2L13.5 13.5"
          stroke="currentColor"
          stroke-width="1.4"
          stroke-linecap="round"
        /></svg
      >
    </button>
    <div class="zoom">
      <button class="zbtn" class:on={zoom === "fit"} onclick={() => (zoom = "fit")} title="fit width">fit</button>
      <button class="zbtn" class:on={zoom === 1} onclick={() => (zoom = 1)} title="actual size">100%</button>
      <button class="zbtn ic" onclick={zoomOut} aria-label="zoom out" title="zoom out">−</button>
      <span class="zpct">{zoomPct}%</span>
      <button class="zbtn ic" onclick={zoomIn} aria-label="zoom in" title="zoom in">+</button>
    </div>
  </div>

  {#if findOpen}
    <div class="find-bar" role="search">
      <input
        bind:this={findInput}
        class="find-input"
        type="text"
        placeholder="find in document"
        aria-label="find in document"
        spellcheck="false"
        value={findQuery}
        oninput={onFindInput}
        onkeydown={onFindKey}
      />
      <span class="find-status" aria-live="polite">{findStatus}</span>
      {#if unsearched > 0}
        <span
          class="find-note"
          title="these pages carry more text items than the viewer reads (plots that draw every point as text), so they were not searched"
          >{unsearched} {unsearched === 1 ? "page" : "pages"} not searched</span
        >
      {/if}
      {#if matchesCapped}
        <span class="find-note">first {MAX_MATCHES.toLocaleString("en-US")} shown</span>
      {/if}
      <span class="spacer"></span>
      <button class="zbtn ic" onclick={() => step(-1)} aria-label="previous match" title="previous (Shift+Enter)"
        >↑</button
      >
      <button class="zbtn ic" onclick={() => step(1)} aria-label="next match" title="next (Enter)">↓</button>
      <button class="zbtn ic" onclick={closeFind} aria-label="close find" title="close (Esc)">×</button>
    </div>
  {/if}

  <div class="pdf-body" bind:this={body}>
    {#if chipPos !== null}
      <ReferenceChip x={chipPos.x} y={chipPos.y} label={chipLabel} />
    {/if}
    {#if outlineOpen && outline.length > 0}
      <nav class="pdf-outline" aria-label="document outline">
        <ul>
          {@render outlineItems(outline, "", 0)}
        </ul>
      </nav>
    {/if}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div
      class="pdf-scroll"
      bind:this={scroller}
      onwheel={onWheel}
      onscroll={onScroll}
      onpointerdown={onPagePointerDown}
      onpointermove={onPagePointerMove}
      onpointerup={onPagePointerUp}
      onpointercancel={onPagePointerUp}
      tabindex="0"
      role="document"
      aria-label="PDF pages"
    >
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
            style:--total-scale-factor={scale}
          >
            {#if region !== null && region.page === p.num}
              {#key regionFlash}
                <div
                  class="pdf-region"
                  aria-hidden="true"
                  style:left={`${region.x * scale}px`}
                  style:top={`${region.y * scale}px`}
                  style:width={`${region.w * scale}px`}
                  style:height={`${region.h * scale}px`}
                ></div>
              {/key}
            {/if}
            {#if pick !== null && pick.page === p.num}
              <div
                class="pdf-pick"
                class:drawing={dragging}
                aria-hidden="true"
                style:left={`${pick.x * scale}px`}
                style:top={`${pick.y * scale}px`}
                style:width={`${pick.w * scale}px`}
                style:height={`${pick.h * scale}px`}
              ></div>
            {/if}
          </div>
        {/each}
      {/if}
    </div>
  </div>
</div>

<style>
  .pdf-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .pdf-bar,
  .find-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 26px;
    padding: 0 0.7rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }

  .find-bar {
    gap: 0.45rem;
    padding-left: 0.5rem;
  }

  .find-input {
    appearance: none;
    width: min(22rem, 45%);
    min-width: 8rem;
    height: 19px;
    padding: 0 0.45rem;
    border: 1px solid var(--edge);
    border-radius: 4px;
    background: var(--term-bg);
    font: inherit;
    color: var(--fg);
  }

  .find-input:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }

  .find-status {
    font-variant-numeric: tabular-nums;
    color: var(--fg);
  }

  .find-note {
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .pages {
    display: inline-flex;
    align-items: center;
    gap: 0.3em;
    font-variant-numeric: tabular-nums;
  }

  .page-input {
    appearance: none;
    box-sizing: content-box;
    min-width: 1.4ch;
    padding: 0 0.3em;
    border: 1px solid transparent;
    border-radius: 4px;
    background: none;
    font: inherit;
    font-variant-numeric: tabular-nums;
    color: var(--fg);
    text-align: right;
  }

  .page-input:hover {
    border-color: var(--edge);
  }

  .page-input:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: var(--term-bg);
  }

  .pages.dim {
    opacity: 0.6;
  }

  .selection-note {
    padding-left: 0.55rem;
    border-left: 1px solid var(--edge);
    color: var(--muted);
  }

  .region-note {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    color: var(--fg);
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
    font-size: var(--text-xs);
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
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 20px;
    height: 20px;
    padding: 0 0.3rem;
    font-size: var(--text-lg);
    line-height: 1;
  }

  .zbtn.ic.sm {
    min-width: 16px;
    height: 16px;
    font-size: var(--text-sm);
  }

  .zpct {
    min-width: 3.2ch;
    text-align: center;
    font-variant-numeric: tabular-nums;
  }

  .pdf-body {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
  }

  /* The area tool: the page takes a crosshair, and its text and link layers
     step aside so a drag draws a box instead of selecting or following. */
  .pdf-view.area .pdf-slot {
    cursor: crosshair;
  }

  .pdf-view.area .pdf-slot :global(.textLayer),
  .pdf-view.area .pdf-slot :global(.annotationLayer) {
    pointer-events: none;
  }

  .pdf-pick {
    position: absolute;
    z-index: 4;
    box-sizing: border-box;
    pointer-events: none;
    border: 1.5px solid var(--accent);
    border-radius: 2px;
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    /* A light halo keeps the edge readable on dark figures. */
    box-shadow: 0 0 0 1px rgba(255, 255, 255, 0.7);
  }

  .pdf-pick.drawing {
    border-style: dashed;
    background: color-mix(in srgb, var(--accent) 6%, transparent);
  }

  .pdf-outline {
    flex: none;
    width: clamp(160px, 26%, 280px);
    overflow: auto;
    padding: 0.35rem 0;
    border-right: 1px solid var(--edge);
    background: var(--term-bg);
    font-size: var(--text-sm);
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }

  .pdf-outline ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .ol-row {
    display: flex;
    align-items: center;
    gap: 1px;
    padding-right: 0.4rem;
  }

  .ol-twist,
  .ol-twist-pad {
    flex: none;
    width: 16px;
    height: 20px;
  }

  .ol-twist {
    appearance: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    background: none;
    padding: 0;
    color: var(--muted);
    cursor: pointer;
    border-radius: 3px;
  }

  .ol-twist svg {
    transition: transform 0.12s ease;
  }

  .ol-twist.open svg {
    transform: rotate(90deg);
  }

  .ol-twist:hover {
    color: var(--fg);
  }

  .ol-title {
    appearance: none;
    flex: 1;
    min-width: 0;
    border: none;
    background: none;
    padding: 0.15rem 0.35rem;
    border-radius: 4px;
    font: inherit;
    color: var(--fg);
    text-align: left;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    cursor: pointer;
  }

  .ol-title:hover {
    background: var(--row-hover);
  }

  .ol-title:focus-visible,
  .ol-twist:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  .ol-title.bold {
    font-weight: 600;
  }

  .ol-title.italic {
    font-style: italic;
  }

  .pdf-scroll {
    flex: 1;
    min-width: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    padding: 14px 0;
    outline: none;
    background: color-mix(in srgb, var(--fg) 4%, var(--term-bg));
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }

  .pdf-slot {
    flex: none;
    position: relative;
    /* A PDF page is paper: white in every theme. */
    background: #fff;
    box-shadow: 0 1px 6px rgba(0, 0, 0, 0.18);
    border-radius: 2px;
    overflow: hidden;
    --scale-round-x: 1px;
    --scale-round-y: 1px;
  }

  .pdf-slot :global(.pdf-canvas) {
    display: block;
  }

  /* pdf.js text layer (its viewer CSS, trimmed): transparent spans sized from
     --total-scale-factor, so selection and find highlights sit on the glyphs. */
  .pdf-slot :global(.textLayer) {
    position: absolute;
    inset: 0;
    text-align: initial;
    overflow: clip;
    opacity: 1;
    line-height: 1;
    letter-spacing: normal;
    word-spacing: normal;
    text-size-adjust: none;
    forced-color-adjust: none;
    transform-origin: 0 0;
    caret-color: CanvasText;
    z-index: 1;
    --min-font-size: 1;
    --text-scale-factor: calc(var(--total-scale-factor) * var(--min-font-size));
    --min-font-size-inv: calc(1 / var(--min-font-size));
  }

  .pdf-slot :global(.textLayer span),
  .pdf-slot :global(.textLayer br) {
    color: transparent;
    position: absolute;
    white-space: pre;
    cursor: text;
    transform-origin: 0% 0%;
  }

  .pdf-slot :global(.textLayer > :not(.markedContent)),
  .pdf-slot :global(.textLayer .markedContent span:not(.markedContent)) {
    z-index: 1;
    --font-height: 0;
    font-size: calc(var(--text-scale-factor) * var(--font-height));
    --scale-x: 1;
    --rotate: 0deg;
    transform: rotate(var(--rotate)) scaleX(var(--scale-x)) scale(var(--min-font-size-inv));
  }

  .pdf-slot :global(.textLayer .markedContent) {
    display: contents;
  }

  .pdf-slot :global(.textLayer span.highlight) {
    position: static;
    margin: -1px;
    padding: 1px;
    border-radius: 3px;
    font-size: inherit;
    transform: none;
    background: color-mix(in srgb, var(--accent) 28%, transparent);
  }

  .pdf-slot :global(.textLayer span.highlight.selected) {
    background: color-mix(in srgb, var(--accent) 55%, transparent);
    box-shadow: 0 0 0 1px var(--accent);
  }

  .pdf-slot :global(.textLayer ::selection) {
    background: rgba(80, 140, 220, 0.35);
  }

  /* pdf.js link layer: absolutely placed sections over the canvas, one
     anchor filling each. */
  .pdf-slot :global(.annotationLayer) {
    position: absolute;
    top: 0;
    left: 0;
    pointer-events: none;
    transform-origin: 0 0;
    z-index: 2;
  }

  .pdf-slot :global(.annotationLayer section) {
    position: absolute;
    text-align: initial;
    pointer-events: auto;
    box-sizing: border-box;
    transform-origin: 0 0;
  }

  .pdf-slot :global(.annotationLayer .linkAnnotation > a) {
    position: absolute;
    inset: 0;
    font-size: 1em;
    border-radius: 2px;
  }

  .pdf-slot :global(.annotationLayer .linkAnnotation > a:hover) {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent) 50%, transparent);
  }

  .pdf-region {
    position: absolute;
    z-index: 3;
    box-sizing: border-box;
    pointer-events: none;
    border: 2px solid var(--accent);
    border-radius: 2px;
    background: color-mix(in srgb, var(--accent) 8%, transparent);
    animation: region-in 1.6s ease-out;
  }

  @keyframes region-in {
    0%,
    40% {
      background: color-mix(in srgb, var(--accent) 26%, transparent);
      box-shadow: 0 0 0 4px color-mix(in srgb, var(--accent) 30%, transparent);
    }
  }

  .file-error,
  .file-loading {
    margin: auto;
    color: var(--muted);
    font-size: var(--text-md);
    padding: 1rem;
    text-align: center;
  }

  @media (prefers-reduced-motion: reduce) {
    .zbtn,
    .ol-twist svg {
      transition: none;
    }

    .pdf-region {
      animation: none;
    }
  }
</style>
