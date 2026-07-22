<script lang="ts">
  /**
   * The board pane: server-rendered raster stage + outline rail + numeric
   * inspector + page navigator.
   *
   * Layout truth is server-side — the stage shows exactly the engine's pixels,
   * never a DOM re-layout of the scene. The client parses the board JSON only
   * for *identity and geometry* (hit-testing, the outline, the inspector
   * numbers), which are the file's own literal values, not derived layout.
   * Every mutation routes through POST /board/edit so the canonical
   * byte-stable writer stays the one authority on bytes.
   */
  import {
    fsBoardEdit,
    fsBoardRender,
    midTruncate,
    type BoardRender,
  } from "./files";
  import { retain, release, noteWrite, type FileEntry } from "./fileStore.svelte";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
  }
  let { path }: Props = $props();

  // --- file entry: revalidation signal -----------------------------------
  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const p = path;
    const e = retain(p);
    entry = e;
    void e.ensureChunk();
    return () => release(p);
  });

  // --- parsed geometry (identity only, never layout truth) ---------------
  interface ObjInfo {
    id: string;
    kind: string;
    at: [number, number] | null;
    size: [number, number] | null;
  }
  interface PageInfo {
    id: string;
    objects: ObjInfo[];
  }
  interface BoardInfo {
    title: string | null;
    canvas: [number, number];
    pages: PageInfo[];
  }

  const board = $derived.by<BoardInfo | null>(() => {
    const e = entry;
    if (e === null || e.path !== path || e.chunk === null) return null;
    // Boards past the 256KB first chunk lose editing, not viewing — the
    // render path reads the file server-side regardless.
    if (e.chunk.bytes.length < e.chunk.size) return null;
    try {
      const raw = JSON.parse(new TextDecoder().decode(e.chunk.bytes)) as {
        title?: string;
        canvas?: { size?: [number, number] };
        pages?: {
          id?: string;
          objects?: { id?: string; type?: string; at?: [number, number]; size?: [number, number] }[];
        }[];
      };
      return {
        title: raw.title ?? null,
        canvas: raw.canvas?.size ?? [960, 540],
        pages: (raw.pages ?? []).map((p, i) => ({
          id: p.id ?? `page-${i + 1}`,
          objects: (p.objects ?? []).map((o) => ({
            id: o.id ?? "",
            kind: o.type ?? "?",
            at: Array.isArray(o.at) ? [o.at[0], o.at[1]] : null,
            size: Array.isArray(o.size) ? [o.size[0], o.size[1]] : null,
          })),
        })),
      };
    } catch {
      return null;
    }
  });

  // --- render state -------------------------------------------------------
  let page = $state(0);
  let render = $state<BoardRender | null>(null);
  let renderError = $state<string | null>(null);
  /** The current stage image URL. Swapped only after the new render lands, so
   *  an edit never flashes the stage through a spinner. */
  let imgUrl = $state<string | null>(null);
  let rendering = $state(false);

  // Re-render whenever the path, page, or on-disk content changes. `mtime`
  // is the fileStore's invalidation token — the daemon's 2s watcher moves it
  // when an agent writes the file, so agent edits appear without a reload.
  $effect(() => {
    const p = path;
    const pg = page;
    void entry?.mtime;
    let cancelled = false;
    rendering = true;
    fsBoardRender(p, pg).then(
      (r) => {
        if (cancelled) return;
        render = r;
        renderError = null;
        imgUrl = `/raw/${r.ticket}`;
        rendering = false;
      },
      (err: unknown) => {
        if (cancelled) return;
        renderError = err instanceof Error ? err.message : String(err);
        rendering = false;
      },
    );
    return () => {
      cancelled = true;
    };
  });

  $effect(() => {
    // A path change resets navigation; a shorter board clamps it.
    void path;
    page = 0;
  });
  $effect(() => {
    const count = render?.pageCount ?? 1;
    if (page >= count) page = Math.max(0, count - 1);
  });

  const pageObjects = $derived.by<ObjInfo[]>(() => {
    const b = board;
    if (b === null || page >= b.pages.length) return [];
    return b.pages[page].objects;
  });

  // --- selection + drag ---------------------------------------------------
  let selected = $state<string | null>(null);
  const selectedObj = $derived(pageObjects.find((o) => o.id === selected) ?? null);

  let stageEl = $state<HTMLDivElement | null>(null);
  /** Stage-pixels-per-board-point, from the rendered image's on-screen size. */
  let ptScale = $state(1);
  let stageOrigin = $state<[number, number]>([0, 0]);

  function syncStageMetrics(img: HTMLImageElement): void {
    const b = board;
    if (b === null) return;
    const rect = img.getBoundingClientRect();
    ptScale = rect.width / b.canvas[0];
    const host = stageEl?.getBoundingClientRect();
    if (host !== undefined) stageOrigin = [rect.left - host.left, rect.top - host.top];
  }

  // The image's on-screen size also changes when the PANE does (split drag,
  // window resize) — with no load event fired. Without this, hit-testing and
  // the selection overlay drift until the next render swaps the image.
  $effect(() => {
    const el = stageEl;
    if (el === null) return;
    const ro = new ResizeObserver(() => {
      const img = el.querySelector("img");
      if (img !== null) syncStageMetrics(img);
    });
    ro.observe(el);
    return () => ro.disconnect();
  });

  /** Board-point coordinates of a pointer event on the stage. */
  function toPt(ev: PointerEvent): [number, number] {
    const host = stageEl?.getBoundingClientRect();
    if (host === undefined || ptScale === 0) return [0, 0];
    return [
      (ev.clientX - host.left - stageOrigin[0]) / ptScale,
      (ev.clientY - host.top - stageOrigin[1]) / ptScale,
    ];
  }

  interface Drag {
    id: string;
    startPt: [number, number];
    origAt: [number, number];
    dx: number;
    dy: number;
    moved: boolean;
  }
  let drag = $state<Drag | null>(null);
  let saving = $state(false);
  let saveError = $state<string | null>(null);

  function hit(pt: [number, number]): ObjInfo | null {
    // Topmost wins: z-order is array order, so walk backwards.
    for (let i = pageObjects.length - 1; i >= 0; i--) {
      const o = pageObjects[i];
      if (o.at === null || o.size === null) continue;
      if (
        pt[0] >= o.at[0] &&
        pt[0] <= o.at[0] + o.size[0] &&
        pt[1] >= o.at[1] &&
        pt[1] <= o.at[1] + o.size[1]
      )
        return o;
    }
    return null;
  }

  function onPointerDown(ev: PointerEvent): void {
    if (ev.button !== 0) return;
    const pt = toPt(ev);
    const target = hit(pt);
    selected = target?.id ?? null;
    if (target === null || target.at === null) return;
    (ev.currentTarget as HTMLElement).setPointerCapture(ev.pointerId);
    drag = { id: target.id, startPt: pt, origAt: target.at, dx: 0, dy: 0, moved: false };
  }

  function onPointerMove(ev: PointerEvent): void {
    const d = drag;
    if (d === null) return;
    const pt = toPt(ev);
    d.dx = pt[0] - d.startPt[0];
    d.dy = pt[1] - d.startPt[1];
    if (Math.abs(d.dx) > 2 || Math.abs(d.dy) > 2) d.moved = true;
  }

  function onPointerUp(): void {
    const d = drag;
    drag = null;
    if (d === null || !d.moved) return;
    // Snap to the 8pt grid client-side purely for the optimistic number; the
    // daemon's normalize() is the authority and snaps identically.
    const at: [number, number] = [
      Math.round((d.origAt[0] + d.dx) / 8) * 8,
      Math.round((d.origAt[1] + d.dy) / 8) * 8,
    ];
    void commit(d.id, { at });
  }

  /** Commits are chained: edit is server-side load→modify→save, so two
   *  overlapping gestures (a drag racing an inspector field) would lose one. */
  let commitChain: Promise<void> = Promise.resolve();

  function commit(
    id: string,
    change: { at?: [number, number]; size?: [number, number] },
  ): Promise<void> {
    commitChain = commitChain.then(async () => {
      saving = true;
      saveError = null;
      try {
        const mtime = await fsBoardEdit(path, id, change);
        // Adopt our own write: publishing the returned token moves
        // entry.mtime, which both refreshes the parsed geometry in place and
        // re-keys the stage render effect — the pixels follow the gesture
        // immediately instead of trailing the 2s disk watcher.
        noteWrite(path, mtime);
      } catch (err) {
        saveError = err instanceof Error ? err.message : String(err);
      } finally {
        saving = false;
      }
    });
    return commitChain;
  }

  /** Inspector numeric commit: one field of at/size. */
  function commitField(field: "x" | "y" | "w" | "h", raw: string): void {
    const o = selectedObj;
    if (o === null || o.at === null || o.size === null) return;
    const v = Number(raw);
    if (!Number.isFinite(v)) return;
    if (field === "x" || field === "y") {
      const at: [number, number] = field === "x" ? [v, o.at[1]] : [o.at[0], v];
      void commit(o.id, { at });
    } else {
      const size: [number, number] = field === "w" ? [v, o.size[1]] : [o.size[0], v];
      void commit(o.id, { size });
    }
  }

  const warnings = $derived(
    (render?.diagnostics ?? []).filter((d) => d.severity !== "info"),
  );

  /** The selection box in stage pixels, tracking an in-flight drag. */
  const selectionBox = $derived.by(() => {
    const o = selectedObj;
    if (o === null || o.at === null || o.size === null) return null;
    const d = drag;
    const dx = d !== null && d.id === o.id ? d.dx : 0;
    const dy = d !== null && d.id === o.id ? d.dy : 0;
    return {
      left: stageOrigin[0] + (o.at[0] + dx) * ptScale,
      top: stageOrigin[1] + (o.at[1] + dy) * ptScale,
      width: o.size[0] * ptScale,
      height: o.size[1] * ptScale,
    };
  });
</script>

<div class="board-view">
  <div class="stage-wrap">
    <!-- The stage is a pointer surface for select/drag; keyboard access to
         the same objects goes through the outline rail's real buttons. -->
    <div
      class="stage"
      role="presentation"
      bind:this={stageEl}
      onpointerdown={onPointerDown}
      onpointermove={onPointerMove}
      onpointerup={onPointerUp}
      onpointercancel={() => (drag = null)}
    >
      {#if imgUrl !== null}
        <img
          src={imgUrl}
          alt={board?.title ?? "board page"}
          draggable="false"
          onload={(ev) => syncStageMetrics(ev.currentTarget as HTMLImageElement)}
        />
        {#if selectionBox !== null}
          <div
            class="selection"
            style:left={`${selectionBox.left}px`}
            style:top={`${selectionBox.top}px`}
            style:width={`${selectionBox.width}px`}
            style:height={`${selectionBox.height}px`}
          ></div>
        {/if}
      {:else if renderError !== null}
        <div class="board-error">{renderError}</div>
      {:else}
        <Spinner />
      {/if}
      {#if rendering && imgUrl !== null}
        <div class="rendering-dot" title="rendering"></div>
      {/if}
    </div>

    <div class="pagebar">
      <button
        class="nav"
        disabled={page === 0}
        onclick={() => (page = Math.max(0, page - 1))}
        aria-label="previous page">‹</button
      >
      <span class="page-label">
        {render?.pages[page] ?? "…"} · {page + 1}/{render?.pageCount ?? 1}
      </span>
      <button
        class="nav"
        disabled={page + 1 >= (render?.pageCount ?? 1)}
        onclick={() => (page = page + 1)}
        aria-label="next page">›</button
      >
      {#if saving}
        <span class="save-state">saving…</span>
      {:else if saveError !== null}
        <span class="save-state err" title={saveError}>{midTruncate(saveError, 60)}</span>
      {/if}
    </div>

    {#if warnings.length > 0}
      <div class="diags">
        {#each warnings as w (w.rendered)}
          <div class="diag" class:err={w.severity === "error"}>{w.rendered}</div>
        {/each}
      </div>
    {/if}
  </div>

  <aside class="rail">
    <div class="rail-title">{board?.title ?? "board"}</div>
    <div class="outline">
      {#each pageObjects as o (o.id)}
        <button
          class="obj"
          class:on={o.id === selected}
          onclick={() => (selected = o.id === selected ? null : o.id)}
        >
          <span class="obj-kind">{o.kind}</span>
          <span class="obj-id">{o.id}</span>
        </button>
      {/each}
      {#if pageObjects.length === 0}
        <div class="empty">no objects on this page</div>
      {/if}
    </div>

    {#if selectedObj !== null && selectedObj.at !== null && selectedObj.size !== null}
      <div class="inspector">
        <div class="insp-head">{selectedObj.id}</div>
        <div class="insp-grid">
          <label>x <input type="number" step="8" value={selectedObj.at[0]}
            onchange={(e) => commitField("x", (e.currentTarget as HTMLInputElement).value)} /></label>
          <label>y <input type="number" step="8" value={selectedObj.at[1]}
            onchange={(e) => commitField("y", (e.currentTarget as HTMLInputElement).value)} /></label>
          <label>w <input type="number" step="8" value={selectedObj.size[0]}
            onchange={(e) => commitField("w", (e.currentTarget as HTMLInputElement).value)} /></label>
          <label>h <input type="number" step="8" value={selectedObj.size[1]}
            onchange={(e) => commitField("h", (e.currentTarget as HTMLInputElement).value)} /></label>
        </div>
        <div class="insp-unit">pt · snaps to the 8 pt grid</div>
      </div>
    {/if}
  </aside>
</div>

<style>
  .board-view {
    position: absolute;
    inset: 0;
    display: flex;
    background: var(--term-bg);
    overflow: hidden;
  }
  .stage-wrap {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    touch-action: none;
    user-select: none;
  }
  .stage img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    box-shadow: 0 2px 16px var(--scrim);
    border: 1px solid var(--edge);
    border-radius: 4px;
  }
  .selection {
    position: absolute;
    border: 1.5px solid var(--accent);
    border-radius: 2px;
    pointer-events: none;
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent) 30%, transparent);
  }
  .rendering-dot {
    position: absolute;
    top: 10px;
    right: 10px;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
    opacity: 0.7;
  }
  .board-error {
    color: var(--err);
    font-size: var(--text-sm);
    padding: 16px;
    text-align: center;
  }
  .pagebar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    border-top: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .nav {
    background: none;
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--fg);
    width: 22px;
    height: 22px;
    line-height: 1;
    cursor: pointer;
  }
  .nav:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .nav:not(:disabled):hover {
    background: var(--row-hover);
  }
  .page-label {
    font-family: var(--mono);
  }
  .save-state {
    margin-left: auto;
  }
  .save-state.err {
    color: var(--err);
  }
  .diags {
    max-height: 96px;
    overflow-y: auto;
    border-top: 1px solid var(--edge);
    padding: 4px 12px;
  }
  .diag {
    font-size: var(--text-xs);
    font-family: var(--mono);
    color: var(--warn);
    padding: 1px 0;
  }
  .diag.err {
    color: var(--err);
  }
  .rail {
    width: 220px;
    flex-shrink: 0;
    border-left: 1px solid var(--edge);
    background: var(--rail-bg);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .rail-title {
    padding: 10px 12px 6px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .outline {
    flex: 1;
    overflow-y: auto;
    padding: 0 6px;
  }
  .obj {
    display: flex;
    align-items: baseline;
    gap: 6px;
    width: 100%;
    padding: 4px 6px;
    background: none;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    text-align: left;
    font-size: var(--text-xs);
  }
  .obj:hover {
    background: var(--row-hover);
  }
  .obj.on {
    background: var(--row-active);
  }
  .obj-kind {
    color: var(--muted);
    font-family: var(--mono);
    flex-shrink: 0;
  }
  .obj-id {
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .empty {
    color: var(--muted);
    font-size: var(--text-xs);
    padding: 8px 6px;
  }
  .inspector {
    border-top: 1px solid var(--edge);
    padding: 8px 12px 10px;
  }
  .insp-head {
    font-size: var(--text-xs);
    font-family: var(--mono);
    color: var(--accent);
    margin-bottom: 6px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .insp-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 4px 8px;
  }
  .insp-grid label {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    color: var(--muted);
    font-family: var(--mono);
  }
  .insp-grid input {
    width: 100%;
    min-width: 0;
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 3px;
    color: var(--fg);
    font-size: var(--text-xs);
    font-family: var(--mono);
    padding: 2px 4px;
  }
  .insp-unit {
    margin-top: 6px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
</style>
