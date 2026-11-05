<script lang="ts">
  /**
   * HTML file view with a preview | split | edit toggle. Preview is the
   * sandboxed iframe the daemon serves under CSP
   * "sandbox allow-scripts" (relative assets resolve through the ticket's
   * folder-confined `/raw/{ticket}/{path}` route).
   * Edit is the shared CodeMirror editor in HTML mode (Cmd/Ctrl+S saves; dirty
   * dot + conflict handling come from CodeView). SPLIT puts the editor beside a
   * live preview of the editor's buffer (a sandboxed `srcdoc` iframe, same
   * origin-less isolation) that re-renders as you type — the file is still only
   * written on save, and the plain Preview mode stays the authoritative /raw
   * render (the one that can load the page's relative assets). The editor mounts
   * once and survives every toggle, so flipping modes never drops an unsaved
   * buffer. The live srcdoc is debounced so a keystroke doesn't reload the whole
   * iframe on every character. Editing is offered only for files under the 1MB
   * cap.
   *
   * In the app, Preview is not an iframe but a web view of its own laid over
   * the layer (`shared/paneView.ts`): its own web process, so a heavy report
   * can't freeze the window (a 40 MB one did, 2026-11-04). The iframe stays
   * the browser's path and the fallback for an app build without pane views.
   * The page can be pointed at like a PDF: a text selection (either way: the
   * native view's bridge, or the daemon's `?bridge=1` script in the iframe)
   * or, in the app, a box drawn with the area tool on a snapshot of the
   * view goes to an agent through the reference bridge.
  */
  import { tick, untrack, type Component } from "svelte";
  import { basename, EDIT_MAX_BYTES, type FileChunk } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import SplitEditPreview from "./SplitEditPreview.svelte";
  import Spinner from "./Spinner.svelte";
  import { PaneViewControl, paneView, paneViewsPossible } from "../shared/paneView";
  import { paneFrame } from "../shared/paneFrame";
  import { referencePill, withBridge } from "../shared/paneViewBridge";
  import {
    activeSelection,
    clearSelection,
    cutAt,
    referenceTarget,
    requestReference,
    setSelection,
    type FileSelection,
  } from "../shared/reference";
  import ReferenceChip from "../shared/ReferenceChip.svelte";
  import { PINNED } from "../shared/keys";
  import type { Point, Region } from "./imageRegion";
  import { cropRaster, pageFragment, shownToView, snapshotSource, viewBox, viewToShown, type Size } from "./htmlRegion";
  import { paneVisibility } from "./viewLifecycle.svelte";

  interface Props {
    path: string;
    /** This tab's pane layer is on screen (Pane, through FileView). */
    visible?: boolean;
    /** A press inside the native page view: focus this pane (its clicks
     *  never reach the pane's own pointer handler). */
    onFocusRequest?: () => void;
  }

  let { path, visible = undefined, onFocusRequest }: Props = $props();
  let root = $state<HTMLElement | null>(null);
  const shown = paneVisibility(
    () => visible,
    () => root,
  );
  /** Preview through a native pane view; false after the shell declined. */
  let native = $state(paneViewsPossible());
  /** The native view has yet to show this URL's page. */
  let viewLoading = $state(true);
  /** The native view's page process died for good (until a reload). */
  let viewCrashed = $state(false);
  const control = new PaneViewControl();

  type Mode = "preview" | "split" | "edit";
  let mode = $state<Mode>("preview");
  let chunk = $state<FileChunk | null>(null);
  let chunkError = $state<string | null>(null);
  /** Null until the first fetch tells us whether the file fits the edit cap. */
  let editable = $state<boolean | null>(null);
  /** The editor mounts on the first split/edit and then persists (CSS-hidden in
   *  preview) so no toggle drops the unsaved buffer. */
  let entered = $state(false);
  let CodeView = $state<
    Component<{ path: string; first: FileChunk; onDoc?: (text: string) => void }> | null
  >(null);
  let codeLoadError = $state<string | null>(null);
  $effect(() => {
    if (!entered || CodeView !== null) return;
    void import("./CodeView.svelte").then(
      (m) => (CodeView = m.default),
      () => (codeLoadError = "failed to load the editor"),
    );
  });
  /** The editor's live buffer, and its debounced mirror for the live iframe (a
   *  full srcdoc reload per keystroke would flicker/scroll-reset). */
  let liveSource = $state("");
  let liveDebounced = $state("");
  $effect(() => {
    const src = liveSource;
    const t = setTimeout(() => (liveDebounced = src), 200);
    return () => clearTimeout(t);
  });

  // The shared store entry holds the ticketed /raw/ URL: cached across a tab
  // switch and re-minted in place when the file changes on disk (a save on the
  // edit side, or an agent write) — so preview reflects it without reopening.
  // The daemon serves HTML under CSP "sandbox allow-scripts" and the iframe
  // repeats the sandbox — the bearer token never appears in a URL.
  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const e = retain(path);
    entry = e;
    void e.ensureRawUrl();
    return () => release(path);
  });
  // The frame loads the page by its own name UNDER the ticket
  // (`/raw/{ticket}/report.html`), so its relative `app.js` / `figs/a.png`
  // resolve to `/raw/{ticket}/app.js` — the daemon's folder-confined route.
  // The name is the ticket's canonical file's, not the pane path's: opened
  // through `latest.html -> runs/42/report.html` the page and its assets
  // live in runs/42, and a hidden `.summary.html` is served only by name.
  const url = $derived(
    entry?.rawUrl != null
      ? `${entry.rawUrl}/${encodeURIComponent(entry.rawName ?? basename(path))}`
      : null,
  );
  const error = $derived(entry?.rawError ?? null);

  // Reset per path.
  $effect(() => {
    void path;
    mode = "preview";
    chunk = null;
    chunkError = null;
    editable = null;
    entered = false;
    codeLoadError = null;
    liveSource = "";
    liveDebounced = "";
    untrack(() => {
      pageSel = null;
      leaveArea();
      unpublish();
    });
  });

  // Area mode belongs to the preview: another mode ends it.
  $effect(() => {
    if (mode !== "preview") untrack(leaveArea);
  });

  async function enterEditor(target: "split" | "edit"): Promise<void> {
    // Read the raw source from the shared store (cached with the preview URL);
    // CodeView handles the rest (background fill + save/dirty/conflict flow).
    const e = entry;
    if (e === null) return;
    // A failed fetch is retried on the next click (the store refetches a
    // missing chunk): gating on the old error left `entered` set with no
    // chunk, so the edit layer stayed blank for the life of the tab.
    if (chunk === null) {
      chunkError = null;
      await e.ensureChunk();
      if (entry !== e) return; // path changed while fetching
      if (e.chunk === null) {
        chunkError = e.chunkError ?? "failed to load source";
        return;
      }
      chunk = e.chunk;
      editable = e.chunk.size <= EDIT_MAX_BYTES;
    }
    if (editable === false) return; // too large; stay in preview
    entered = true;
    mode = target;
  }

  // --- pointing at part of the page (context bridge) ---------------------------------
  //
  // A text selection in the page sends its quote and the heading above it
  // (`@report.html (§ Results) "…"`), offered by a pill the page's bridge
  // draws by it (either transport: `control` speaks to whichever holds the
  // page). The area tool (native view only) freezes the view behind a snapshot of it; a box
  // drawn there sends `#xywh=…` in document CSS px, the text under it, and
  // the snapshot's pixels as a PNG crop. Both publish through the shared
  // reference bridge, the way PdfView does.

  const selOwner = {};
  /** What this view last published, and which kind: it clears only its
   *  own, and a newer selection elsewhere is told apart by identity. */
  let published: FileSelection | null = null;
  let publishedKind: "text" | "area" | null = null;
  /** The page's text selection as last reported: kept while the area tool
   *  or a newer selection elsewhere covers it, re-offered after. */
  let pageSel: { text: string; context: string | null } | null = null;
  let disposed = false;
  /** Text taken from under a box, before the composer's own excerpt cut. */
  const REGION_TEXT_MAX = 2000;
  /** Crop raster long-side cap (past it a model downsamples anyway). */
  const CROP_CAP = 1568;
  /** Room the chip needs inside the stage (px). */
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
    clearSelection(selOwner);
  }

  // A newer selection elsewhere (another view, a terminal) replaces this
  // one; a finished box goes with it.
  $effect(() => {
    const a = $activeSelection;
    if (published === null || a === published) return;
    const wasArea = publishedKind === "area";
    published = null;
    publishedKind = null;
    if (wasArea && boxFrom === null) {
      pick = null;
      pickReady = false;
    }
  });

  $effect(() => () => {
    disposed = true;
    unpublish();
  });

  // The in-page pill wears the shared chip's words and follows the target.
  $effect(() => {
    control.setChip(referencePill($referenceTarget, undefined, PINNED.reference));
  });

  function publishText(): void {
    const s = pageSel;
    if (s === null) return;
    publish(
      {
        kind: "file",
        path,
        startLine: null,
        endLine: null,
        text: s.text,
        // Where it sits, for the agent: `@report.html (§ Results) "…"`.
        ...(s.context !== null ? { context: `§ ${s.context}` } : {}),
        label: "selection",
      },
      "text",
    );
  }

  function onPageSelection(sel: { text: string; context: string | null }): void {
    if (sel.text.trim() === "") {
      pageSel = null;
      unpublish("text");
      return;
    }
    pageSel = { text: sel.text, context: sel.context };
    if (!areaMode) publishText();
  }

  /** The in-page pill: reference the page's own selection, even when a
   *  newer one elsewhere had taken the bridge since. */
  function onPageReference(): void {
    if (!areaMode && pageSel !== null && publishedKind !== "text") publishText();
    requestReference();
  }

  // --- the area tool ---

  let areaMode = $state(false);
  /** The view's viewport as it was when the tool started (CSS px size). */
  let snap = $state<{ src: string; width: number; height: number } | null>(null);
  let snapEl = $state<HTMLImageElement | null>(null);
  let stageEl = $state<HTMLDivElement | null>(null);
  let stageW = $state(0);
  let stageH = $state(0);
  /** The box being drawn, then the one pointed at (view px). */
  let pick = $state.raw<Region | null>(null);
  /** The box is drawn and published (its chip shows). */
  let pickReady = $state(false);
  let boxFrom: { at: Point; client: Point; pointer: number } | null = null;
  let areaError = $state<string | null>(null);
  /** Snapshots and crops resolve late: only the newest one lands. */
  let areaGen = 0;
  let pickGen = 0;

  const viewSize = $derived<Size>(snap !== null ? { w: snap.width, h: snap.height } : { w: 0, h: 0 });
  const shownSize = $derived<Size>({ w: stageW, h: stageH });

  async function enterArea(): Promise<void> {
    const gen = ++areaGen;
    areaError = null;
    areaMode = true;
    // Captured while the view is still on screen; it hides once the
    // snapshot has painted in its place (`onSnapLoad`).
    const s = await control.snapshot();
    if (gen !== areaGen || disposed) return;
    if (s === null || s.width <= 0 || s.height <= 0) {
      areaMode = false;
      areaError = "couldn't capture the page";
      return;
    }
    unpublish("text");
    snap = { src: `data:image/png;base64,${s.png}`, width: s.width, height: s.height };
  }

  function onSnapLoad(): void {
    if (!areaMode) return;
    control.freeze(true);
    void tick().then(() => stageEl?.focus({ preventScroll: true }));
  }

  function onSnapError(): void {
    leaveArea();
    areaError = "couldn't capture the page";
  }

  function leaveArea(): void {
    areaGen += 1;
    pickGen += 1;
    areaMode = false;
    snap = null;
    pick = null;
    pickReady = false;
    boxFrom = null;
    unpublish("area");
    control.freeze(false);
    // The page's own selection outlived the stand-in: offer it again.
    if (pageSel !== null && published === null) publishText();
  }

  function toggleArea(): void {
    if (areaMode) leaveArea();
    else void enterArea();
  }

  function clearPick(): void {
    pick = null;
    pickReady = false;
    pickGen += 1;
    unpublish("area");
  }

  function stagePoint(e: PointerEvent): Point | null {
    const el = stageEl;
    if (el === null) return null;
    const r = el.getBoundingClientRect();
    return shownToView({ x: e.clientX - r.left, y: e.clientY - r.top }, shownSize, viewSize);
  }

  function onStagePointerDown(e: PointerEvent): void {
    if (e.button !== 0 || snap === null) return;
    const at = stagePoint(e);
    if (at === null) return;
    e.preventDefault();
    if (pick !== null) clearPick();
    boxFrom = { at, client: { x: e.clientX, y: e.clientY }, pointer: e.pointerId };
    stageEl?.setPointerCapture(e.pointerId);
    stageEl?.focus({ preventScroll: true });
  }

  function onStagePointerMove(e: PointerEvent): void {
    const from = boxFrom;
    if (from === null || e.pointerId !== from.pointer) return;
    // A few pixels of jitter is a click, not a box.
    if (pick === null && Math.hypot(e.clientX - from.client.x, e.clientY - from.client.y) < 4) return;
    const to = stagePoint(e);
    if (to === null) return;
    pick = viewBox(from.at, to, viewSize);
  }

  function onStagePointerUp(e: PointerEvent): void {
    const from = boxFrom;
    if (from === null || e.pointerId !== from.pointer) return;
    boxFrom = null;
    if (stageEl?.hasPointerCapture(e.pointerId) === true) stageEl.releasePointerCapture(e.pointerId);
    const p = pick;
    if (p === null || e.type === "pointercancel") {
      clearPick();
      return;
    }
    void finishPick(p);
  }

  /** Publish a finished box once its text and crop are in. */
  async function finishPick(p: Region): Promise<void> {
    const gen = ++pickGen;
    const [under, crop] = await Promise.all([
      control.region({ x: p.x, y: p.y, width: p.w, height: p.h }),
      renderCrop(p),
    ]);
    if (gen !== pickGen || disposed) return;
    const text = cutAt((under?.text ?? "").replace(/\s+/g, " ").trim(), REGION_TEXT_MAX);
    const sel: FileSelection = {
      kind: "file",
      path,
      startLine: null,
      endLine: null,
      text,
      label: "region",
    };
    // Without the scroll offsets the box can't be placed in the document:
    // the text and pixels still go.
    if (under !== null) sel.fragment = pageFragment(p, under.scrollX, under.scrollY);
    if (crop !== null) sel.crop = crop;
    publish(sel, "area");
    pickReady = true;
  }

  /** The box's pixels from the snapshot, at the snapshot's own density. */
  async function renderCrop(p: Region): Promise<Blob | null> {
    const img = snapEl;
    if (img === null || !img.complete || img.naturalWidth === 0) return null;
    const src = snapshotSource(p, viewSize, { w: img.naturalWidth, h: img.naturalHeight });
    if (src === null) return null;
    const out = cropRaster(src, CROP_CAP);
    const canvas = document.createElement("canvas");
    canvas.width = out.w;
    canvas.height = out.h;
    try {
      const ctx = canvas.getContext("2d");
      if (ctx === null) return null;
      ctx.drawImage(img, src.x, src.y, src.w, src.h, 0, 0, out.w, out.h);
      return await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/png"));
    } catch {
      // No pixels: the locator and text still go.
      return null;
    } finally {
      canvas.width = canvas.height = 0;
    }
  }

  /** The box as drawn over the snapshot. */
  const pickShown = $derived(pick !== null ? viewToShown(pick, shownSize, viewSize) : null);

  /** The chip sits under the box's bottom-right corner, inside the stage. */
  const chipPos = $derived.by(() => {
    const p = pickShown;
    if (p === null || !pickReady) return null;
    const clamp = (n: number, lo: number, hi: number) => Math.min(Math.max(n, lo), Math.max(lo, hi));
    return {
      x: clamp(Math.max(p.x, p.x + p.w - CHIP_W), 4, stageW - CHIP_W),
      y: clamp(p.y + p.h + 6, 4, stageH - CHIP_H - 4),
    };
  });

  function onViewKey(e: KeyboardEvent): void {
    if (e.key !== "Escape" || !areaMode) return;
    if (pick !== null) clearPick();
    else leaveArea();
    e.preventDefault();
  }

  /** The area tool draws on a snapshot of the native view: a framed page's
   *  pixels can't be read, so there it shows disabled. */
  const areaShown = $derived(mode === "preview" && url !== null && error === null);
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="html-view" bind:this={root} onkeydown={onViewKey}>
  <div class="html-bar">
    <div class="toggle" role="tablist" aria-label="html mode">
      <button
        class="seg"
        class:on={mode === "preview"}
        role="tab"
        aria-selected={mode === "preview"}
        onclick={() => (mode = "preview")}>preview</button
      >
      <button
        class="seg"
        class:on={mode === "split"}
        role="tab"
        aria-selected={mode === "split"}
        title={editable === false ? "over 1 MB — preview only" : "edit with a live preview"}
        disabled={editable === false}
        onclick={() => void enterEditor("split")}>split</button
      >
      <button
        class="seg"
        class:on={mode === "edit"}
        role="tab"
        aria-selected={mode === "edit"}
        title={editable === false ? "over 1 MB — preview only" : "edit source"}
        disabled={editable === false}
        onclick={() => void enterEditor("edit")}>edit</button
      >
    </div>
    {#if chunkError !== null}<span class="html-bar-err">{chunkError}</span>{/if}
    {#if areaError !== null}<span class="html-bar-err">{areaError}</span>{/if}
    {#if pick !== null && pickReady}
      <span class="selection-note region-note" title="the area you pointed at, in CSS px of the page">
        area · {pick.w}×{pick.h} px
        <button class="zbtn ic sm" aria-label="clear the area" title="clear the area (Esc)" onclick={clearPick}
          >×</button
        >
      </span>
    {/if}
    {#if areaShown}
      <span class="spacer"></span>
      <button
        class="zbtn ic"
        class:on={areaMode}
        onclick={toggleArea}
        disabled={!native || (viewLoading && !areaMode)}
        aria-label="select an area"
        aria-pressed={areaMode}
        title={!native
          ? "selecting an area needs the macOS app"
          : areaMode
            ? "drag a box on the page to reference it in an agent — click to stop (Esc)"
            : "select an area to reference in an agent"}
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
    {/if}
  </div>

  <div class="html-content">
    <!-- Authoritative /raw render (loads relative assets). Shown in preview
         mode; kept mounted (hidden) so re-entering needs no re-mint. -->
    <div class="layer" class:hidden={mode !== "preview"}>
      {#if error !== null}
        <div class="file-error">{error}</div>
      {:else if url !== null && native}
        <!-- The loading state sits INSIDE the placeholder: paneView treats a
             positioned status element elsewhere over it as an overlay. -->
        <div
          class="native-slot"
          use:paneView={{
            url,
            shown: shown.pane && mode === "preview",
            onFallback: () => (native = false),
            onLoading: (l) => {
              viewLoading = l;
              // A load under way means the page is no longer the crashed one.
              if (l) viewCrashed = false;
            },
            onCrashed: () => (viewCrashed = true),
            onPage: () => (viewCrashed = false),
            onFocus: () => onFocusRequest?.(),
            onSelection: onPageSelection,
            onReference: onPageReference,
            control,
          }}
        >
          {#if viewCrashed}
            <div class="view-crashed">
              <span>This page stopped responding</span>
              <button
                class="crash-reload"
                onclick={() => {
                  viewCrashed = false;
                  control.nav("reload");
                }}>Reload</button
              >
            </div>
          {:else if viewLoading}<Spinner />{/if}
        </div>
        {#if snap !== null}
          <!-- The frozen view's stand-in: its snapshot, drawn on with the
               area tool. -->
          <div
            class="area-stage"
            role="application"
            aria-label="the page as captured — drag a box to point at part of it (Esc to stop)"
            tabindex="-1"
            bind:this={stageEl}
            bind:clientWidth={stageW}
            bind:clientHeight={stageH}
            onpointerdown={onStagePointerDown}
            onpointermove={onStagePointerMove}
            onpointerup={onStagePointerUp}
            onpointercancel={onStagePointerUp}
          >
            <img
              class="snap"
              src={snap.src}
              alt=""
              draggable="false"
              bind:this={snapEl}
              onload={onSnapLoad}
              onerror={onSnapError}
            />
            {#if pickShown !== null}
              <div
                class="pick-mark"
                class:drawing={!pickReady}
                aria-hidden="true"
                style:left="{pickShown.x}px"
                style:top="{pickShown.y}px"
                style:width="{pickShown.w}px"
                style:height="{pickShown.h}px"
              ></div>
            {/if}
            {#if chipPos !== null}
              <ReferenceChip x={chipPos.x} y={chipPos.y} label="region" />
            {/if}
          </div>
        {/if}
      {:else if url !== null}
        <!-- `?bridge=1`: the daemon appends the pane bridge to this page, so
             text marking works here too (shared/paneFrame.ts). -->
        <iframe
          src={withBridge(url)}
          title={path}
          sandbox="allow-scripts"
          use:paneFrame={{
            onFocus: () => onFocusRequest?.(),
            onSelection: onPageSelection,
            onReference: onPageReference,
            control,
          }}
        ></iframe>
      {:else}
        <Spinner />
      {/if}
    </div>

    <!-- Editor (+ live preview in split). Mounts on the first split/edit and
         then persists, CSS-hidden in preview, so no toggle drops the buffer. -->
    {#if entered && chunk !== null}
      {@const first = chunk}
      <div class="layer" class:hidden={mode === "preview"}>
        {#if CodeView !== null}
          <SplitEditPreview split={mode === "split"}>
            {#snippet editor()}
              <CodeView {path} {first} onDoc={(t) => (liveSource = t)} />
            {/snippet}
            {#snippet preview()}
              <iframe class="live" title="{path} (live)" sandbox="allow-scripts" srcdoc={liveDebounced}
              ></iframe>
            {/snippet}
          </SplitEditPreview>
        {:else if codeLoadError !== null}
          <div class="file-error">{codeLoadError}</div>
        {:else}
          <Spinner />
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .html-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  /* Quiet mode toggle bar, matching the markdown/pane top-bar treatment. */
  .html-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 26px;
    padding: 0 0.6rem;
    border-bottom: 1px solid var(--edge);
  }

  .toggle {
    display: flex;
    align-items: center;
    gap: 1px;
  }

  .seg {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    letter-spacing: 0.04em;
    color: var(--muted);
    cursor: pointer;
    padding: 2px 8px;
    border-radius: 4px;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .seg:hover:not(:disabled) {
    color: var(--fg);
  }

  .seg.on {
    color: var(--fg);
    background: var(--row-active);
  }

  .seg:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .html-bar-err {
    font-size: var(--text-xs);
    color: var(--err);
  }

  .html-content {
    flex: 1;
    position: relative;
    min-height: 0;
  }

  .layer {
    position: absolute;
    inset: 0;
  }

  .hidden {
    display: none;
  }

  iframe {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    border: none;
    background: #ffffff; /* pages assume a white canvas regardless of theme */
  }

  /* Where the native pane view sits: the page's own white until it paints. */
  .native-slot {
    position: absolute;
    inset: 0;
    background: #ffffff;
  }

  /* A page whose process died for good: said quietly where it was, on the
     workbench's own surface rather than the page's white. */
  .view-crashed {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.6rem;
    background: var(--bg);
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .crash-reload {
    border: 1px solid var(--edge);
    border-radius: 5px;
    padding: 0.25rem 0.7rem;
    color: var(--fg);
    background: var(--bg);
    font: inherit;
    cursor: pointer;
  }

  .crash-reload:hover {
    border-color: color-mix(in srgb, var(--accent) 60%, var(--edge));
  }

  .crash-reload:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  /* The area tool's stand-in for the frozen view: its snapshot, exactly in
     the view's slot, under a crosshair. */
  .area-stage {
    position: absolute;
    inset: 0;
    overflow: hidden;
    background: #ffffff;
    cursor: crosshair;
    touch-action: none;
    user-select: none;
  }

  .area-stage:focus {
    outline: none;
  }

  .snap {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    display: block;
    pointer-events: none;
    -webkit-user-drag: none;
  }

  /* The pointed-at area: solid once drawn, dashed while dragging. */
  .pick-mark {
    position: absolute;
    box-sizing: border-box;
    pointer-events: none;
    border: 1.5px solid var(--accent);
    border-radius: 2px;
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--term-bg) 70%, transparent);
  }

  .pick-mark.drawing {
    border-style: dashed;
    background: color-mix(in srgb, var(--accent) 6%, transparent);
  }

  .spacer {
    flex: 1;
  }

  .selection-note {
    padding-left: 0.55rem;
    border-left: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .region-note {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    color: var(--fg);
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

  .zbtn:hover:not(:disabled) {
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

  .zbtn:disabled {
    opacity: 0.4;
    cursor: default;
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

  /* The live srcdoc iframe fills its split pane rather than the whole view. */
  iframe.live {
    position: absolute;
    inset: 0;
  }

  .file-error {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font-size: var(--text-md);
    padding: 1rem;
    text-align: center;
  }
</style>
