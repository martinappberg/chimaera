<script lang="ts">
  /**
   * The board pane: server-rendered raster stage + outline rail + numeric
   * inspector + page navigator, plus present mode, corner-resize handles,
   * an actor-aware undo stack, agent-edit attribution flashes, an in-place
   * text editor (double-click or Enter on a text-bearing object), and the
   * §6.4 selection-as-deixis "send to chat" affordance.
   *
   * Layout truth is server-side — the stage shows exactly the engine's pixels,
   * never a DOM re-layout of the scene. The client parses the board JSON only
   * for *identity and geometry* (hit-testing, the outline, the inspector
   * numbers, editor seeds), which are the file's own literal values, not
   * derived layout. Every mutation routes through POST /board/edit so the
   * canonical byte-stable writer stays the one authority on bytes.
   */
  import { untrack } from "svelte";
  import {
    fsBoardEdit,
    fsBoardExport,
    fsBoardJournalAll,
    fsBoardJournalAppend,
    fsBoardRender,
    midTruncate,
    navigateDownload,
    type BoardExport,
    type BoardExportFormat,
    type BoardJournalOp,
    type BoardRender,
  } from "./files";
  import { retain, release, noteWrite, type FileEntry } from "./fileStore.svelte";
  import { boardNudge } from "./boardEvents";
  import {
    applyFieldSet,
    attributeDiff,
    boardFrames,
    childFrameRect,
    composeBoardContext,
    configSig,
    CORNERS,
    diagramNodeIndex,
    diagramNodeLabel,
    editorFontPx,
    editorTextToNodeLabel,
    editorTextToParagraphs,
    fateCensus,
    GRID_PT,
    hitChild,
    MIN_RESIZE_PT,
    nextPinId,
    pageFrames,
    paragraphsToEditorText,
    parseBoard,
    pinAnchor,
    resizeFrame,
    samePair,
    sameParagraphs,
    snap8,
    snapshotRegion,
    UndoStack,
    unresolvedPins,
    type BoardInfo,
    type ChildFrame,
    type Corner,
    type ExpectedChange,
    type FieldChange,
    type Frame,
    type ObjInfo,
    type ObjSnap,
    type PinInfo,
  } from "./boardInteract";
  import { referenceTarget, workspaceRelative } from "../shared/reference";
  import { matchChord, PINNED, REFERENCE_CHORD } from "../shared/keys";
  import { attachImageToComposer, insertIntoComposer } from "../chat/composerBus";
  import { IMAGE_MAX_BASE64, IMAGE_MAX_DIM, type ImageAttachment } from "../chat/images";
  import { uploadToSession } from "../net/uploads";
  import { copyText } from "../shared/clipboard";
  import BoardPresentChrome from "./BoardPresentChrome.svelte";
  import BoardRail from "./BoardRail.svelte";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
    /** Active workspace root, for the deixis context line's relative path. */
    wsRoot?: string | null;
  }
  let { path, wsRoot = null }: Props = $props();

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
        // The landed render carries fresh childFrames — the committed-pin
        // overlay's job is done (a child drag's outline holds its dropped
        // spot only until the engine's own frame arrives).
        childOverlay = null;
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
    // A path change resets navigation and drops the previous board's pin
    // overlay (refreshPins guards against a stale fetch landing, but the old
    // dots must not linger over the new stage meanwhile).
    void path;
    page = 0;
    pins = [];
    openPin = null;
    pinDraft = null;
    pinMode = false;
    selectedChild = null;
    childOverlay = null;
    // The export popover is board-scoped: close it and drop the previous
    // board's preflight (its key would miss anyway; the state must not
    // flash the old board's fates over the new one).
    exportOpen = false;
    preflight = null;
    preflightKey = "";
    exportError = null;
    exportSeq++;
    exportBusy = false;
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

  // --- composite children (click-through selection) -----------------------
  /** The selected composite's drilled-into child (a derived id like
   *  `flow/too-hot`), or null when the selection is the object itself. The
   *  click-through idiom: the first press selects the composite, a second
   *  press inside it selects the child under the pointer. */
  let selectedChild = $state<string | null>(null);
  /** A committed child pin's optimistic frame — holds the outline at the
   *  dropped spot for the beat between pointer-up and the re-render whose
   *  childFrames carry the engine's own placement. */
  let childOverlay = $state<{ id: string; frame: Frame } | null>(null);

  /** The render's hit-test map: composite id → derived children + frames. */
  const pageChildFrames = $derived.by<Record<string, ChildFrame[]>>(
    () => render?.childFrames ?? {},
  );
  const selectedChildren = $derived(selected !== null ? (pageChildFrames[selected] ?? []) : []);

  /** The selected child's current frame (overlay first, then the render's). */
  const selectedChildFrame = $derived.by<Frame | null>(() => {
    const id = selectedChild;
    if (id === null) return null;
    const ov = childOverlay;
    if (ov !== null && ov.id === id) return ov.frame;
    const kid = selectedChildren.find((c) => c.id === id);
    return kid !== undefined ? childFrameRect(kid) : null;
  });

  // --- comment pins (§6.4: journal-only, never the board file) ------------
  /** Unresolved pins reduced from GET /board/journal. */
  let pins = $state<PinInfo[]>([]);
  /** Next pin id to mint (`c<max+1>` across everything the journal holds). */
  let nextPin = $state("c1");
  /** The pin tool: while armed, a stage press drops a pin instead of
   *  selecting/dragging. */
  let pinMode = $state(false);
  /** An armed press's pending pin — where it points, awaiting its text. */
  let pinDraft = $state<{ at: [number, number]; object: string | null } | null>(null);
  let pinDraftText = $state("");
  let pinInputEl = $state<HTMLInputElement | null>(null);
  /** Pin id whose popover is open. */
  let openPin = $state<string | null>(null);
  let pinBusy = $state(false);
  let pinFetchSeq = 0;

  const currentPageId = $derived(board?.pages[page]?.id ?? `page-${page + 1}`);
  const pagePins = $derived(pins.filter((p) => p.page === currentPageId));

  async function refreshPins(): Promise<void> {
    const p = path;
    const seq = ++pinFetchSeq;
    try {
      const events = await fsBoardJournalAll(p);
      if (seq !== pinFetchSeq || p !== path) return;
      pins = unresolvedPins(events);
      nextPin = nextPinId(events);
    } catch {
      // The overlay is best-effort — keep the last-known pins.
    }
  }

  // Pins refetch on the same signals as the stage render (path + on-disk
  // change) PLUS the board-epoch nudge: a journal append — another window's
  // pin, a CLI comment — moves no file mtime, so only the epoch frame
  // carries it.
  $effect(() => {
    void path;
    void entry?.mtime;
    void $boardNudge;
    void refreshPins();
  });

  // Autofocus the draft input on open — the press that dropped the pin
  // focused the pane root, not the input.
  $effect(() => {
    pinInputEl?.focus();
  });

  function cancelPinDraft(): void {
    pinDraft = null;
    pinDraftText = "";
    pinMode = false;
    rootEl?.focus({ preventScroll: true });
  }

  async function commitPinDraft(): Promise<void> {
    const d = pinDraft;
    const text = pinDraftText.trim();
    if (d === null || pinBusy) return;
    if (text === "") {
      cancelPinDraft();
      return;
    }
    const id = nextPin;
    const op: BoardJournalOp = {
      event: "comment",
      page: currentPageId,
      ...(d.object !== null ? { object: d.object } : {}),
      at: d.at,
      pin: id,
      text,
    };
    pinBusy = true;
    try {
      await fsBoardJournalAppend(path, op);
      cancelPinDraft();
      showToast(`pinned ${id}${d.object !== null ? ` on ${d.object}` : ""}`);
      void refreshPins();
    } catch (err) {
      showToast(err instanceof Error ? err.message : "pin failed");
    } finally {
      pinBusy = false;
    }
  }

  function onPinDraftKey(ev: KeyboardEvent): void {
    // The input owns its keys, like the text editor: nothing bubbles to the
    // pane/window handlers while typing.
    ev.stopPropagation();
    if (ev.key === "Escape") {
      ev.preventDefault();
      cancelPinDraft();
    } else if (ev.key === "Enter") {
      ev.preventDefault();
      void commitPinDraft();
    }
  }

  async function resolvePin(id: string): Promise<void> {
    if (pinBusy) return;
    pinBusy = true;
    try {
      await fsBoardJournalAppend(path, { event: "comment-resolved", pin: id });
      openPin = null;
      showToast(`resolved ${id}`);
      void refreshPins();
    } catch (err) {
      showToast(err instanceof Error ? err.message : "resolve failed");
    } finally {
      pinBusy = false;
    }
  }

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

  /** Board-point coordinates of a pointer/mouse event on the stage. */
  function toPt(ev: MouseEvent): [number, number] {
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
      }
    | {
        // A drilled-into diagram node: commits `nodes.<i>.at` on the PARENT
        // (the pin the layout honors), never a frame of its own.
        mode: "child-move";
        parent: string;
        childId: string;
        startPt: [number, number];
        origAt: [number, number];
        origSize: [number, number];
        dx: number;
        dy: number;
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
    // A drilled-into child owns the selection: children are not resizable
    // (their size is layout-derived), and parent handles would read as the
    // child's.
    if (selectedChild !== null) return [];
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
    // While the text editor is open the stage underneath is inert: no drag,
    // resize, or reselection may start beneath it. A press outside the
    // textarea still blurs it, which commits and closes — the next press
    // interacts normally.
    if (ev.button !== 0 || presenting || textEdit !== null) return;
    // Focus scoping for the undo keys: only the pane the user is working in
    // may answer ⌘Z (multiple board panes, terminals, editors coexist).
    rootEl?.focus({ preventScroll: true });
    const pt = toPt(ev);
    // Any stage press dismisses an open pin popover (its own chrome stops
    // propagation, so this only fires for presses outside it).
    openPin = null;
    if (pinMode) {
      // An armed press drops the pin — object-bound when it lands on one —
      // and opens the text input; it never selects or starts a drag. A
      // re-press while the input is open just re-drops the draft. Cancel the
      // press's default so the compatibility mousedown cannot steal focus
      // from the input the autofocus effect is about to give it.
      ev.preventDefault();
      const target = hit(pt);
      pinDraft = { at: [Math.round(pt[0]), Math.round(pt[1])], object: target?.id ?? null };
      pinDraftText = "";
      return;
    }
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
    // Click-through into a composite's children: a press inside the ALREADY
    // selected composite selects the derived child under the pointer (a
    // node, a lane box) — and a node child arms a drag whose release pins
    // `nodes.<i>.at`. A press on the composite's background clears the child
    // and falls through to the normal whole-object selection/drag.
    if (target !== null && target.id === selected) {
      const kid = hitChild(pageChildFrames[target.id] ?? [], pt);
      if (kid !== null) {
        selectedChild = kid.id;
        childOverlay = null;
        if (diagramNodeIndex(target, kid.id) !== null) {
          (ev.currentTarget as HTMLElement).setPointerCapture(ev.pointerId);
          drag = {
            mode: "child-move",
            parent: target.id,
            childId: kid.id,
            startPt: pt,
            origAt: [kid.frame[0], kid.frame[1]],
            origSize: [kid.frame[2], kid.frame[3]],
            dx: 0,
            dy: 0,
            moved: false,
          };
        }
        return;
      }
    }
    selected = target?.id ?? null;
    selectedChild = null;
    childOverlay = null;
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
    if (d.mode === "move" || d.mode === "child-move") {
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
    } else if (d.mode === "child-move") {
      const at: [number, number] = [snap8(d.origAt[0] + d.dx), snap8(d.origAt[1] + d.dy)];
      if (samePair(at, d.origAt)) return;
      const parent = pageObjects.find((o) => o.id === d.parent);
      if (parent === undefined) return;
      // Resolve the node index from the child id at RELEASE time against the
      // parent's current nodes array — the id is the stable anchor, the
      // index is not (an agent may have edited the diagram mid-drag).
      const idx = diagramNodeIndex(parent, d.childId);
      if (idx === null) return;
      const set = { [`nodes.${idx}.at`]: at };
      childOverlay = { id: d.childId, frame: { at, size: d.origSize } };
      // Child pins stay OFF the §6.7 undo stack, like every set-based edit:
      // its actor-rule staleness check is frame-based, and the journal's
      // restyle event is the audit trail.
      void commit(d.parent, { set }, configSig(applyFieldSet(parent.raw, set)));
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
    change: {
      at?: [number, number];
      size?: [number, number];
      text?: string[];
      set?: Record<string, unknown>;
    },
    /** Predicted post-write config fingerprint, for `set` commits only —
     *  computed by the caller from the object's raw JSON before the write. */
    expectedSig?: string,
  ): Promise<void> {
    commitChain = commitChain.then(async () => {
      saving = true;
      saveError = null;
      try {
        const mtime = await fsBoardEdit(path, id, change);
        // Geometry is expected by value, config by fingerprint; text edits
        // deliberately carry no expectation (configSig excludes text). Merge
        // with any still-pending expectation for the object so a config
        // commit never clobbers a pending geometry one, or vice versa.
        const expected: ExpectedChange = { ...(ownExpected.get(id) ?? {}) };
        if (change.at !== undefined) expected.at = change.at;
        if (change.size !== undefined) expected.size = change.size;
        if (expectedSig !== undefined) expected.sig = expectedSig;
        if (expected.at !== undefined || expected.size !== undefined || expected.sig !== undefined)
          ownExpected.set(id, expected);
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

  /** Inspector config commit (the chart section): one sparse `set` on the
   *  selected object, riding the same commit chain. The expected fingerprint
   *  is the client's own prediction of the saved config, so the refresh this
   *  write causes attributes as ours — no flash. Config edits stay off the
   *  §6.7 undo stack (its staleness rule is frame-based, like text edits);
   *  the journal's restyle event remains the audit trail. */
  function commitSet(set: Record<string, unknown>): void {
    const o = selectedObj;
    if (o === null) return;
    const expectedSig = configSig(applyFieldSet(o.raw, set));
    void commit(o.id, { set }, expectedSig);
  }

  // --- in-place text editing ----------------------------------------------
  /** The open inline editor: which object, the live textarea value, and the
   *  seed paragraphs (the no-change gate + the font approximation's line
   *  count). Editing is offered only where `ObjInfo.text` is non-null — the
   *  kinds the /board/edit text op accepts — plus diagram NODE children,
   *  where `child` names the derived id and the commit rewrites the parent's
   *  `nodes.<i>.label` via the set op. */
  let textEdit = $state<{
    id: string;
    value: string;
    seed: string[];
    child?: { id: string };
  } | null>(null);
  let editorEl = $state<HTMLTextAreaElement | null>(null);

  function beginTextEdit(o: ObjInfo): void {
    if (presenting || o.text === null || o.at === null || o.size === null) return;
    selected = o.id;
    selectedChild = null;
    drag = null;
    textEdit = { id: o.id, value: paragraphsToEditorText(o.text), seed: o.text };
  }

  /** The overlay editor over a diagram node's frame, seeded with its stored
   *  label — same chrome, same Esc/⌘Enter semantics as the object editor. */
  function beginChildLabelEdit(parent: ObjInfo, childId: string): void {
    if (presenting) return;
    const idx = diagramNodeIndex(parent, childId);
    if (idx === null) return;
    const label = diagramNodeLabel(parent, idx);
    if (label === null) return;
    selected = parent.id;
    selectedChild = childId;
    drag = null;
    textEdit = { id: parent.id, value: label, seed: [label], child: { id: childId } };
  }

  /**
   * Close the editor. `commitValue` false is the Esc cancel; `refocus` is
   * keyboard-close only — a blur-driven close must not yank focus back from
   * wherever the user just clicked. Nulls the state FIRST so the blur that
   * follows a keyboard close cannot double-commit. A no-change commit sends
   * nothing (rich styled runs survive exactly by not being rewritten).
   */
  function closeTextEdit(commitValue: boolean, refocus = false): void {
    const ed = textEdit;
    if (ed === null) return;
    textEdit = null;
    if (refocus) rootEl?.focus({ preventScroll: true });
    if (!commitValue) return;
    if (ed.child !== undefined) {
      // A node label commit: `nodes.<i>.label` on the parent via the set op,
      // index re-resolved from the stable child id at commit time. The
      // predicted fingerprint makes the refresh attribute as ours (no
      // flash); like text edits, it stays off the frame-based undo stack.
      const parent = pageObjects.find((o) => o.id === ed.id);
      if (parent === undefined) return;
      const idx = diagramNodeIndex(parent, ed.child.id);
      if (idx === null) return;
      const label = editorTextToNodeLabel(ed.value);
      if (label === ed.seed[0]) return;
      const set = { [`nodes.${idx}.label`]: label };
      void commit(ed.id, { set }, configSig(applyFieldSet(parent.raw, set)));
      return;
    }
    const paras = editorTextToParagraphs(ed.value);
    if (sameParagraphs(paras, ed.seed)) return;
    // Text edits stay off the undo stack: its actor-rule staleness check is
    // frame-based (§6.7); the journal still records the TextEdited event.
    void commit(ed.id, { text: paras });
  }

  function onEditorKey(ev: KeyboardEvent): void {
    // The editor owns its keys — nothing bubbles to the pane/window handlers
    // (undo chord, Enter-to-edit, app chords) while typing. Plain Enter falls
    // through to the textarea's native newline.
    ev.stopPropagation();
    if (ev.key === "Escape") {
      ev.preventDefault();
      closeTextEdit(false, true);
    } else if (ev.key === "Enter" && (ev.metaKey || ev.ctrlKey)) {
      ev.preventDefault();
      closeTextEdit(true, true);
    }
  }

  function onDblClick(ev: MouseEvent): void {
    if (presenting || textEdit !== null) return;
    const pt = toPt(ev);
    const target = hit(pt);
    // A node child takes the double-click when it's under the pointer of the
    // already-selected composite (the two presses before dblclick did the
    // drill-in) — the overlay editor opens over the node's frame.
    if (target !== null && target.id === selected) {
      const kid = hitChild(pageChildFrames[target.id] ?? [], pt);
      if (kid !== null && diagramNodeIndex(target, kid.id) !== null) {
        beginChildLabelEdit(target, kid.id);
        return;
      }
    }
    if (target === null || target.text === null) return;
    beginTextEdit(target);
  }

  // The editor's overlay frame in stage pixels, tracking the object's own
  // literal geometry (an agent moving it mid-edit moves the editor with it).
  // A node child's editor tracks the render's child frame instead — the
  // engine's own layout, the only geometry a derived child has.
  const editorBox = $derived.by(() => {
    const ed = textEdit;
    if (ed === null) return null;
    if (ed.child !== undefined) {
      const childId = ed.child.id;
      const kid = (pageChildFrames[ed.id] ?? []).find((c) => c.id === childId);
      if (kid === undefined) return null;
      const f = childFrameRect(kid);
      return {
        left: stageOrigin[0] + f.at[0] * ptScale,
        top: stageOrigin[1] + f.at[1] * ptScale,
        width: Math.max(48, f.size[0] * ptScale),
        height: Math.max(28, f.size[1] * ptScale),
        font: editorFontPx(f.size, ptScale, 1),
      };
    }
    const o = pageObjects.find((x) => x.id === ed.id);
    if (o === undefined || o.at === null || o.size === null) return null;
    return {
      left: stageOrigin[0] + o.at[0] * ptScale,
      top: stageOrigin[1] + o.at[1] * ptScale,
      width: Math.max(48, o.size[0] * ptScale),
      height: Math.max(28, o.size[1] * ptScale),
      font: editorFontPx(o.size, ptScale, Math.max(1, ed.seed.length)),
    };
  });

  // Autofocus on open, caret at the end (the seed is existing prose, not a
  // field to overtype).
  $effect(() => {
    const el = editorEl;
    if (el === null) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  });

  // If the edited object vanishes (agent deleted it, page switched under a
  // clamp), drop the editor without committing — there is nothing to anchor
  // a write to. Reads and writes textEdit, but the null write terminates it.
  $effect(() => {
    const ed = textEdit;
    if (ed !== null && !pageObjects.some((o) => o.id === ed.id)) textEdit = null;
  });

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

  // --- selection-as-deixis (§6.4) -----------------------------------------
  /** The reference target when it is a chat session: composerBus can insert
   *  into a mounted (or mounting) chat Composer. */
  const chatTarget = $derived(
    $referenceTarget !== null && $referenceTarget.ui === "chat" ? $referenceTarget : null,
  );
  /** …and when it is a terminal session: Chimaera never types into a TUI's
   *  PTY, so the affordance degrades to "copy snapshot path" — same region
   *  snapshot, landed in the session's upload dir, its context line + @path
   *  on the clipboard for the user to paste themselves. */
  const termTarget = $derived(
    $referenceTarget !== null && $referenceTarget.ui === "term" ? $referenceTarget : null,
  );
  /** What the deixis affordances point at: the drilled-into child (its
   *  derived id + engine frame) when one is selected, else the object itself
   *  — so "chat" carries `[board: … › flow/too-hot]` and the snapshot crops
   *  to the node, not the whole diagram. */
  const deixisTarget = $derived.by<{ id: string; frame: Frame } | null>(() => {
    const o = selectedObj;
    if (o === null) return null;
    if (selectedChild !== null) {
      const f = selectedChildFrame;
      return f !== null ? { id: selectedChild, frame: f } : null;
    }
    return o.at !== null && o.size !== null ? { id: o.id, frame: { at: o.at, size: o.size } } : null;
  });
  const sendableFrame = $derived(deixisTarget !== null);
  let snapshotBusy = $state(false);
  const canSendToChat = $derived(
    chatTarget !== null && sendableFrame && board !== null && !snapshotBusy,
  );
  const canCopyForTerm = $derived(
    chatTarget === null && termTarget !== null && sendableFrame && board !== null && !snapshotBusy,
  );
  const sendTitle = $derived.by(() => {
    if (chatTarget === null && termTarget === null)
      return "no agent session to send to — open an agent";
    if (!sendableFrame) return "select an object on the stage first";
    if (chatTarget !== null) return `send selection to ${chatTarget.name} (${PINNED.reference})`;
    return `copy a snapshot path for ${termTarget?.name} (${PINNED.reference}) — chimaera never types into a TUI`;
  });

  /**
   * Crop the selection's padded bounds out of the server's own page render.
   * The render request matches the stage's (content-addressed server cache →
   * a stat + ticket mint, not a re-render); /board/render has no region
   * parameter today, so the object scoping happens here — still exclusively
   * the engine's pixels, never a DOM re-layout. Caps mirror chat/images.ts.
   */
  async function snapshotCanvas(region: Frame): Promise<HTMLCanvasElement | null> {
    const scale = Math.min(4, Math.max(1, window.devicePixelRatio || 1));
    const r = await fsBoardRender(path, page, scale);
    const resp = await fetch(`/raw/${r.ticket}`);
    if (!resp.ok) throw new Error(`snapshot fetch failed (${resp.status})`);
    const bitmap = await createImageBitmap(await resp.blob());
    const b = board;
    if (b === null) return null;
    const pxPerPt = bitmap.width / b.canvas[0];
    const sx = Math.max(0, Math.floor(region.at[0] * pxPerPt));
    const sy = Math.max(0, Math.floor(region.at[1] * pxPerPt));
    const sw = Math.min(bitmap.width - sx, Math.ceil(region.size[0] * pxPerPt));
    const sh = Math.min(bitmap.height - sy, Math.ceil(region.size[1] * pxPerPt));
    if (sw < 1 || sh < 1) return null;
    const shrink = Math.min(1, IMAGE_MAX_DIM / Math.max(sw, sh));
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(sw * shrink));
    canvas.height = Math.max(1, Math.round(sh * shrink));
    canvas.getContext("2d")?.drawImage(bitmap, sx, sy, sw, sh, 0, 0, canvas.width, canvas.height);
    return canvas;
  }

  /** The chat half: the crop as a base64 composer attachment. */
  async function snapshotAttachment(region: Frame, label: string): Promise<ImageAttachment | null> {
    const canvas = await snapshotCanvas(region);
    if (canvas === null) return null;
    const url = canvas.toDataURL("image/png");
    const data = url.slice(url.indexOf(",") + 1);
    if (data.length > IMAGE_MAX_BASE64) return null;
    return { media_type: "image/png", data, label: `${label} ${canvas.width}×${canvas.height}` };
  }

  /**
   * §6.4: push the compact context line + an object-scoped region snapshot
   * into the target chat composer via composerBus. The context line goes
   * even when the snapshot pipeline hiccups — pointing still works without
   * pixels, and the toast says which happened.
   */
  async function sendSelectionToChat(): Promise<void> {
    const b = board;
    const d = deixisTarget;
    const target = chatTarget;
    if (b === null || d === null || target === null) return;
    if (snapshotBusy) return;
    const pageId = b.pages[page]?.id ?? `page-${page + 1}`;
    const rel = wsRoot !== null ? workspaceRelative(path, wsRoot) : path;
    const context = composeBoardContext(rel, pageId, [d.id]);
    const region = snapshotRegion([d.frame], b.canvas);
    snapshotBusy = true;
    let attachment: ImageAttachment | null = null;
    let failed = false;
    try {
      if (region !== null) attachment = await snapshotAttachment(region, `board ${d.id}`);
    } catch {
      failed = true;
    } finally {
      snapshotBusy = false;
    }
    insertIntoComposer(target.id, context);
    if (attachment !== null) attachImageToComposer(target.id, attachment);
    showToast(
      attachment !== null
        ? `sent ${d.id} to ${target.name}`
        : `sent ${d.id} to ${target.name}${failed ? " — snapshot failed" : " (no snapshot)"}`,
    );
  }

  /**
   * §6.4's TUI fallback: the same region snapshot, but nothing is ever typed
   * into a PTY — the crop lands in the session's upload dir (the same
   * landing pad OS drops use) and the context line + @path go to the
   * clipboard for the user to paste themselves.
   */
  async function copySnapshotPathForTerm(): Promise<void> {
    const b = board;
    const d = deixisTarget;
    const target = termTarget;
    if (b === null || d === null || target === null) return;
    if (snapshotBusy) return;
    const pageId = b.pages[page]?.id ?? `page-${page + 1}`;
    const rel = wsRoot !== null ? workspaceRelative(path, wsRoot) : path;
    const context = composeBoardContext(rel, pageId, [d.id]);
    const region = snapshotRegion([d.frame], b.canvas);
    if (region === null) return;
    snapshotBusy = true;
    try {
      const canvas = await snapshotCanvas(region);
      const blob =
        canvas === null
          ? null
          : await new Promise<Blob | null>((res) => canvas.toBlob(res, "image/png"));
      if (blob === null) {
        showToast("snapshot failed");
        return;
      }
      const stamp = new Date()
        .toISOString()
        .replace(/[-:]/g, "")
        .replace(/\..+$/, "")
        .replace("T", "-");
      // A derived id carries its parent's `/` — flattened for the upload's
      // strict basename sanitize.
      const safe = d.id.replace(/[/\\]/g, "-");
      const upload = await uploadToSession(target.id, blob, `board-${safe}-${stamp}.png`);
      const copied = await copyText(`${context}@${upload.path}`);
      showToast(
        copied
          ? `copied snapshot path for ${target.name}`
          : "snapshot uploaded — clipboard unavailable",
      );
    } catch (err) {
      showToast(err instanceof Error ? err.message : "snapshot upload failed");
    } finally {
      snapshotBusy = false;
    }
  }

  /** One affordance, two targets: the chat composer, or the TUI clipboard. */
  function sendSelection(): void {
    if (canSendToChat) void sendSelectionToChat();
    else if (canCopyForTerm) void copySnapshotPathForTerm();
  }

  // Pane-local keys, same focus-containment pattern as the undo chord: Enter
  // opens the inline editor on a selected text-bearing object; the pinned
  // reference chord (⇧⌘R) sends the selection to the chat composer. Interactive
  // targets keep their own Enter, and a defaultPrevented chord means the
  // app-level reference bridge already claimed it (a live text selection).
  $effect(() => {
    const onKey = (ev: KeyboardEvent): void => {
      if (presenting || textEdit !== null) return;
      const root = rootEl;
      if (root === null || !root.contains(document.activeElement)) return;
      const t = ev.target;
      if (
        t instanceof HTMLElement &&
        (t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA" ||
          t.tagName === "BUTTON" ||
          t.tagName === "SELECT" ||
          t.isContentEditable)
      )
        return;
      if (ev.key === "Enter" && !ev.metaKey && !ev.ctrlKey && !ev.altKey && !ev.shiftKey) {
        const o = selectedObj;
        // A drilled-into node child takes Enter first — its label opens in
        // the same overlay editor a double-click would.
        if (o !== null && selectedChild !== null && diagramNodeIndex(o, selectedChild) !== null) {
          ev.preventDefault();
          beginChildLabelEdit(o, selectedChild);
          return;
        }
        if (o !== null && o.text !== null && o.at !== null && o.size !== null) {
          ev.preventDefault();
          beginTextEdit(o);
        }
        return;
      }
      if (
        matchChord(ev, REFERENCE_CHORD) !== null &&
        !ev.defaultPrevented &&
        (canSendToChat || canCopyForTerm)
      ) {
        ev.preventDefault();
        sendSelection();
      }
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
    frames: Map<string, ObjSnap>;
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

  // --- export popover (§11: the fidelity preflight) ------------------------
  /** The route's format vocabulary exactly; only the label softens the id. */
  const EXPORT_FORMATS: { id: BoardExportFormat; label: string }[] = [
    { id: "pptx", label: "pptx" },
    { id: "pdf", label: "pdf" },
    { id: "svg", label: "svg" },
    { id: "svg-outlined", label: "svg outlined" },
  ];
  let exportOpen = $state(false);
  let exportFormat = $state<BoardExportFormat>("pptx");
  /** The CLI's `--charts native`: pptx-only, opt-in until the plan's
   *  hand-verified "Edit Data opens" pass in desktop PowerPoint. */
  let exportNative = $state(false);
  let exportBusy = $state(false);
  let exportError = $state<string | null>(null);
  /** The one pptx export the open popover performed: its `objects` ARE the
   *  preflight and its ticket IS what the download button consumes — the
   *  fates are never bought with a second export of the same configuration. */
  let preflight = $state<BoardExport | null>(null);
  /** What the current `preflight` was exported from (path·native·mtime), so
   *  switching formats away and back never re-exports unchanged input. */
  let preflightKey = "";
  /** When it landed: a download ticket lives ~10 min server-side, so a
   *  preflight older than the memo window re-exports rather than handing the
   *  download button a dead ticket (same margin as files.ts's ticket memo). */
  let preflightAt = 0;
  const PREFLIGHT_TTL_MS = 8 * 60 * 1000;
  let exportSeq = 0;
  let exportPopEl = $state<HTMLDivElement | null>(null);
  let exportChipEl = $state<HTMLButtonElement | null>(null);

  function closeExport(): void {
    exportOpen = false;
    exportError = null;
    exportSeq++;
    exportBusy = false;
  }

  // The pptx preflight: opening the popover on pptx (or toggling native
  // charts, or the board changing on disk underneath) performs THE export,
  // so the fates shown are the file the ticket downloads — §11's "stated
  // before you export", with the statement costing no extra export.
  $effect(() => {
    if (!exportOpen || exportFormat !== "pptx") return;
    const p = path;
    const native = exportNative;
    const key = `${p} ${native} ${entry?.mtime ?? ""}`;
    if (preflight !== null && key === preflightKey && Date.now() - preflightAt < PREFLIGHT_TTL_MS)
      return;
    const seq = ++exportSeq;
    exportBusy = true;
    exportError = null;
    fsBoardExport(p, "pptx", native).then(
      (r) => {
        if (seq !== exportSeq) return;
        preflight = r;
        preflightKey = key;
        preflightAt = Date.now();
        exportBusy = false;
      },
      (err: unknown) => {
        if (seq !== exportSeq) return;
        preflight = null;
        exportError = err instanceof Error ? err.message : String(err);
        exportBusy = false;
      },
    );
  });

  /** Download the selected format: pptx consumes the preflight's own ticket
   *  (that export already happened); pdf/svg export now, then download. */
  async function downloadExport(): Promise<void> {
    if (exportBusy) return;
    if (exportFormat === "pptx") {
      const p = preflight;
      if (p === null) return;
      navigateDownload(p.ticket);
      showToast(`downloading ${p.filename}`);
      closeExport();
      return;
    }
    exportBusy = true;
    exportError = null;
    try {
      const r = await fsBoardExport(path, exportFormat);
      navigateDownload(r.ticket);
      showToast(`downloading ${r.filename}`);
      closeExport();
    } catch (err) {
      exportError = err instanceof Error ? err.message : String(err);
      exportBusy = false;
    }
  }

  // The popover dismisses like the pin popover: Escape, or a press outside
  // its own chrome (the chip toggles itself).
  $effect(() => {
    if (!exportOpen) return;
    const onKey = (ev: KeyboardEvent): void => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        closeExport();
        exportChipEl?.focus();
      }
    };
    const onPress = (ev: PointerEvent): void => {
      const t = ev.target;
      if (!(t instanceof Node)) return;
      if (exportPopEl?.contains(t) === true || exportChipEl?.contains(t) === true) return;
      closeExport();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("pointerdown", onPress);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("pointerdown", onPress);
    };
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
    pinMode = false;
    pinDraft = null;
    openPin = null;
    closeExport();
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
    if (d !== null && d.mode === "move" && d.id === o.id) {
      at = [o.at[0] + d.dx, o.at[1] + d.dy];
    } else if (d !== null && d.mode === "resize" && d.id === o.id) {
      at = d.cur.at;
      size = d.cur.size;
    }
    return {
      left: stageOrigin[0] + at[0] * ptScale,
      top: stageOrigin[1] + at[1] * ptScale,
      width: size[0] * ptScale,
      height: size[1] * ptScale,
    };
  });

  /** The drilled-into child's outline in stage pixels, tracking an in-flight
   *  child drag exactly like the parent box tracks a move. */
  const childBox = $derived.by(() => {
    const f = selectedChildFrame;
    if (f === null) return null;
    const d = drag;
    let at = f.at;
    if (d !== null && d.mode === "child-move" && d.childId === selectedChild) {
      at = [f.at[0] + d.dx, f.at[1] + d.dy];
    }
    return {
      left: stageOrigin[0] + at[0] * ptScale,
      top: stageOrigin[1] + at[1] * ptScale,
      width: f.size[0] * ptScale,
      height: f.size[1] * ptScale,
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
      class:pinning={pinMode && !presenting}
      role="presentation"
      bind:this={stageEl}
      onpointerdown={onPointerDown}
      onpointermove={onPointerMove}
      onpointerup={onPointerUp}
      onpointercancel={() => (drag = null)}
      ondblclick={onDblClick}
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
            <!-- With a child drilled into, the parent demotes to a dashed
                 containment hint and the child wears the selection. -->
            <div
              class="selection"
              class:parent={selectedChild !== null}
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
            {#if childBox !== null}
              <div
                class="selection"
                style:left={`${childBox.left}px`}
                style:top={`${childBox.top}px`}
                style:width={`${childBox.width}px`}
                style:height={`${childBox.height}px`}
              ></div>
            {/if}
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
          <!-- Comment pins (§6.4): numbered dots from the journal, never the
               board file. Object-bound dots ride the object's frame corner so
               they track moves; point pins sit at their stored point. Their
               chrome stops pointer propagation so a click never selects or
               drags the stage underneath. -->
          {#each pagePins as p, i (p.pin)}
            {@const anchor = pinAnchor(p, pageObjects)}
            {#if anchor !== null}
              <button
                class="pin-dot"
                class:open={openPin === p.pin}
                style:left={`${stageOrigin[0] + anchor[0] * ptScale - 9}px`}
                style:top={`${stageOrigin[1] + anchor[1] * ptScale - 9}px`}
                aria-label={`comment ${p.pin}: ${p.text}`}
                title={p.text}
                onpointerdown={(e) => e.stopPropagation()}
                ondblclick={(e) => e.stopPropagation()}
                onclick={() => (openPin = openPin === p.pin ? null : p.pin)}
              >{i + 1}</button>
              {#if openPin === p.pin}
                <div
                  class="pin-pop"
                  role="presentation"
                  style:left={`${stageOrigin[0] + anchor[0] * ptScale + 12}px`}
                  style:top={`${stageOrigin[1] + anchor[1] * ptScale + 12}px`}
                  onpointerdown={(e) => e.stopPropagation()}
                  ondblclick={(e) => e.stopPropagation()}
                >
                  <div class="pin-pop-text">{p.text}</div>
                  <div class="pin-pop-bar">
                    <span class="pin-pop-meta"
                      >{p.pin} · {p.actor}{p.object !== null ? ` · ${p.object}` : ""}</span
                    >
                    <button
                      class="pin-resolve"
                      disabled={pinBusy}
                      onclick={() => void resolvePin(p.pin)}>resolve</button
                    >
                  </div>
                </div>
              {/if}
            {/if}
          {/each}
          {#if pinDraft !== null}
            <div
              class="pin-draft"
              role="presentation"
              style:left={`${stageOrigin[0] + pinDraft.at[0] * ptScale}px`}
              style:top={`${stageOrigin[1] + pinDraft.at[1] * ptScale}px`}
              onpointerdown={(e) => e.stopPropagation()}
              ondblclick={(e) => e.stopPropagation()}
            >
              <input
                bind:this={pinInputEl}
                bind:value={pinDraftText}
                maxlength="500"
                placeholder={pinDraft.object !== null
                  ? `comment on ${pinDraft.object}…`
                  : "comment…"}
                aria-label="comment pin text (Enter pins, Esc cancels)"
                spellcheck="false"
                onkeydown={onPinDraftKey}
              />
            </div>
          {/if}
          {#if textEdit !== null && editorBox !== null}
            <!-- The overlay covers the object's rendered text at the same
                 frame; the styling is deliberately editor-chrome, not a
                 WYSIWYG imitation — layout truth stays server-side. -->
            <textarea
              class="text-editor"
              bind:this={editorEl}
              bind:value={textEdit.value}
              style:left={`${editorBox.left}px`}
              style:top={`${editorBox.top}px`}
              style:width={`${editorBox.width}px`}
              style:height={`${editorBox.height}px`}
              style:font-size={`${editorBox.font}px`}
              aria-label="edit object text (Esc cancels, ⌘/Ctrl+Enter commits)"
              spellcheck="false"
              onkeydown={onEditorKey}
              onblur={() => closeTextEdit(true)}
              ondblclick={(e) => e.stopPropagation()}
            ></textarea>
          {/if}
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
        <button
          class="nav wide"
          class:armed={pinMode}
          onclick={() => {
            pinMode = !pinMode;
            if (!pinMode) pinDraft = null;
          }}
          aria-pressed={pinMode}
          aria-label="drop a comment pin"
          title="comment pin: click the stage to drop a numbered note (journal-only — never the board file)"
          >pin</button
        >
        <button
          class="nav wide"
          disabled={!canSendToChat && !canCopyForTerm}
          onclick={sendSelection}
          aria-label={chatTarget !== null ? "send selection to chat" : "copy snapshot path"}
          title={sendTitle}
          >{snapshotBusy
            ? "sending…"
            : chatTarget === null && termTarget !== null
              ? "copy snapshot path"
              : "chat"}</button
        >
        <button
          bind:this={exportChipEl}
          class="nav wide"
          class:armed={exportOpen}
          onclick={() => (exportOpen ? closeExport() : (exportOpen = true))}
          aria-expanded={exportOpen}
          aria-haspopup="dialog"
          aria-label="export board"
          title="export: pptx / pdf / svg — pptx states every object's fidelity fate before you download"
          >export</button
        >
        <button class="nav wide" onclick={enterPresent} aria-label="present" title="present"
          >present</button
        >
        {#if exportOpen}
          <div
            class="export-pop"
            bind:this={exportPopEl}
            role="dialog"
            aria-label="export board"
          >
            <div class="export-formats" role="group" aria-label="export format">
              {#each EXPORT_FORMATS as f (f.id)}
                <button
                  class="fmt"
                  class:sel={exportFormat === f.id}
                  aria-pressed={exportFormat === f.id}
                  onclick={() => (exportFormat = f.id)}
                  >{f.label}</button
                >
              {/each}
            </div>
            {#if exportFormat === "pptx"}
              <label
                class="native-toggle"
                title="native charts (opt-in — Edit Data not yet hand-verified): real c:chart parts where a chart maps cleanly; the rest stay grouped shapes with the reason in their fate"
              >
                <input type="checkbox" bind:checked={exportNative} />
                native charts
              </label>
              {#if exportError === null && preflight !== null}
                {@const fates = preflight.objects ?? []}
                <!-- §11: the fidelity preflight — the degradation contract
                     stated before the deck is opened, from the very export
                     the download button hands over. -->
                <div class="census">
                  {fates.length === 0 ? "no objects" : fateCensus(fates)}
                </div>
                <ul class="fates" aria-label="per-object export fates">
                  {#each fates as f, i (`${i}:${f.id}`)}
                    <li class="fate" title={f.reason}>
                      <span class="fate-id">{f.id}</span>
                      <span class="fate-tier {f.tier}">{f.tier}</span>
                      <span class="fate-reason">{f.reason}</span>
                    </li>
                  {/each}
                </ul>
              {:else if exportBusy}
                <div class="census">exporting…</div>
              {/if}
            {/if}
            {#if exportError !== null}
              <div class="export-err">{exportError}</div>
            {/if}
            <div class="export-bar">
              <button
                class="nav wide"
                disabled={exportBusy || (exportFormat === "pptx" && preflight === null)}
                onclick={() => void downloadExport()}
                >{exportBusy
                  ? "exporting…"
                  : exportFormat === "pptx" && preflight !== null
                    ? `download ${preflight.filename}`
                    : "download"}</button
              >
            </div>
          </div>
        {/if}
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
      childFrames={pageChildFrames}
      {selectedChild}
      catSwatches={render?.catSwatches ?? []}
      onselect={(id) => {
        selected = id;
        selectedChild = null;
        childOverlay = null;
      }}
      onselectchild={(parentId, childId) => {
        selected = parentId;
        selectedChild = childId;
        childOverlay = null;
      }}
      oncommitfield={commitField}
      oncommitset={commitSet}
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
  .selection.parent {
    border-style: dashed;
    border-color: color-mix(in srgb, var(--accent) 55%, transparent);
    box-shadow: none;
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
  .text-editor {
    position: absolute;
    box-sizing: border-box;
    padding: 2px 6px;
    background: var(--term-bg);
    color: var(--fg);
    border: 1.5px solid var(--accent);
    border-radius: 2px;
    outline: none;
    resize: none;
    overflow: auto;
    line-height: 1.3;
    font-family: inherit;
    box-shadow:
      0 0 0 3px color-mix(in srgb, var(--accent) 22%, transparent),
      0 2px 12px var(--scrim);
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
  .stage.pinning {
    cursor: crosshair;
  }
  .pin-dot {
    position: absolute;
    z-index: 2;
    width: 18px;
    height: 18px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: 1.5px solid var(--term-bg);
    border-radius: 50% 50% 50% 4px;
    background: var(--accent);
    color: var(--term-bg);
    font-family: var(--mono);
    font-size: 10px;
    line-height: 1;
    cursor: pointer;
    box-shadow: 0 1px 4px var(--scrim);
  }
  .pin-dot:hover,
  .pin-dot.open {
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 30%, transparent);
  }
  .pin-pop {
    position: absolute;
    z-index: 3;
    min-width: 160px;
    max-width: 260px;
    padding: 8px 10px;
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    box-shadow: 0 4px 16px var(--scrim);
    font-size: var(--text-xs);
    color: var(--fg);
  }
  .pin-pop-text {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .pin-pop-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 6px;
  }
  .pin-pop-meta {
    flex: 1;
    color: var(--muted);
    font-family: var(--mono);
  }
  .pin-resolve {
    background: none;
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--fg);
    padding: 1px 8px;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .pin-resolve:not(:disabled):hover {
    background: var(--row-hover);
  }
  .pin-resolve:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .pin-draft {
    position: absolute;
    z-index: 3;
  }
  .pin-draft input {
    width: 220px;
    padding: 4px 8px;
    background: var(--term-bg);
    color: var(--fg);
    border: 1.5px solid var(--accent);
    border-radius: 4px;
    outline: none;
    font-size: var(--text-xs);
    font-family: inherit;
    box-shadow:
      0 0 0 3px color-mix(in srgb, var(--accent) 22%, transparent),
      0 2px 12px var(--scrim);
  }
  .nav.armed {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--term-bg);
  }
  .nav.armed:not(:disabled):hover {
    background: var(--accent);
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
    /* Anchors the export popover just above the bar, wherever the bar sits
       (the diagnostics strip below may shift the stage-wrap bottom). */
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    border-top: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .export-pop {
    position: absolute;
    z-index: 4;
    right: 8px;
    bottom: calc(100% + 6px);
    width: 300px;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    box-shadow: 0 4px 16px var(--scrim);
    font-size: var(--text-xs);
    color: var(--fg);
  }
  .export-formats {
    display: flex;
    gap: 4px;
  }
  .fmt {
    flex: 1;
    background: none;
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--fg);
    padding: 3px 0;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .fmt:hover {
    background: var(--row-hover);
  }
  .fmt.sel {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--term-bg);
  }
  .native-toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--fg);
    cursor: pointer;
    user-select: none;
  }
  .native-toggle input {
    accent-color: var(--accent);
    margin: 0;
  }
  .census {
    color: var(--muted);
    font-family: var(--mono);
  }
  .fates {
    /* Compact rows; scrolls past ~8 of them. */
    max-height: 176px;
    overflow-y: auto;
    margin: 0;
    padding: 0;
    list-style: none;
    border-top: 1px solid var(--edge);
  }
  .fate {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 3px 0;
    border-bottom: 1px solid var(--edge);
  }
  .fate-id {
    font-family: var(--mono);
    flex-shrink: 0;
    max-width: 96px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .fate-tier {
    font-family: var(--mono);
    flex-shrink: 0;
  }
  .fate-tier.native {
    color: var(--accent);
  }
  .fate-tier.vector,
  .fate-tier.raster {
    color: var(--warn);
  }
  .fate-reason {
    flex: 1;
    min-width: 0;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .export-err {
    color: var(--err);
    overflow-wrap: anywhere;
  }
  .export-bar {
    display: flex;
    justify-content: flex-end;
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
