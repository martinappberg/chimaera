<script lang="ts" module>
  import type { SlideData } from "@aiden0z/pptx-renderer";

  /** EMU per CSS pixel at 96 dpi (the renderer's unit). */
  export const EMU_PER_PX = 9525;

  export interface EditItem {
    id: string;
    name: string;
    kind: string;
    /** Box in slide px (the unrotated box, as the file stores it). */
    x: number;
    y: number;
    w: number;
    h: number;
    rotation: number;
    /** Pictures keep their aspect ratio unless Shift frees it. */
    keepAspect: boolean;
    /** Tables move but don't resize (their columns don't follow the frame). */
    resizable: boolean;
  }

  /** The slide's own top-level shapes, back to front. */
  export function editItems(slide: SlideData | undefined): EditItem[] {
    if (slide === undefined) return [];
    return slide.nodes
      .filter((n) => n.id !== "" && n.size.w >= 0 && n.size.h >= 0)
      .map((n) => ({
        id: n.id,
        name: n.name.trim(),
        kind: n.nodeType,
        x: n.position.x,
        y: n.position.y,
        w: n.size.w,
        h: n.size.h,
        rotation: n.rotation ?? 0,
        keepAspect: n.nodeType === "picture",
        resizable: n.nodeType !== "table" && (n.rotation ?? 0) === 0,
      }));
  }
</script>

<script lang="ts">
  /**
   * The PowerPoint viewer's edit mode, laid over the stage's slide: select a
   * shape, drag it, resize it from its handles, nudge it with the arrows,
   * delete it, double-click (or Enter) to edit its text. Every gesture
   * becomes an operation on `DeckEdits` (one undo step per gesture); the
   * view redraws the slide from the patched XML, so what you see is the
   * renderer's real output, never an approximation.
   *
   * The layer takes the slide's pointer while editing: a link or a slide
   * jump in the deck doesn't fire under it. Geometry is written in whole
   * EMU from the file's own values, so a shape that isn't moved keeps its
   * numbers exactly.
   */
  import { untrack } from "svelte";
  import type { DeckEdits } from "./pptxEditStore.svelte";
  import { LINE_BREAK, PptxXmlError, readText, readXfrm, type Xfrm } from "./pptxXml";

  interface Props {
    edits: DeckEdits;
    slide: SlideData | undefined;
    /** The slide's part in the package (`ppt/slides/slide3.xml`). */
    part: string;
    slideW: number;
    slideH: number;
    /** Screen px per slide px. */
    scale: number;
    /** Bumped when the slide was redrawn: the shapes are re-read. */
    version: number;
    selected: string | null;
    /** Text editing took the keyboard (the view's keys stand down). */
    texting: boolean;
  }

  let {
    edits,
    slide,
    part,
    slideW,
    slideH,
    scale,
    version,
    selected = $bindable(null),
    texting = $bindable(false),
  }: Props = $props();

  const items = $derived.by(() => {
    void version;
    return editItems(slide);
  });
  const sel = $derived(items.find((it) => it.id === selected) ?? null);

  // A redraw that lost the selected shape (undo of a delete's redo, a
  // rebase) drops the selection.
  $effect(() => {
    if (selected !== null && sel === null) untrack(() => (selected = null));
  });

  let layerEl = $state<HTMLDivElement | null>(null);

  // --- gestures ------------------------------------------------------------------

  interface Gesture {
    kind: "move" | "resize";
    /** Handle for a resize: two letters of n/s/e/w ("" for none). */
    handle: string;
    id: string;
    pointer: number;
    start: { x: number; y: number };
    item: EditItem;
    /** The file's own transform, when it has one (EMU). */
    base: Xfrm | null;
    gesture: string;
    moved: boolean;
  }

  let g: Gesture | null = null;
  let seq = 0;
  /** The box shown while a gesture runs (slide px); null at rest. */
  let live = $state.raw<{ x: number; y: number; w: number; h: number } | null>(null);
  /** Snap guides while dragging (slide px). */
  let guides = $state.raw<{ v: number[]; h: number[] }>({ v: [], h: [] });
  let raf = 0;
  let queued: Xfrm | null = null;

  function toSlide(e: { clientX: number; clientY: number }): { x: number; y: number } | null {
    const r = layerEl?.getBoundingClientRect();
    if (r === undefined || !(scale > 0)) return null;
    return { x: (e.clientX - r.left) / scale, y: (e.clientY - r.top) / scale };
  }

  function hit(p: { x: number; y: number }): EditItem | null {
    for (let k = items.length - 1; k >= 0; k--) {
      const it = items[k]!;
      if (p.x >= it.x && p.x <= it.x + it.w && p.y >= it.y && p.y <= it.y + it.h) return it;
    }
    return null;
  }

  function baseXfrm(id: string): Xfrm | null {
    const xml = edits.partText(part);
    if (xml === null) return null;
    try {
      return readXfrm(xml, id);
    } catch {
      return null;
    }
  }

  function begin(e: PointerEvent, kind: "move" | "resize", item: EditItem, handle = ""): void {
    const at = toSlide(e);
    if (at === null) return;
    e.preventDefault();
    e.stopPropagation();
    layerEl?.setPointerCapture(e.pointerId);
    g = {
      kind,
      handle,
      id: item.id,
      pointer: e.pointerId,
      start: at,
      item,
      base: baseXfrm(item.id),
      gesture: `${kind}-${item.id}-${++seq}`,
      moved: false,
    };
  }

  function onPointerDown(e: PointerEvent): void {
    if (e.button !== 0 || texting) return;
    const at = toSlide(e);
    if (at === null) return;
    const it = hit(at);
    layerEl?.focus({ preventScroll: true });
    if (it === null) {
      selected = null;
      return;
    }
    selected = it.id;
    begin(e, "move", it);
  }

  function onHandleDown(e: PointerEvent, handle: string): void {
    if (e.button !== 0 || sel === null) return;
    begin(e, "resize", sel, handle);
  }

  /** Snap `box`'s edges and centre to the slide's and the other shapes'
   *  (within a few screen pixels); Alt turns it off. */
  function snap(box: { x: number; y: number; w: number; h: number }, id: string, moving: boolean, free: boolean) {
    const tol = 6 / scale;
    const vs = [0, slideW / 2, slideW];
    const hs = [0, slideH / 2, slideH];
    for (const it of items) {
      if (it.id === id) continue;
      vs.push(it.x, it.x + it.w / 2, it.x + it.w);
      hs.push(it.y, it.y + it.h / 2, it.y + it.h);
    }
    const out = { ...box };
    const shown = { v: [] as number[], h: [] as number[] };
    if (free) return { box: out, shown };
    const best = (edges: number[], targets: number[]) => {
      let d: number | null = null;
      let at = 0;
      for (const e of edges) {
        for (const t of targets) {
          const delta = t - e;
          if (Math.abs(delta) <= tol && (d === null || Math.abs(delta) < Math.abs(d))) {
            d = delta;
            at = t;
          }
        }
      }
      return d === null ? null : { d, at };
    };
    if (moving) {
      const sx = best([box.x, box.x + box.w / 2, box.x + box.w], vs);
      if (sx !== null) {
        out.x += sx.d;
        shown.v.push(sx.at);
      }
      const sy = best([box.y, box.y + box.h / 2, box.y + box.h], hs);
      if (sy !== null) {
        out.y += sy.d;
        shown.h.push(sy.at);
      }
    }
    return { box: out, shown };
  }

  function resized(it: EditItem, handle: string, dx: number, dy: number, keep: boolean) {
    let { x, y, w, h } = it;
    const min = 4 / scale;
    if (handle.includes("e")) w = Math.max(min, it.w + dx);
    if (handle.includes("w")) {
      w = Math.max(min, it.w - dx);
      x = it.x + it.w - w;
    }
    if (handle.includes("s")) h = Math.max(min, it.h + dy);
    if (handle.includes("n")) {
      h = Math.max(min, it.h - dy);
      y = it.y + it.h - h;
    }
    if (keep && it.w > 0 && it.h > 0) {
      const ratio = it.w / it.h;
      const corner = handle.length === 2;
      if (corner || handle === "e" || handle === "w") {
        const nh = w / ratio;
        if (handle.includes("n")) y = it.y + it.h - nh;
        else if (!handle.includes("s")) y = it.y + (it.h - nh) / 2;
        h = nh;
      } else {
        const nw = h * ratio;
        x = it.x + (it.w - nw) / 2;
        w = nw;
      }
    }
    return { x, y, w, h };
  }

  /** The EMU a box in px writes: the file's own number where that edge
   *  didn't move, so an untouched coordinate never drifts by rounding. */
  function toEmu(box: { x: number; y: number; w: number; h: number }, it: EditItem, base: Xfrm | null): Xfrm {
    const emu = (px: number) => Math.round(px * EMU_PER_PX);
    const same = (a: number, b: number) => Math.abs(a - b) < 1e-6;
    return {
      x: base !== null && same(box.x, it.x) ? base.x : emu(box.x),
      y: base !== null && same(box.y, it.y) ? base.y : emu(box.y),
      cx: base !== null && same(box.w, it.w) ? base.cx : emu(box.w),
      cy: base !== null && same(box.h, it.h) ? base.cy : emu(box.h),
    };
  }

  function flush(): void {
    raf = 0;
    const cur = g;
    const to = queued;
    queued = null;
    if (cur === null || to === null) return;
    edits.edit({ kind: "xfrm", part, shape: cur.id, from: cur.base, to }, cur.gesture);
  }

  function onPointerMove(e: PointerEvent): void {
    const cur = g;
    if (cur === null || e.pointerId !== cur.pointer) return;
    const at = toSlide(e);
    if (at === null) return;
    const dx = at.x - cur.start.x;
    const dy = at.y - cur.start.y;
    if (!cur.moved && Math.hypot(dx * scale, dy * scale) < 3) return;
    cur.moved = true;
    const it = cur.item;
    let box =
      cur.kind === "move"
        ? { x: it.x + dx, y: it.y + dy, w: it.w, h: it.h }
        : resized(it, cur.handle, dx, dy, it.keepAspect !== e.shiftKey);
    if (cur.kind === "move" && e.shiftKey) {
      // Shift keeps a move on one axis.
      if (Math.abs(dx) > Math.abs(dy)) box.y = it.y;
      else box.x = it.x;
    }
    const s = snap(box, it.id, cur.kind === "move", e.altKey);
    box = s.box;
    guides = s.shown;
    live = box;
    queued = toEmu(box, it, cur.base);
    if (raf === 0) raf = requestAnimationFrame(flush);
  }

  function end(e: PointerEvent): void {
    const cur = g;
    if (cur === null || e.pointerId !== cur.pointer) return;
    if (layerEl?.hasPointerCapture(e.pointerId)) layerEl.releasePointerCapture(e.pointerId);
    if (raf !== 0) {
      cancelAnimationFrame(raf);
      flush();
    }
    g = null;
    live = null;
    guides = { v: [], h: [] };
  }

  $effect(() => () => {
    if (raf !== 0) cancelAnimationFrame(raf);
  });

  // --- keys ---------------------------------------------------------------------

  let nudgeGesture: string | null = null;
  $effect(() => {
    void selected;
    nudgeGesture = null;
  });

  function nudge(dx: number, dy: number): void {
    const it = sel;
    if (it === null) return;
    const base = baseXfrm(it.id);
    nudgeGesture ??= `nudge-${it.id}-${++seq}`;
    const box = { x: it.x + dx, y: it.y + dy, w: it.w, h: it.h };
    edits.edit({ kind: "xfrm", part, shape: it.id, from: base, to: toEmu(box, it, base) }, nudgeGesture);
  }

  /** Keys the layer handles; true when it took the key. */
  export function key(e: KeyboardEvent): boolean {
    if (texting) return false;
    const step = e.shiftKey ? 10 : 1;
    switch (e.key) {
      case "ArrowLeft":
        if (sel === null) return false;
        nudge(-step, 0);
        return true;
      case "ArrowRight":
        if (sel === null) return false;
        nudge(step, 0);
        return true;
      case "ArrowUp":
        if (sel === null) return false;
        nudge(0, -step);
        return true;
      case "ArrowDown":
        if (sel === null) return false;
        nudge(0, step);
        return true;
      case "Delete":
      case "Backspace":
        if (sel === null) return false;
        if (edits.edit({ kind: "delete", part, shape: sel.id })) selected = null;
        return true;
      case "Enter":
        if (sel === null) return false;
        startText(sel);
        return true;
      case "Tab": {
        if (items.length === 0) return false;
        const at = items.findIndex((it) => it.id === selected);
        const n = items.length;
        const next = at < 0 ? (e.shiftKey ? n - 1 : 0) : (at + (e.shiftKey ? n - 1 : 1)) % n;
        selected = items[next]!.id;
        return true;
      }
      case "Escape":
        if (selected === null) return false;
        selected = null;
        return true;
      default:
        return false;
    }
  }

  // --- text -----------------------------------------------------------------------

  /** Line separator in the editor stands for a line break (`a:br`). */
  const LS = " ";
  let textFor = $state<string | null>(null);
  let draft = $state("");
  let textEl = $state<HTMLTextAreaElement | null>(null);
  let textFrom: string[] = [];
  let textGesture = "";
  let textTimer: ReturnType<typeof setTimeout> | null = null;
  let textError = $state<string | null>(null);

  function startText(it: EditItem): void {
    const xml = edits.partText(part);
    if (xml === null) return;
    let paras: string[] | null;
    try {
      paras = readText(xml, it.id);
    } catch (e) {
      if (!(e instanceof PptxXmlError)) throw e;
      paras = null;
    }
    if (paras === null) return;
    textFrom = paras;
    textGesture = `text-${it.id}-${++seq}`;
    draft = paras.map((p) => p.split(LINE_BREAK).join(LS)).join("\n");
    textFor = it.id;
    textError = null;
    texting = true;
    queueMicrotask(() => {
      textEl?.focus();
      textEl?.select();
    });
  }

  function writeText(): void {
    if (textTimer !== null) clearTimeout(textTimer);
    textTimer = null;
    const id = textFor;
    if (id === null) return;
    const to = draft.split("\n").map((p) => p.split(LS).join(LINE_BREAK));
    const ok = edits.edit({ kind: "text", part, shape: id, from: textFrom, to }, textGesture);
    textError = ok ? null : edits.error;
  }

  function onTextInput(): void {
    if (textTimer !== null) clearTimeout(textTimer);
    textTimer = setTimeout(writeText, 140);
  }

  function endText(): void {
    if (textFor === null) return;
    writeText();
    textFor = null;
    texting = false;
    layerEl?.focus({ preventScroll: true });
  }

  function onTextKey(e: KeyboardEvent): void {
    e.stopPropagation();
    if (e.key === "Escape") {
      e.preventDefault();
      endText();
    } else if (e.key === "Enter" && e.shiftKey) {
      e.preventDefault();
      const el = e.currentTarget as HTMLTextAreaElement;
      const { selectionStart: a, selectionEnd: b } = el;
      draft = draft.slice(0, a) + LS + draft.slice(b);
      queueMicrotask(() => el.setSelectionRange(a + 1, a + 1));
      onTextInput();
    }
  }

  $effect(() => () => {
    if (textTimer !== null) {
      clearTimeout(textTimer);
      writeText();
    }
  });

  const textItem = $derived(items.find((it) => it.id === textFor) ?? null);
  /** The editor sits under the shape, or above it when there's no room. */
  const textBox = $derived.by(() => {
    const it = textItem;
    if (it === null) return null;
    const W = slideW * scale;
    const H = slideH * scale;
    const width = Math.min(W - 16, Math.max(260, it.w * scale));
    const left = Math.min(Math.max(8, it.x * scale), W - width - 8);
    const below = (it.y + it.h) * scale + 8;
    const room = H - below - 8;
    const height = Math.min(160, Math.max(64, H * 0.3));
    const top = room >= Math.min(height, 80) ? below : Math.max(8, it.y * scale - height - 8);
    return { left, top, width, maxHeight: Math.max(48, Math.min(height, top === below ? room : it.y * scale - 16)) };
  });

  // --- drawing ------------------------------------------------------------------

  const shown = $derived(live ?? (sel === null ? null : { x: sel.x, y: sel.y, w: sel.w, h: sel.h }));
  const HANDLES = ["nw", "n", "ne", "e", "se", "s", "sw", "w"] as const;
  function handlePos(hd: string, b: { w: number; h: number }): { left: number; top: number } {
    const x = hd.includes("w") ? 0 : hd.includes("e") ? b.w : b.w / 2;
    const y = hd.includes("n") ? 0 : hd.includes("s") ? b.h : b.h / 2;
    return { left: x * scale, top: y * scale };
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
  class="edit-layer"
  bind:this={layerEl}
  tabindex="-1"
  role="application"
  aria-label="edit slide: drag shapes, double-click to edit text"
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={end}
  onpointercancel={end}
  ondblclick={(e) => {
    const at = toSlide(e);
    const it = at === null ? null : hit(at);
    if (it !== null) {
      selected = it.id;
      startText(it);
    }
  }}
>
  {#each items as it (it.id)}
    {#if it.id !== selected}
      <div
        class="hover-box"
        aria-hidden="true"
        style:left="{it.x * scale}px"
        style:top="{it.y * scale}px"
        style:width="{it.w * scale}px"
        style:height="{it.h * scale}px"
        style:transform={it.rotation !== 0 ? `rotate(${it.rotation}deg)` : undefined}
      ></div>
    {/if}
  {/each}
  {#each guides.v as gx, k (k)}
    <div class="guide v" aria-hidden="true" style:left="{gx * scale}px"></div>
  {/each}
  {#each guides.h as gy, k (k)}
    <div class="guide h" aria-hidden="true" style:top="{gy * scale}px"></div>
  {/each}
  {#if shown !== null && sel !== null}
    <div
      class="sel-box"
      class:moving={live !== null}
      style:left="{shown.x * scale}px"
      style:top="{shown.y * scale}px"
      style:width="{shown.w * scale}px"
      style:height="{shown.h * scale}px"
      style:transform={sel.rotation !== 0 ? `rotate(${sel.rotation}deg)` : undefined}
      title={sel.name}
    >
      {#if sel.resizable && textFor === null}
        {#each HANDLES as hd (hd)}
          {@const p = handlePos(hd, shown)}
          <span
            class="handle h-{hd}"
            style:left="{p.left}px"
            style:top="{p.top}px"
            onpointerdown={(e) => onHandleDown(e, hd)}
            aria-hidden="true"
          ></span>
        {/each}
      {/if}
    </div>
  {/if}
  {#if textBox !== null}
    <div
      class="text-editor"
      style:left="{textBox.left}px"
      style:top="{textBox.top}px"
      style:width="{textBox.width}px"
      onpointerdown={(e) => e.stopPropagation()}
      ondblclick={(e) => e.stopPropagation()}
      role="presentation"
    >
      <textarea
        bind:this={textEl}
        bind:value={draft}
        style:max-height="{textBox.maxHeight}px"
        oninput={onTextInput}
        onkeydown={onTextKey}
        onblur={endText}
        spellcheck="true"
        aria-label="text of {textItem?.name || 'the shape'}"
        title="Enter: new paragraph · Shift+Enter: line break · Esc: done"
      ></textarea>
      {#if textError !== null}<span class="text-err">{textError}</span>{/if}
    </div>
  {/if}
</div>

<style>
  .edit-layer {
    position: absolute;
    inset: 0;
    z-index: 1;
    outline: none;
    cursor: default;
    touch-action: none;
  }

  /* Every shape shows where it is while editing, quietly. */
  .hover-box {
    position: absolute;
    box-sizing: border-box;
    border: 1px dashed transparent;
    transform-origin: center;
    pointer-events: none;
  }

  .edit-layer:hover .hover-box {
    border-color: color-mix(in srgb, var(--accent) 35%, transparent);
  }

  .sel-box {
    position: absolute;
    box-sizing: border-box;
    border: 1.5px solid var(--accent);
    box-shadow: 0 0 0 1px color-mix(in srgb, #fff 70%, transparent);
    transform-origin: center;
    cursor: move;
    pointer-events: none;
  }

  .sel-box.moving {
    border-style: dashed;
  }

  .handle {
    position: absolute;
    width: 9px;
    height: 9px;
    margin: -5px 0 0 -5px;
    box-sizing: border-box;
    border: 1.5px solid var(--accent);
    border-radius: 2px;
    background: #fff;
    pointer-events: auto;
  }

  .h-nw,
  .h-se {
    cursor: nwse-resize;
  }
  .h-ne,
  .h-sw {
    cursor: nesw-resize;
  }
  .h-n,
  .h-s {
    cursor: ns-resize;
  }
  .h-e,
  .h-w {
    cursor: ew-resize;
  }

  .guide {
    position: absolute;
    pointer-events: none;
    background: color-mix(in srgb, var(--accent) 80%, #f0f);
  }
  .guide.v {
    top: 0;
    bottom: 0;
    width: 1px;
  }
  .guide.h {
    left: 0;
    right: 0;
    height: 1px;
  }

  .text-editor {
    position: absolute;
    z-index: 2;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.35rem;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--overlay-bg);
    box-shadow: 0 6px 24px color-mix(in srgb, #000 22%, transparent);
    color-scheme: inherit;
  }

  .text-editor textarea {
    box-sizing: border-box;
    width: 100%;
    min-height: 3.2em;
    resize: none;
    field-sizing: content;
    border: 0;
    outline: none;
    background: transparent;
    color: var(--fg);
    font: inherit;
    font-family: var(--sans, system-ui);
    font-size: var(--text-sm);
    line-height: 1.45;
    overflow: auto;
  }

  .text-err {
    color: var(--err);
    font-size: var(--text-xs);
  }
</style>
