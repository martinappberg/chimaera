<script module lang="ts">
  /** Per-file pan/zoom memory, so a tab switch comes back where it was. */
  const memory = new Map<string, { fit: boolean; scale: number; tx: number; ty: number; page: number }>();
</script>

<script lang="ts">
  /**
   * Diagram boards — JSON Canvas, Excalidraw, draw.io — on one pannable,
   * zoomable surface: drag or scroll to pan, pinch or Cmd/Ctrl+scroll to zoom
   * at the pointer, fit and 1:1 in the bar (keys: 0 fit, 1 actual size, +/−,
   * arrows). Each format parses in its own lazy module (`boards/`); the file
   * is read once through a `/raw` ticket and re-read in place when it changes
   * on disk, keeping the view. Excalidraw and draw.io drawings assume white
   * paper, so in a dark theme they're shown the way Excalidraw's own dark
   * mode does it (inverted, hues kept, images restored), with a switch back to
   * their original colors; they export as SVG or a 2× PNG in their original
   * colors. JSON Canvas is drawn with the theme's own tokens. Nothing a board
   * names is fetched from the network.
   */
  import { untrack, type Component, type Snippet } from "svelte";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import { fetchRawBytes, saveBlob, TooLargeError } from "./rawBytes";
  import { activeTheme } from "../settings/store.svelte";
  import { boardFormat, boardStem, fitView, zoomAround, type Rect } from "./boards/format";
  import type { CanvasDoc } from "./boards/canvas";
  import type { DrawioPage } from "./boards/drawio";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
    wsRoot?: string | null;
    /** The host's view switch (board ⇄ source), drawn at the bar's end. */
    switcher?: Snippet;
  }

  let { path, wsRoot = null, switcher }: Props = $props();

  const format = $derived(boardFormat(path));
  const CAPS = { canvas: 16, excalidraw: 50, drawio: 16 } as const;

  interface SvgScene {
    kind: "svg";
    svg: SVGSVGElement;
    bounds: Rect;
    notes: string[];
  }
  interface CanvasScene {
    kind: "canvas";
    doc: CanvasDoc;
    bounds: Rect;
    notes: string[];
  }
  type Scene = SvgScene | CanvasScene;

  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const e = retain(path);
    entry = e;
    void e.ensureMtime();
    return () => release(path);
  });

  let scene = $state.raw<Scene | null>(null);
  let error = $state<string | null>(null);
  let pages = $state.raw<DrawioPage[]>([]);
  let page = $state(untrack(() => memory.get(path)?.page ?? 0));
  let CanvasBoard = $state<Component<{ doc: CanvasDoc; path: string; wsRoot?: string | null }> | null>(null);

  async function load(p: string): Promise<{ text: string }> {
    const fmt = boardFormat(p);
    const cap = (fmt === null ? 16 : CAPS[fmt]) * 1024 * 1024;
    const bytes = await fetchRawBytes(p, cap);
    return { text: new TextDecoder().decode(bytes) };
  }

  async function build(text: string, pageIndex: number): Promise<{ scene: Scene; pages: DrawioPage[] }> {
    switch (format) {
      case "canvas": {
        const [{ parseCanvas }, comp] = await Promise.all([import("./boards/canvas"), import("./boards/CanvasBoard.svelte")]);
        CanvasBoard ??= comp.default;
        const doc = parseCanvas(text);
        const notes = doc.dropped > 0 ? [`${doc.dropped} malformed ${doc.dropped === 1 ? "entry" : "entries"} skipped`] : [];
        if (doc.nodes.length === 0) notes.push("this canvas is empty");
        return { scene: { kind: "canvas", doc, bounds: doc.bounds, notes }, pages: [] };
      }
      case "excalidraw": {
        const { parseExcalidraw, renderExcalidraw } = await import("./boards/excalidraw");
        const ex = parseExcalidraw(text);
        const svg = renderExcalidraw(ex);
        const notes: string[] = [];
        if (ex.skipped > 0) notes.push(`${ex.skipped} ${ex.skipped === 1 ? "element" : "elements"} of an unknown kind not drawn`);
        if (ex.elements.length === 0) notes.push("this drawing is empty");
        return { scene: { kind: "svg", svg, bounds: ex.bounds, notes }, pages: [] };
      }
      case "drawio": {
        const [{ readDrawio, parseModel }, { renderDrawio }] = await Promise.all([
          import("./boards/drawio"),
          import("./boards/drawioSvg"),
        ]);
        const all = await readDrawio(text);
        const i = Math.min(Math.max(0, pageIndex), all.length - 1);
        const pg = all[i];
        if (pg.model === null) throw new Error(pg.error ?? "this page couldn't be read");
        const out = renderDrawio(parseModel(pg.model));
        const notes: string[] = [];
        if (out.approximated > 0) {
          notes.push(`${out.approximated} ${out.approximated === 1 ? "shape" : "shapes"} drawn as a box (a stencil this viewer doesn't know)`);
        }
        if (out.blocked > 0) notes.push(`${out.blocked} linked ${out.blocked === 1 ? "image" : "images"} not loaded`);
        return { scene: { kind: "svg", svg: out.svg, bounds: out.bounds, notes }, pages: all };
      }
      default:
        throw new Error("not a board format");
    }
  }

  // (Re)build on open, on every on-disk version, and on a page switch. The
  // last drawing stays up until the next is ready.
  let text: string | null = null;
  let seen: string | null | undefined;
  let gen = 0;
  function rebuild(reread: boolean): void {
    const mine = ++gen;
    const want = page;
    void (async () => {
      if (reread || text === null) text = (await load(path)).text;
      return build(text, want);
    })().then(
      (res) => {
        if (mine !== gen) return;
        const first = scene === null;
        scene = res.scene;
        pages = res.pages;
        error = null;
        if (first) restoreView();
        else if (fitMode) applyFit();
      },
      (e: unknown) => {
        if (mine !== gen) return;
        error = e instanceof TooLargeError || e instanceof Error ? e.message : "the board could not be drawn";
      },
    );
  }

  $effect(() => {
    const m = entry?.mtime ?? null;
    if (seen !== undefined && (m === null || m === seen || seen === null)) {
      if (m !== null) seen = m;
      return;
    }
    seen = m;
    untrack(() => rebuild(true));
  });

  function showPage(i: number): void {
    if (i === page) return;
    page = i;
    fitMode = true;
    rebuild(false);
  }

  // --- the surface ----------------------------------------------------------------

  let viewport = $state<HTMLDivElement | null>(null);
  let vw = $state(0);
  let vh = $state(0);
  let scale = $state(1);
  let tx = $state(0);
  let ty = $state(0);
  let fitMode = $state(true);

  $effect(() => {
    const el = viewport;
    if (el === null) return;
    const ro = new ResizeObserver(() => {
      vw = el.clientWidth;
      vh = el.clientHeight;
      if (untrack(() => fitMode)) applyFit();
    });
    ro.observe(el);
    vw = el.clientWidth;
    vh = el.clientHeight;
    return () => ro.disconnect();
  });

  function applyFit(): void {
    const s = scene;
    if (s === null || vw === 0 || vh === 0) return;
    const v = fitView(s.bounds, vw, vh, 24, 1);
    scale = v.scale;
    tx = v.tx;
    ty = v.ty;
    fitMode = true;
  }

  function restoreView(): void {
    const m = memory.get(path);
    if (m !== undefined && !m.fit) {
      scale = m.scale;
      tx = m.tx;
      ty = m.ty;
      fitMode = false;
    } else applyFit();
  }

  $effect(() => {
    if (scene === null) return;
    memory.set(path, { fit: fitMode, scale, tx, ty, page });
  });

  function zoomAt(factor: number, cx: number, cy: number): void {
    const v = zoomAround({ scale, tx, ty }, factor, cx, cy, 0.02, 8);
    scale = v.scale;
    tx = v.tx;
    ty = v.ty;
    fitMode = false;
  }

  function zoomStep(dir: 1 | -1): void {
    zoomAt(dir > 0 ? 1.25 : 0.8, vw / 2, vh / 2);
  }

  function actualSize(cx = vw / 2, cy = vh / 2): void {
    zoomAt(1 / scale, cx, cy);
  }

  function local(e: { clientX: number; clientY: number }): [number, number] {
    const r = viewport?.getBoundingClientRect();
    return r === undefined ? [0, 0] : [e.clientX - r.left, e.clientY - r.top];
  }

  function canScroll(el: Element, dx: number, dy: number): boolean {
    const box = el.closest<HTMLElement>(".cv-scroll");
    if (box === null) return false;
    if (dy !== 0 && box.scrollHeight > box.clientHeight + 1) {
      return dy < 0 ? box.scrollTop > 0 : box.scrollTop + box.clientHeight < box.scrollHeight - 1;
    }
    if (dx !== 0 && box.scrollWidth > box.clientWidth + 1) {
      return dx < 0 ? box.scrollLeft > 0 : box.scrollLeft + box.clientWidth < box.scrollWidth - 1;
    }
    return false;
  }

  function onWheel(e: WheelEvent): void {
    if (scene === null) return;
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      const [cx, cy] = local(e);
      const d = Math.max(-60, Math.min(60, e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY));
      zoomAt(Math.exp(-d * 0.01), cx, cy);
      return;
    }
    if (e.target instanceof Element && canScroll(e.target, e.deltaX, e.deltaY)) return;
    e.preventDefault();
    const k = e.deltaMode === 1 ? 16 : 1;
    tx -= e.deltaX * k;
    ty -= e.deltaY * k;
    fitMode = false;
  }

  // Drag pans once the pointer has moved a few pixels, so a click on a card
  // stays a click; a text card double-clicked into "selecting" lets a drag
  // select its text instead.
  let press: { id: number; x: number; y: number; tx: number; ty: number; panning: boolean } | null = null;
  let dragging = $state(false);
  let swallowClick = false;

  function onPointerDown(e: PointerEvent): void {
    if ((e.button !== 0 && e.button !== 1) || scene === null) return;
    const t = e.target as Element | null;
    if (t?.closest(".cv-card.selecting") != null) return;
    viewport?.querySelectorAll(".cv-card.selecting").forEach((c) => c.classList.remove("selecting"));
    press = { id: e.pointerId, x: e.clientX, y: e.clientY, tx, ty, panning: false };
    if (e.button === 1) e.preventDefault();
  }

  function onPointerMove(e: PointerEvent): void {
    const p = press;
    if (p === null || e.pointerId !== p.id) return;
    const dx = e.clientX - p.x;
    const dy = e.clientY - p.y;
    if (!p.panning) {
      if (Math.hypot(dx, dy) < 4) return;
      p.panning = true;
      dragging = true;
      viewport?.setPointerCapture(e.pointerId);
      window.getSelection()?.removeAllRanges();
    }
    tx = p.tx + dx;
    ty = p.ty + dy;
    fitMode = false;
  }

  function onPointerUp(e: PointerEvent): void {
    const p = press;
    if (p === null || e.pointerId !== p.id) return;
    press = null;
    if (p.panning) {
      dragging = false;
      swallowClick = true;
      setTimeout(() => (swallowClick = false), 0);
      if (viewport?.hasPointerCapture(e.pointerId)) viewport.releasePointerCapture(e.pointerId);
    }
  }

  function onClickCapture(e: MouseEvent): void {
    if (!swallowClick) return;
    e.stopPropagation();
    e.preventDefault();
    swallowClick = false;
  }

  function onDblClick(e: MouseEvent): void {
    const card = (e.target as Element | null)?.closest?.(".cv-card.kind-text");
    if (card !== null && card !== undefined) {
      card.classList.add("selecting");
      return;
    }
    const [cx, cy] = local(e);
    if (fitMode) actualSize(cx, cy);
    else applyFit();
  }

  function onKey(e: KeyboardEvent): void {
    if (e.metaKey || e.ctrlKey || e.altKey || scene === null) return;
    const step = 80;
    switch (e.key) {
      case "0":
        applyFit();
        break;
      case "1":
        actualSize();
        break;
      case "+":
      case "=":
        zoomStep(1);
        break;
      case "-":
        zoomStep(-1);
        break;
      case "ArrowLeft":
        tx += step;
        fitMode = false;
        break;
      case "ArrowRight":
        tx -= step;
        fitMode = false;
        break;
      case "ArrowUp":
        ty += step;
        fitMode = false;
        break;
      case "ArrowDown":
        ty -= step;
        fitMode = false;
        break;
      default:
        return;
    }
    e.preventDefault();
  }

  // --- the drawing ------------------------------------------------------------------

  const dark = $derived(activeTheme().kind === "dark");
  /** In a dark theme, drawings are adapted unless the reader asks for the
   *  original colors. */
  let original = $state(false);
  const adapt = $derived(dark && !original && scene?.kind === "svg");

  function mountSvg(node: HTMLElement, svg: SVGSVGElement) {
    node.replaceChildren(svg);
    return {
      update(next: SVGSVGElement) {
        if (node.firstChild !== next) node.replaceChildren(next);
      },
    };
  }

  // --- export ---------------------------------------------------------------------

  let exportNote = $state<string | null>(null);
  let noteTimer: ReturnType<typeof setTimeout> | null = null;
  function note(t: string): void {
    exportNote = t;
    if (noteTimer !== null) clearTimeout(noteTimer);
    noteTimer = setTimeout(() => (exportNote = null), 3500);
  }
  $effect(() => () => {
    if (noteTimer !== null) clearTimeout(noteTimer);
  });

  function svgText(): string | null {
    const s = scene;
    if (s?.kind !== "svg") return null;
    const clone = s.svg.cloneNode(true) as SVGSVGElement;
    return `<?xml version="1.0" encoding="UTF-8"?>\n${new XMLSerializer().serializeToString(clone)}`;
  }

  function exportName(ext: string): string {
    const pageName = pages.length > 1 ? `-${(pages[page]?.name ?? String(page + 1)).replace(/[^\w.-]+/g, "_")}` : "";
    return `${boardStem(path)}${pageName}.${ext}`;
  }

  function exportSvg(): void {
    const doc = svgText();
    if (doc !== null) saveBlob(new Blob([doc], { type: "image/svg+xml" }), exportName("svg"));
  }

  async function exportPng(): Promise<void> {
    const doc = svgText();
    const s = scene;
    if (doc === null || s === null) return;
    const k = Math.min(2, 16384 / Math.max(s.bounds.w, s.bounds.h));
    const img = new Image();
    img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(doc)}`;
    try {
      await img.decode();
      const canvas = document.createElement("canvas");
      canvas.width = Math.max(1, Math.ceil(s.bounds.w * k));
      canvas.height = Math.max(1, Math.ceil(s.bounds.h * k));
      const ctx = canvas.getContext("2d");
      if (ctx === null) throw new Error("no canvas");
      ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
      const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/png"));
      if (blob === null) throw new Error("no image");
      saveBlob(blob, exportName("png"));
    } catch {
      // HTML labels (draw.io) taint the canvas in some engines.
      note("PNG export isn't available for this drawing here — export SVG instead");
    }
  }

  const pct = $derived(Math.round(scale * 100));
</script>

<div class="board-view">
  <div class="board-bar">
    {#if scene !== null}
      <button class="bbtn icon" onclick={() => zoomStep(-1)} title="zoom out (−)" aria-label="zoom out">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M3.5 8h9" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg>
      </button>
      <button class="bbtn zoom" class:on={fitMode} onclick={applyFit} title="fit the board to the pane (0)">{fitMode ? "fit" : `${pct}%`}</button>
      <button class="bbtn icon" onclick={() => zoomStep(1)} title="zoom in (+)" aria-label="zoom in">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M3.5 8h9M8 3.5v9" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg>
      </button>
      <button class="bbtn" class:on={!fitMode && Math.abs(scale - 1) < 0.001} onclick={() => actualSize()} title="actual size (1)">1:1</button>
    {/if}
    {#if pages.length > 1}
      <div class="pages" role="tablist" aria-label="pages">
        {#each pages as pg, i (i)}
          <button class="page" class:on={i === page} role="tab" aria-selected={i === page} title={pg.name} onclick={() => showPage(i)}>{pg.name}</button>
        {/each}
      </div>
    {/if}
    {#if exportNote !== null}
      <span class="bar-note" role="status">{exportNote}</span>
    {:else if scene !== null && scene.notes.length > 0}
      <span class="bar-note" title={scene.notes.join("; ")}>{scene.notes.join(" · ")}</span>
    {/if}
    <span class="spacer"></span>
    {#if scene?.kind === "svg"}
      {#if dark}
        <button class="bbtn" class:on={original} onclick={() => (original = !original)} title="show the drawing in its own colors instead of adapted to the dark theme"
          >original colors</button
        >
      {/if}
      <button class="bbtn" onclick={exportSvg} title="download the drawing as SVG (its own colors)">svg</button>
      <button class="bbtn" onclick={() => void exportPng()} title="download the drawing as a 2× PNG (its own colors)">png</button>
    {/if}
    {@render switcher?.()}
  </div>

  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="surface"
    class:dragging
    bind:this={viewport}
    tabindex="0"
    role="application"
    aria-label="board: drag or scroll to pan, pinch or Ctrl+scroll to zoom"
    onwheel={onWheel}
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={onPointerUp}
    onclickcapture={onClickCapture}
    ondblclick={onDblClick}
    onkeydown={onKey}
  >
    {#if scene !== null}
      <div class="world" style:transform="translate({tx}px, {ty}px) scale({scale})">
        <div class="content" style:left="{scene.bounds.x}px" style:top="{scene.bounds.y}px">
          {#if scene.kind === "svg"}
            <div class="drawing" class:adapt use:mountSvg={scene.svg}></div>
          {:else if CanvasBoard !== null}
            <CanvasBoard doc={scene.doc} {path} {wsRoot} />
          {/if}
        </div>
      </div>
    {/if}
    {#if error !== null && scene === null}
      <div class="board-msg">{error}</div>
    {:else if scene === null}
      <Spinner label="drawing" />
    {/if}
    {#if error !== null && scene !== null}
      <div class="stale-note" role="status">{error} — showing the last good drawing</div>
    {/if}
  </div>
</div>

<style>
  .board-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .board-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    height: 26px;
    padding: 0 0.5rem 0 0.4rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    min-width: 0;
  }

  .spacer {
    flex: 1;
  }

  .bar-note {
    min-width: 0;
    margin-left: 0.4rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .bbtn,
  .page {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.45rem;
    border-radius: 4px;
    white-space: nowrap;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .bbtn.icon {
    display: inline-flex;
    align-items: center;
    padding: 0.2rem 0.3rem;
  }

  .bbtn.zoom {
    min-width: 4.5ch;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
  }

  .bbtn:hover,
  .page:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .bbtn.on {
    color: var(--fg);
  }

  .pages {
    display: flex;
    gap: 1px;
    margin-left: 0.5rem;
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
  }

  .page {
    max-width: 16ch;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .page.on {
    color: var(--fg);
    background: var(--row-active);
  }

  .surface {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: hidden;
    outline: none;
    cursor: grab;
    touch-action: none;
    background-color: var(--term-bg);
    /* A quiet dot grid, like a canvas: tells a pannable surface from a page. */
    background-image: radial-gradient(color-mix(in srgb, var(--fg) 12%, transparent) 1px, transparent 1px);
    background-size: 22px 22px;
  }

  .surface:focus-visible {
    box-shadow: inset 0 0 0 1px var(--focus-ring);
  }

  .surface.dragging {
    cursor: grabbing;
    user-select: none;
  }

  .world {
    position: absolute;
    left: 0;
    top: 0;
    transform-origin: 0 0;
  }

  .content {
    position: absolute;
  }

  .drawing :global(svg) {
    display: block;
    overflow: visible;
  }

  /* Default white paper is left off on screen, so the drawing sits on the
     surface; exports keep it (it's in the SVG, only hidden here). */
  .drawing :global(.ex-paper[fill="#ffffff"]),
  .drawing :global(.dio-paper[fill="#ffffff"]) {
    display: none;
  }

  /* Excalidraw's own dark mode: invert, turn the hues back, and restore the
     photos inside. */
  .drawing.adapt {
    filter: invert(93%) hue-rotate(180deg);
  }

  .drawing.adapt :global(.ex-img),
  .drawing.adapt :global(.dio-img) {
    filter: invert(100%) hue-rotate(180deg) saturate(1.25);
  }

  .surface :global(.cv-card.selecting) {
    cursor: text;
    user-select: text;
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 35%, transparent);
  }

  .surface :global(.cv-card:not(.selecting)) {
    user-select: none;
  }

  .board-msg {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1rem;
    color: var(--muted);
    font-size: var(--text-md);
    text-align: center;
    background: var(--term-bg);
  }

  .stale-note {
    position: absolute;
    left: 50%;
    bottom: 10px;
    transform: translateX(-50%);
    max-width: calc(100% - 20px);
    padding: 0.25rem 0.6rem;
    border: 1px solid color-mix(in srgb, var(--warn) 40%, var(--edge));
    border-radius: 6px;
    background: var(--overlay-bg);
    color: var(--warn);
    font-size: var(--text-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  @media (prefers-reduced-motion: reduce) {
    .bbtn,
    .page {
      transition: none;
    }
  }
</style>
