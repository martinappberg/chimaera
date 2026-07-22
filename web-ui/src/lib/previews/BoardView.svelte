<script lang="ts">
  /**
   * The board pane: server-rendered raster stage + outline rail + numeric
   * inspector + page navigator, plus present mode, corner-resize handles,
   * an actor-aware undo stack, and agent-edit attribution flashes.
   *
   * Layout truth is server-side — the stage shows exactly the engine's pixels,
   * never a DOM re-layout of the scene. The client parses the board JSON only
   * for *identity and geometry* (hit-testing, the outline, the inspector
   * numbers), which are the file's own literal values, not derived layout.
   * Every mutation routes through POST /board/edit so the canonical
   * byte-stable writer stays the one authority on bytes.
   */
  import { untrack } from "svelte";
  import { fsBoardEdit, fsBoardRender, midTruncate, type BoardRender } from "./files";
  import { retain, release, noteWrite, type FileEntry } from "./fileStore.svelte";
  import {
    attributeDiff,
    boardFrames,
    CORNERS,
    GRID_PT,
    MIN_RESIZE_PT,
    pageFrames,
    parseBoard,
    resizeFrame,
    samePair,
    snap8,
    UndoStack,
    type BoardInfo,
    type Corner,
    type ExpectedChange,
    type FieldChange,
    type Frame,
    type ObjInfo,
  } from "./boardInteract";
  import BoardPresentChrome from "./BoardPresentChrome.svelte";
  import BoardRail from "./BoardRail.svelte";
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

  const board = $derived.by<BoardInfo | null>(() => {
    const e = entry;
    if (e === null || e.path !== path || e.chunk === null) return null;
    // Boards past the 256KB first chunk lose editing, not viewing — the
    // render path reads the file server-side regardless.
    if (e.chunk.bytes.length < e.chunk.size) return null;
    return parseBoard(e.chunk.bytes);
  });

  // --- render state -------------------------------------------------------
  let page = $state(0);
  let render = $state<BoardRender | null>(null);
  let renderError = $state<string | null>(null);
  /** The current stage image URL. Swapped only after the new render lands, so
   *  an edit never flashes the stage through a spinner. */
  let imgUrl = $state<string | null>(null);
  let rendering = $state(false);

  // Re-render whenever the path, page, present mode, or on-disk content
  // changes. `mtime` is the fileStore's invalidation token — the daemon's 2s
  // watcher moves it when an agent writes the file, so agent edits appear
  // without a reload. Present mode doubles the scale for the fullscreen pixels;
  // the route clamps to [0.25, 4] identically.
  $effect(() => {
    const p = path;
    const pg = page;
    const scale = Math.min(4, Math.max(1, (window.devicePixelRatio || 1) * (presenting ? 2 : 1)));
    void entry?.mtime;
    let cancelled = false;
    rendering = true;
    fsBoardRender(p, pg, scale).then(
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

  let rootEl = $state<HTMLDivElement | null>(null);
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
  // window resize, present mode) — with no load event fired. Without this,
  // hit-testing and the selection overlay drift until the next render swaps
  // the image.
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

  type DragState =
    | {
        mode: "move";
        id: string;
        startPt: [number, number];
        origAt: [number, number];
        dx: number;
        dy: number;
        moved: boolean;
      }
    | {
        mode: "resize";
        id: string;
        corner: Corner;
        startPt: [number, number];
        origAt: [number, number];
        origSize: [number, number];
        cur: Frame;
        moved: boolean;
      };
  let drag = $state<DragState | null>(null);
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

  /** Visual size of a corner handle; hit zone is 2px more forgiving. */
  const HANDLE_PX = 8;
  const handleBoxes = $derived.by(() => {
    const box = selectionBox;
    if (box === null) return [];
    return CORNERS.map((corner) => ({
      corner,
      x: box.left + (corner === "ne" || corner === "se" ? box.width : 0),
      y: box.top + (corner === "sw" || corner === "se" ? box.height : 0),
    }));
  });

  function handleAt(px: number, py: number): Corner | null {
    const slop = HANDLE_PX / 2 + 2;
    for (const h of handleBoxes) {
      if (Math.abs(px - h.x) <= slop && Math.abs(py - h.y) <= slop) return h.corner;
    }
    return null;
  }

  function onPointerDown(ev: PointerEvent): void {
    if (ev.button !== 0 || presenting) return;
    // Focus scoping for the undo keys: only the pane the user is working in
    // may answer ⌘Z (multiple board panes, terminals, editors coexist).
    rootEl?.focus({ preventScroll: true });
    const pt = toPt(ev);
    // Handles win over object hit-testing: a handle overhangs the object's
    // corner, and a small object would otherwise be un-resizable.
    const host = stageEl?.getBoundingClientRect();
    const o = selectedObj;
    if (host !== undefined && o !== null && o.at !== null && o.size !== null) {
      const corner = handleAt(ev.clientX - host.left, ev.clientY - host.top);
      if (corner !== null) {
        (ev.currentTarget as HTMLElement).setPointerCapture(ev.pointerId);
        drag = {
          mode: "resize",
          id: o.id,
          corner,
          startPt: pt,
          origAt: o.at,
          origSize: o.size,
          cur: { at: o.at, size: o.size },
          moved: false,
        };
        return;
      }
    }
    const target = hit(pt);
    selected = target?.id ?? null;
    if (target === null || target.at === null) return;
    (ev.currentTarget as HTMLElement).setPointerCapture(ev.pointerId);
    drag = { mode: "move", id: target.id, startPt: pt, origAt: target.at, dx: 0, dy: 0, moved: false };
  }

  function onPointerMove(ev: PointerEvent): void {
    const d = drag;
    if (d === null) return;
    const pt = toPt(ev);
    const dx = pt[0] - d.startPt[0];
    const dy = pt[1] - d.startPt[1];
    if (Math.abs(dx) > 2 || Math.abs(dy) > 2) d.moved = true;
    if (d.mode === "move") {
      d.dx = dx;
      d.dy = dy;
    } else {
      d.cur = resizeFrame(d.corner, d.origAt, d.origSize, dx, dy);
    }
  }

  function onPointerUp(): void {
    const d = drag;
    drag = null;
    if (d === null || !d.moved) return;
    // Snap to the 8pt grid client-side purely for the optimistic number; the
    // daemon's normalize() is the authority and snaps identically — which is
    // also what lets the undo stack record the exact written values.
    if (d.mode === "move") {
      const at: [number, number] = [snap8(d.origAt[0] + d.dx), snap8(d.origAt[1] + d.dy)];
      if (samePair(at, d.origAt)) return;
      undoStack.push({ object: d.id, fields: [{ field: "at", from: d.origAt, to: at }] });
      void commit(d.id, { at });
    } else {
      const at: [number, number] = [snap8(d.cur.at[0]), snap8(d.cur.at[1])];
      const size: [number, number] = [
        Math.max(MIN_RESIZE_PT, snap8(d.cur.size[0])),
        Math.max(MIN_RESIZE_PT, snap8(d.cur.size[1])),
      ];
      const atMoved = !samePair(at, d.origAt);
      const sizeChanged = !samePair(size, d.origSize);
      if (!atMoved && !sizeChanged) return;
      const fields: FieldChange[] = [];
      if (sizeChanged) fields.push({ field: "size", from: d.origSize, to: size });
      if (atMoved) fields.push({ field: "at", from: d.origAt, to: at });
      undoStack.push({ object: d.id, fields });
      const change: { at?: [number, number]; size?: [number, number] } = {};
      if (sizeChanged) change.size = size;
      if (atMoved) change.at = at;
      void commit(d.id, change);
    }
  }

  // --- commits + own-write attribution bookkeeping ------------------------
  /** Commits are chained: edit is server-side load→modify→save, so two
   *  overlapping gestures (a drag racing an inspector field) would lose one. */
  let commitChain: Promise<void> = Promise.resolve();
  /** X-Mtime tokens this pane's own commits produced (task-level marker of
   *  "we caused this invalidation"; capped, insertion-ordered eviction). */
  const ownWrites = new Set<string>();
  /** Exact values this pane committed, per object, awaiting their refresh.
   *  The value-level check is the authoritative attribution signal because
   *  fileStore publishes chunk and mtime in separate microtasks (see
   *  boardInteract.attributeDiff). */
  const ownExpected = new Map<string, ExpectedChange>();

  function commit(
    id: string,
    change: { at?: [number, number]; size?: [number, number] },
  ): Promise<void> {
    commitChain = commitChain.then(async () => {
      saving = true;
      saveError = null;
      try {
        const mtime = await fsBoardEdit(path, id, change);
        ownExpected.set(id, { ...change });
        if (mtime !== null) {
          ownWrites.add(mtime);
          if (ownWrites.size > 64) {
            const oldest = ownWrites.values().next().value;
            if (oldest !== undefined) ownWrites.delete(oldest);
          }
        }
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

  /** Inspector numeric commit: one field of at/size, snapped like the daemon's
   *  normalize() so the recorded undo value matches the written file. */
  function commitField(field: "x" | "y" | "w" | "h", raw: string): void {
    const o = selectedObj;
    if (o === null || o.at === null || o.size === null) return;
    const n = Number(raw);
    if (!Number.isFinite(n)) return;
    const v = field === "w" || field === "h" ? Math.max(GRID_PT, snap8(n)) : snap8(n);
    if (field === "x" || field === "y") {
      const at: [number, number] = field === "x" ? [v, o.at[1]] : [o.at[0], v];
      if (samePair(at, o.at)) return;
      undoStack.push({ object: o.id, fields: [{ field: "at", from: o.at, to: at }] });
      void commit(o.id, { at });
    } else {
      const size: [number, number] = field === "w" ? [v, o.size[1]] : [o.size[0], v];
      if (samePair(size, o.size)) return;
      undoStack.push({ object: o.id, fields: [{ field: "size", from: o.size, to: size }] });
      void commit(o.id, { size });
    }
  }

  // --- actor-aware undo (§6.7) --------------------------------------------
  const undoStack = new UndoStack();
  let toast = $state<string | null>(null);
  let toastTimer = 0;

  function showToast(msg: string): void {
    toast = msg;
    clearTimeout(toastTimer);
    toastTimer = window.setTimeout(() => (toast = null), 2500);
  }
  $effect(() => () => clearTimeout(toastTimer));

  function runUndoRedo(redo: boolean): void {
    const b = board;
    if (b === null) return;
    // Staleness is judged against the file's current values at undo time —
    // the actor rule: never revert an agent's later write with a stale value.
    const res = redo ? undoStack.redo(boardFrames(b)) : undoStack.undo(boardFrames(b));
    if (res.kind === "apply") {
      void commit(res.object, res.change);
      showToast(`${redo ? "redid" : "undid"} ${res.verb} of ${res.object}`);
    } else if (res.kind === "stale") {
      showToast(`${redo ? "redo" : "undo"} for ${res.object} dropped — changed by another actor`);
    }
  }

  // ⌘Z/⌃Z + shift on the window, scoped to this pane by focus containment so
  // terminals, editors, and sibling board panes keep their own undo.
  $effect(() => {
    const onKey = (ev: KeyboardEvent): void => {
      if (presenting) return;
      if (!(ev.metaKey || ev.ctrlKey) || ev.altKey || ev.key.toLowerCase() !== "z") return;
      const root = rootEl;
      if (root === null || !root.contains(document.activeElement)) return;
      const t = ev.target;
      if (t instanceof HTMLElement && (t.tagName === "INPUT" || t.tagName === "TEXTAREA")) return;
      ev.preventDefault();
      runUndoRedo(ev.shiftKey);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // --- agent-edit attribution flashes (§6.5) ------------------------------
  interface Flash {
    key: number;
    at: [number, number];
    size: [number, number];
  }
  let flashes = $state<Flash[]>([]);
  let flashSeq = 0;
  const flashTimers = new Set<number>();
  let flashBaseline: {
    path: string;
    page: number;
    /** False while the board was unloaded/unparseable — a parse appearing is
     *  not an agent adding every object, so it must not flash. */
    hadBoard: boolean;
    frames: Map<string, Frame>;
  } | null = null;

  function addFlashes(items: { id: string; frame: Frame }[]): void {
    const added = items.map((c) => ({ key: flashSeq++, at: c.frame.at, size: c.frame.size }));
    flashes = [...flashes, ...added];
    const keys = new Set(added.map((f) => f.key));
    const t = window.setTimeout(() => {
      flashTimers.delete(t);
      flashes = flashes.filter((f) => !keys.has(f.key));
    }, 1200);
    flashTimers.add(t);
  }
  $effect(() => () => {
    for (const t of flashTimers) clearTimeout(t);
  });

  // Diff each reparse of the current page against the previous one; changes
  // this pane did not commit (checked by value via `ownExpected`, with
  // `ownWrites` marking the tokens we minted) flash a 1.2s attribution
  // outline at their new frames. A path or page switch resets the baseline
  // without flashing; removed objects get no overlay.
  $effect(() => {
    const p = path;
    const pg = page;
    const hasBoard = board !== null;
    const frames = pageFrames(pageObjects);
    void entry?.mtime;
    const prev = flashBaseline;
    flashBaseline = { path: p, page: pg, hadBoard: hasBoard, frames };
    if (prev === null || prev.path !== p || prev.page !== pg) return;
    if (!prev.hadBoard || !hasBoard) return;
    const changed = attributeDiff(prev.frames, frames, ownExpected);
    if (changed.length === 0) return;
    untrack(() => addFlashes(changed));
  });

  // --- present mode --------------------------------------------------------
  let presenting = $state(false);
  let chromeVisible = $state(true);
  let notesOpen = $state(false);
  let hideTimer = 0;

  function enterPresent(): void {
    presenting = true;
    notesOpen = false;
    chromeVisible = true;
    selected = null;
    drag = null;
    const el = rootEl;
    // Full-bleed within the pane works regardless; browser fullscreen is a
    // progressive upgrade the browser may refuse. Exit is handled by the
    // present-effect teardown so unmount-while-fullscreen also restores.
    if (el !== null && typeof el.requestFullscreen === "function") {
      void el.requestFullscreen().catch(() => {});
    }
  }
  function exitPresent(): void {
    presenting = false;
    notesOpen = false;
  }
  function step(delta: number): void {
    const count = render?.pageCount ?? 1;
    page = Math.min(Math.max(0, page + delta), Math.max(0, count - 1));
  }
  function armHide(): void {
    clearTimeout(hideTimer);
    hideTimer = window.setTimeout(() => (chromeVisible = false), 2000);
  }

  // Present-mode listeners live on the window and exist only while presenting.
  $effect(() => {
    if (!presenting) return;
    const onKey = (ev: KeyboardEvent): void => {
      switch (ev.key) {
        case "ArrowRight":
        case " ":
        case "PageDown":
          step(1);
          break;
        case "ArrowLeft":
        case "PageUp":
          step(-1);
          break;
        case "Home":
          page = 0;
          break;
        case "End":
          page = Math.max(0, (render?.pageCount ?? 1) - 1);
          break;
        case "Escape":
          exitPresent();
          break;
        case "n":
        case "N":
          notesOpen = !notesOpen;
          break;
        default:
          return;
      }
      ev.preventDefault();
    };
    const onMove = (): void => {
      chromeVisible = true;
      armHide();
    };
    const onFs = (): void => {
      // Browser-level exit (Esc in fullscreen fires no keydown) ends present.
      if (document.fullscreenElement !== rootEl) exitPresent();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("mousemove", onMove);
    document.addEventListener("fullscreenchange", onFs);
    armHide();
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("mousemove", onMove);
      document.removeEventListener("fullscreenchange", onFs);
      clearTimeout(hideTimer);
      if (document.fullscreenElement === rootEl) void document.exitFullscreen().catch(() => {});
    };
  });

  const warnings = $derived((render?.diagnostics ?? []).filter((d) => d.severity !== "info"));

  /** The selection box in stage pixels, tracking an in-flight drag/resize. */
  const selectionBox = $derived.by(() => {
    const o = selectedObj;
    if (o === null || o.at === null || o.size === null) return null;
    const d = drag;
    let at = o.at;
    let size = o.size;
    if (d !== null && d.id === o.id) {
      if (d.mode === "move") at = [o.at[0] + d.dx, o.at[1] + d.dy];
      else {
        at = d.cur.at;
        size = d.cur.size;
      }
    }
    return {
      left: stageOrigin[0] + at[0] * ptScale,
      top: stageOrigin[1] + at[1] * ptScale,
      width: size[0] * ptScale,
      height: size[1] * ptScale,
    };
  });
</script>

<div
  class="board-view"
  class:presenting
  class:chrome-hidden={presenting && !chromeVisible}
  bind:this={rootEl}
  tabindex="-1"
>
  <div class="stage-wrap">
    <!-- The stage is a pointer surface for select/drag/resize; keyboard access
         to the same objects goes through the outline rail's real buttons. -->
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
        {#if !presenting}
          {#if selectionBox !== null}
            <div
              class="selection"
              style:left={`${selectionBox.left}px`}
              style:top={`${selectionBox.top}px`}
              style:width={`${selectionBox.width}px`}
              style:height={`${selectionBox.height}px`}
            ></div>
            {#each handleBoxes as h (h.corner)}
              <div
                class="handle"
                class:ns={h.corner === "nw" || h.corner === "se"}
                style:left={`${h.x - HANDLE_PX / 2}px`}
                style:top={`${h.y - HANDLE_PX / 2}px`}
              ></div>
            {/each}
          {/if}
          {#each flashes as f (f.key)}
            <div
              class="flash"
              style:left={`${stageOrigin[0] + f.at[0] * ptScale}px`}
              style:top={`${stageOrigin[1] + f.at[1] * ptScale}px`}
              style:width={`${f.size[0] * ptScale}px`}
              style:height={`${f.size[1] * ptScale}px`}
            ></div>
          {/each}
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

    {#if presenting}
      <BoardPresentChrome
        pageLabel={render?.pages[page] ?? "…"}
        {page}
        pageCount={render?.pageCount ?? 1}
        faded={!chromeVisible}
        {notesOpen}
        notes={board?.pages[page]?.notes ?? null}
        onstep={step}
        onexit={exitPresent}
        ontogglenotes={() => (notesOpen = !notesOpen)}
      />
    {:else}
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
        <span class="bar-spacer"></span>
        {#if toast !== null}
          <span class="toast">{toast}</span>
        {/if}
        {#if saving}
          <span class="save-state">saving…</span>
        {:else if saveError !== null}
          <span class="save-state err" title={saveError}>{midTruncate(saveError, 60)}</span>
        {/if}
        <button class="nav wide" onclick={enterPresent} aria-label="present" title="present"
          >present</button
        >
      </div>

      {#if warnings.length > 0}
        <div class="diags">
          {#each warnings as w (w.rendered)}
            <div class="diag" class:err={w.severity === "error"}>{w.rendered}</div>
          {/each}
        </div>
      {/if}
    {/if}
  </div>

  {#if !presenting}
    <BoardRail
      title={board?.title ?? "board"}
      objects={pageObjects}
      {selected}
      onselect={(id) => (selected = id)}
      oncommitfield={commitField}
    />
  {/if}
</div>

<style>
  .board-view {
    position: absolute;
    inset: 0;
    display: flex;
    background: var(--term-bg);
    overflow: hidden;
  }
  .board-view:focus {
    outline: none;
  }
  .stage-wrap {
    position: relative;
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
  .presenting .stage {
    padding: 0;
  }
  .presenting .stage img {
    border: none;
    border-radius: 0;
    box-shadow: none;
  }
  .chrome-hidden .stage {
    cursor: none;
  }
  .selection {
    position: absolute;
    border: 1.5px solid var(--accent);
    border-radius: 2px;
    pointer-events: none;
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent) 30%, transparent);
  }
  .handle {
    position: absolute;
    width: 8px;
    height: 8px;
    background: var(--term-bg);
    border: 1.5px solid var(--accent);
    border-radius: 1px;
    /* Receives the pointer only for the cursor hint — hit-testing is
       coordinate-based in the stage's own handler, and the events bubble. */
    pointer-events: auto;
    cursor: nesw-resize;
  }
  .handle.ns {
    cursor: nwse-resize;
  }
  .flash {
    position: absolute;
    border: 1.5px solid var(--accent);
    border-radius: 2px;
    pointer-events: none;
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent) 30%, transparent);
    animation: flash-fade 1.2s ease-out forwards;
  }
  @keyframes flash-fade {
    from {
      opacity: 1;
    }
    to {
      opacity: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .flash {
      animation: none;
    }
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
  .nav.wide {
    width: auto;
    padding: 0 8px;
    font-size: var(--text-xs);
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
  .bar-spacer {
    flex: 1;
  }
  .toast {
    color: var(--accent);
    font-family: var(--mono);
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
</style>
