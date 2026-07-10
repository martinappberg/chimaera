<script module lang="ts">
  /** Per-tab zoom/pan memory: switching away and back restores the view.
   * Module-scoped so it survives the unmount/remount of a tab switch. */
  interface ViewMem {
    mode: "fit" | "free";
    scale: number;
    tx: number;
    ty: number;
  }
  const memory = new Map<string, ViewMem>();
</script>

<script lang="ts">
  import { fsRawUrl, innerExtension } from "./files";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  const MIN_SCALE = 0.05;
  const MAX_SCALE = 64;
  /** Past this zoom a raster image gets the pixel-inspection grid. */
  const GRID_SCALE = 8;
  const PAD = 24;

  let url = $state<string | null>(null);
  let error = $state<string | null>(null);
  let natural = $state<{ w: number; h: number } | null>(null);

  let mode = $state<"fit" | "free">("fit");
  let scale = $state(1);
  let tx = $state(0);
  let ty = $state(0);

  let viewport = $state<HTMLDivElement | null>(null);
  let vw = $state(0);
  let vh = $state(0);
  let dragging = $state(false);

  const isSvg = $derived(innerExtension(path) === "svg");
  const pct = $derived(Math.round(scale * 100));
  /** Pixel grid only helps for raster pixels; SVG stays vector-crisp. */
  const showGrid = $derived(!isSvg && scale >= GRID_SCALE && natural !== null);

  $effect(() => {
    const p = path;
    url = null;
    error = null;
    natural = null;
    let stale = false;
    fsRawUrl(p)
      .then((u) => {
        if (!stale) url = u;
      })
      .catch((e) => {
        if (!stale) error = e instanceof Error ? e.message : "failed to load image";
      });
    return () => {
      stale = true;
    };
  });

  // Track the viewport size for fit-scaling and pan clamping.
  $effect(() => {
    const el = viewport;
    if (el === null) return;
    const ro = new ResizeObserver((entries) => {
      for (const e of entries) {
        vw = e.contentRect.width;
        vh = e.contentRect.height;
      }
      if (mode === "fit") applyFit();
    });
    ro.observe(el);
    vw = el.clientWidth;
    vh = el.clientHeight;
    return () => ro.disconnect();
  });

  /** Scale that fits the image inside the viewport (never upscales past 1:1). */
  function fitScale(): number {
    if (natural === null || vw === 0 || vh === 0) return 1;
    const s = Math.min((vw - PAD) / natural.w, (vh - PAD) / natural.h);
    return Math.min(Math.max(s, MIN_SCALE), 1);
  }

  function applyFit(): void {
    if (natural === null) return;
    mode = "fit";
    scale = fitScale();
    tx = (vw - natural.w * scale) / 2;
    ty = (vh - natural.h * scale) / 2;
  }

  /** Keep the image sensibly placed: center on any axis where it's smaller
   * than the viewport, otherwise clamp so an edge can't be dragged inside. */
  function clampPan(): void {
    if (natural === null) return;
    const iw = natural.w * scale;
    const ih = natural.h * scale;
    if (iw <= vw) tx = (vw - iw) / 2;
    else tx = Math.min(0, Math.max(vw - iw, tx));
    if (ih <= vh) ty = (vh - ih) / 2;
    else ty = Math.min(0, Math.max(vh - ih, ty));
  }

  /** Set an explicit scale, keeping the image point under (cx,cy) fixed. */
  function scaleAround(next: number, cx: number, cy: number): void {
    if (natural === null) return;
    const clamped = Math.min(Math.max(next, MIN_SCALE), MAX_SCALE);
    const imgX = (cx - tx) / scale;
    const imgY = (cy - ty) / scale;
    scale = clamped;
    tx = cx - imgX * clamped;
    ty = cy - imgY * clamped;
    mode = "free";
    clampPan();
  }

  function centerAt(next: number): void {
    scaleAround(next, vw / 2, vh / 2);
  }

  function onWheel(e: WheelEvent): void {
    if (natural === null) return;
    e.preventDefault();
    const rect = viewport?.getBoundingClientRect();
    if (rect === undefined) return;
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;
    // Smooth exponential zoom; trackpads deliver many small deltas.
    const factor = Math.exp(-e.deltaY * 0.0015);
    scaleAround(scale * factor, cx, cy);
  }

  function onDblClick(e: MouseEvent): void {
    if (natural === null) return;
    const rect = viewport?.getBoundingClientRect();
    // Double-click toggles fit <-> 100%, anchored at the cursor.
    const atFit = mode === "fit" || Math.abs(scale - fitScale()) < 0.001;
    if (atFit) {
      if (rect !== undefined) scaleAround(1, e.clientX - rect.left, e.clientY - rect.top);
      else centerAt(1);
    } else {
      applyFit();
    }
  }

  function onPointerDown(e: PointerEvent): void {
    if (e.button !== 0 || natural === null) return;
    dragging = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    e.preventDefault();
  }

  function onPointerMove(e: PointerEvent): void {
    if (!dragging) return;
    tx += e.movementX;
    ty += e.movementY;
    mode = "free";
    clampPan();
  }

  function onPointerUp(e: PointerEvent): void {
    if (!dragging) return;
    dragging = false;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
  }

  function zoomStep(dir: 1 | -1): void {
    // ~1.25x per press, from wherever we are, centered.
    centerAt(scale * (dir > 0 ? 1.25 : 1 / 1.25));
  }

  function onLoad(e: Event): void {
    const img = e.currentTarget as HTMLImageElement;
    natural = { w: img.naturalWidth || 1, h: img.naturalHeight || 1 };
    const remembered = memory.get(path);
    if (remembered !== undefined) {
      mode = remembered.mode;
      scale = remembered.scale;
      tx = remembered.tx;
      ty = remembered.ty;
      if (mode === "fit") applyFit();
    } else {
      applyFit();
    }
  }

  // Persist view state per tab as it changes (cheap; no network).
  $effect(() => {
    if (natural === null) return;
    memory.set(path, { mode, scale, tx, ty });
  });

  const gridStep = $derived(showGrid ? scale : 0);
</script>

<div class="image-view">
  <div class="img-bar">
    <span class="dims" class:dim={natural === null}>
      {#if natural !== null}{natural.w}×{natural.h}{isSvg ? " · svg" : ""}{:else}—{/if}
    </span>
    <span class="spacer"></span>
    <div class="zoom">
      <button class="zbtn" class:on={mode === "fit"} onclick={applyFit} title="fit to window">fit</button>
      <button
        class="zbtn"
        class:on={mode === "free" && pct === 100}
        onclick={() => centerAt(1)}
        title="actual size (1:1)">100%</button
      >
      <button class="zbtn ic" onclick={() => zoomStep(-1)} aria-label="zoom out" title="zoom out">−</button>
      <span class="zpct">{pct}%</span>
      <button class="zbtn ic" onclick={() => zoomStep(1)} aria-label="zoom in" title="zoom in">+</button>
    </div>
  </div>

  <div
    class="viewport"
    class:grabbing={dragging}
    role="application"
    aria-label="image viewer — scroll to zoom, drag to pan, double-click to toggle fit"
    bind:this={viewport}
    onwheel={onWheel}
    ondblclick={onDblClick}
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={onPointerUp}
  >
    {#if error !== null}
      <div class="file-error">{error}</div>
    {:else if url !== null}
      <!-- checkerboard shows through wherever the image is transparent -->
      <img
        src={url}
        alt={path}
        class:pixelated={!isSvg && scale > 1}
        class:measuring={natural === null}
        style:width={natural !== null ? `${natural.w * scale}px` : undefined}
        style:height={natural !== null ? `${natural.h * scale}px` : undefined}
        style:transform={natural !== null ? `translate(${tx}px, ${ty}px)` : undefined}
        onload={onLoad}
        onerror={() => (error = "image failed to load (ticket may have expired — reopen the tab)")}
        draggable="false"
      />
      {#if showGrid && natural !== null}
        <div
          class="pixel-grid"
          style:width={`${natural.w * scale}px`}
          style:height={`${natural.h * scale}px`}
          style:transform={`translate(${tx}px, ${ty}px)`}
          style:background-size={`${gridStep}px ${gridStep}px`}
        ></div>
      {/if}
    {:else}
      <Spinner />
    {/if}
  </div>
</div>

<style>
  .image-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .img-bar {
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

  .dims {
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
  }

  .dims.dim {
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
    min-width: 4ch;
    text-align: center;
    font-variant-numeric: tabular-nums;
  }

  .viewport {
    flex: 1;
    min-height: 0;
    position: relative;
    overflow: hidden;
    cursor: grab;
    /* Subtle transparency checkerboard (two-tone conic tiles). */
    --chk: color-mix(in srgb, var(--fg) 5%, transparent);
    background-color: var(--term-bg);
    background-image:
      conic-gradient(var(--chk) 25%, transparent 0 50%, var(--chk) 0 75%, transparent 0);
    background-size: 16px 16px;
    touch-action: none;
  }

  .viewport.grabbing {
    cursor: grabbing;
  }

  img {
    position: absolute;
    top: 0;
    left: 0;
    transform-origin: 0 0;
    display: block;
    user-select: none;
    -webkit-user-drag: none;
    image-rendering: auto;
  }

  img.pixelated {
    image-rendering: pixelated;
  }

  img.measuring {
    visibility: hidden;
  }

  .pixel-grid {
    position: absolute;
    top: 0;
    left: 0;
    transform-origin: 0 0;
    pointer-events: none;
    background-image:
      linear-gradient(to right, color-mix(in srgb, var(--fg) 22%, transparent) 1px, transparent 1px),
      linear-gradient(to bottom, color-mix(in srgb, var(--fg) 22%, transparent) 1px, transparent 1px);
  }

  .file-error {
    margin: auto;
    color: var(--muted);
    font-size: 0.8rem;
    padding: 1rem;
    text-align: center;
  }

  @media (prefers-reduced-motion: reduce) {
    .zbtn {
      transition: none;
    }
  }
</style>
