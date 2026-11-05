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
    /** A shape with a text body: double-click (or a click once selected,
     *  or just typing) edits its text. */
    text: boolean;
  }

  /**
   * The slide's own top-level shapes, back to front. A shape whose id
   * repeats on the slide is left out: an edit addresses a shape by its id,
   * and agent-written decks (python-pptx deep copies) can repeat one, so
   * editing either would change both.
   */
  export function editItems(slide: SlideData | undefined): EditItem[] {
    if (slide === undefined) return [];
    const seen = new Map<string, number>();
    for (const n of slide.nodes) seen.set(n.id, (seen.get(n.id) ?? 0) + 1);
    return slide.nodes
      .filter((n) => n.id !== "" && seen.get(n.id) === 1 && n.size.w >= 0 && n.size.h >= 0)
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
        text: n.nodeType === "shape" && (n as { textBody?: unknown }).textBody !== undefined,
      }));
  }
</script>

<script lang="ts">
  /**
   * The PowerPoint viewer's edit mode, laid over the stage's slide. Select a
   * shape (click), drag it, resize it from its handles, nudge it with the
   * arrows, delete it; edit its text where it is drawn (double-click, a click
   * on the selected shape, Enter, or just start typing).
   *
   * A move or resize previews by transforming the drawn shape and commits one
   * operation on release; text goes to the history as you type. Either way
   * the view holds the slide's redraw (`holding`) while the gesture or the
   * typing runs, and redraws it from the patched XML when it ends, so what
   * stays on screen is the renderer's real output. Every operation carries
   * the slide part it started on: paging mid-gesture can't move it onto
   * another slide's shape with the same id.
   *
   * Geometry is written in whole EMU from the file's own values, so an edge
   * that didn't move keeps its number exactly.
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
    /** Text editing has the keyboard (the view's keys stand down). */
    texting: boolean;
    /** The slide must not be redrawn now: text is being typed into it, or a
     *  shape is mid-drag (both live in the drawn slide's own DOM). */
    holding: boolean;
    /** A short note for the bar ("edit tables in PowerPoint"). */
    notice: string | null;
    /** The stage's rendered element for this slide. */
    slideEl: () => HTMLElement | null;
    /** ⌘S while typing: commit the text, then save. */
    onsave: () => void;
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
    holding = $bindable(false),
    notice = $bindable(null),
    slideEl,
    onsave,
  }: Props = $props();

  const items = $derived.by(() => {
    void version;
    return editItems(slide);
  });
  const sel = $derived(items.find((it) => it.id === selected) ?? null);

  // A redraw that lost the selected shape (an undone add, a rebase) drops
  // the selection.
  $effect(() => {
    if (selected !== null && sel === null) untrack(() => (selected = null));
  });

  /** Text shapes with no text: drawn dashed so an empty placeholder can be
   *  found (the renderer draws nothing for it). */
  const empty = $derived.by(() => {
    void edits.version;
    const xml = edits.partText(part);
    const out = new Set<string>();
    if (xml === null) return out;
    for (const it of items) {
      if (!it.text) continue;
      try {
        const paras = readText(xml, it.id);
        if (paras !== null && paras.every((p) => p.trim() === "")) out.add(it.id);
      } catch {
        // Not readable: not offered as empty.
      }
    }
    return out;
  });

  let layerEl = $state<HTMLDivElement | null>(null);
  /** Said to a screen reader as the selection changes. */
  let announce = $state("");
  $effect(() => {
    const it = sel;
    untrack(() => (announce = it === null ? "" : `${it.name || it.kind} selected`));
  });

  let noticeTimer: ReturnType<typeof setTimeout> | null = null;
  function say(text: string): void {
    notice = text;
    if (noticeTimer !== null) clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => {
      noticeTimer = null;
      if (notice === text) notice = null;
    }, 3500);
  }

  // --- geometry -----------------------------------------------------------------------

  function toSlide(e: { clientX: number; clientY: number }): { x: number; y: number } | null {
    const r = layerEl?.getBoundingClientRect();
    if (r === undefined || !(scale > 0)) return null;
    return { x: (e.clientX - r.left) / scale, y: (e.clientY - r.top) / scale };
  }

  /** The topmost shape under `p`: tested in the shape's own (rotated) frame,
   *  with a few screen pixels of slack so a line or a thin shape can be hit. */
  function hit(p: { x: number; y: number }): EditItem | null {
    const pad = 4 / scale;
    for (let k = items.length - 1; k >= 0; k--) {
      const it = items[k]!;
      let { x, y } = p;
      if (it.rotation !== 0) {
        const cx = it.x + it.w / 2;
        const cy = it.y + it.h / 2;
        const a = (-it.rotation * Math.PI) / 180;
        const dx = x - cx;
        const dy = y - cy;
        x = cx + dx * Math.cos(a) - dy * Math.sin(a);
        y = cy + dx * Math.sin(a) + dy * Math.cos(a);
      }
      if (x >= it.x - pad && x <= it.x + it.w + pad && y >= it.y - pad && y <= it.y + it.h + pad) return it;
    }
    return null;
  }

  function baseXfrm(onPart: string, id: string): Xfrm | null {
    const xml = edits.partText(onPart);
    if (xml === null) return null;
    try {
      return readXfrm(xml, id);
    } catch {
      return null;
    }
  }

  /** The renderer's element for shape `it`: the positioned div at its box.
   *  The renderer tags no ids, so it's found by geometry; the later of two
   *  shapes sharing a box wins (it is drawn on top). */
  function drawnShape(it: EditItem): HTMLElement | null {
    const root = slideEl();
    if (root === null) return null;
    const near = (a: string, b: number) => Math.abs(parseFloat(a) - b) < 0.75;
    let found: HTMLElement | null = null;
    for (const el of Array.from(root.querySelectorAll<HTMLElement>("div"))) {
      const st = el.style;
      if (st.position !== "absolute") continue;
      if (near(st.left, it.x) && near(st.top, it.y) && near(st.width, it.w) && near(st.height, it.h)) found = el;
    }
    return found;
  }

  // --- gestures ------------------------------------------------------------------

  type Box = { x: number; y: number; w: number; h: number };

  interface Gesture {
    kind: "move" | "resize";
    /** Handle for a resize: one or two of n/s/e/w. */
    handle: string;
    item: EditItem;
    /** The part the gesture started on (the one it writes). */
    part: string;
    pointer: number;
    start: { x: number; y: number };
    /** The file's own transform, when it has one (EMU). */
    base: Xfrm | null;
    /** The drawn shape, transformed as a preview. */
    drawn: HTMLElement | null;
    /** The shape was selected before this press (a still click edits text). */
    wasSelected: boolean;
    moved: boolean;
    box: Box;
  }

  let g: Gesture | null = null;
  /** The box shown while a gesture runs (slide px); null at rest. */
  let live = $state.raw<Box | null>(null);
  /** Snap guides while dragging (slide px). */
  let guides = $state.raw<{ v: number[]; h: number[] }>({ v: [], h: [] });

  function begin(e: PointerEvent, kind: "move" | "resize", item: EditItem, handle = "", wasSelected = false): void {
    const at = toSlide(e);
    if (at === null) return;
    e.preventDefault();
    e.stopPropagation();
    layerEl?.setPointerCapture(e.pointerId);
    g = {
      kind,
      handle,
      item,
      part,
      pointer: e.pointerId,
      start: at,
      base: baseXfrm(part, item.id),
      drawn: drawnShape(item),
      wasSelected,
      moved: false,
      box: { x: item.x, y: item.y, w: item.w, h: item.h },
    };
    holding = true;
  }

  function onPointerDown(e: PointerEvent): void {
    if (e.button !== 0) return;
    // A press while the box editor is up finishes it, then acts as a press.
    if (texting) {
      if (!boxed) return;
      endText();
    }
    const at = toSlide(e);
    if (at === null) return;
    const it = hit(at);
    layerEl?.focus({ preventScroll: true });
    if (it === null) {
      selected = null;
      return;
    }
    const was = selected === it.id;
    selected = it.id;
    begin(e, "move", it, "", was);
  }

  function onHandleDown(e: PointerEvent, handle: string): void {
    if (e.button !== 0 || sel === null) return;
    begin(e, "resize", sel, handle, true);
  }

  /** Snap a moving box's edges and centre to the slide's and the other
   *  shapes' (within a few screen pixels), or a resized box's moving edges;
   *  Alt turns it off. */
  function snap(box: Box, id: string, handle: string, free: boolean) {
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
    if (handle === "") {
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
      return { box: out, shown };
    }
    // A resize snaps only the edges its handle moves.
    if (handle.includes("e")) {
      const s = best([box.x + box.w], vs);
      if (s !== null) {
        out.w += s.d;
        shown.v.push(s.at);
      }
    } else if (handle.includes("w")) {
      const s = best([box.x], vs);
      if (s !== null && box.w - s.d > 0) {
        out.x += s.d;
        out.w -= s.d;
        shown.v.push(s.at);
      }
    }
    if (handle.includes("s")) {
      const s = best([box.y + box.h], hs);
      if (s !== null) {
        out.h += s.d;
        shown.h.push(s.at);
      }
    } else if (handle.includes("n")) {
      const s = best([box.y], hs);
      if (s !== null && box.h - s.d > 0) {
        out.y += s.d;
        out.h -= s.d;
        shown.h.push(s.at);
      }
    }
    return { box: out, shown };
  }

  function resized(it: EditItem, handle: string, dx: number, dy: number, keep: boolean): Box {
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
      if (handle.length === 2) {
        // A corner follows whichever of the two moved further, relative to size.
        if (Math.abs(w / it.w - 1) >= Math.abs(h / it.h - 1)) h = w / ratio;
        else w = h * ratio;
        if (handle.includes("w")) x = it.x + it.w - w;
        if (handle.includes("n")) y = it.y + it.h - h;
      } else if (handle === "e" || handle === "w") {
        const nh = w / ratio;
        y = it.y + (it.h - nh) / 2;
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
  function toEmu(box: Box, it: EditItem, base: Xfrm | null): Xfrm {
    const emu = (px: number) => Math.round(px * EMU_PER_PX);
    const same = (a: number, b: number) => Math.abs(a - b) < 1e-6;
    return {
      x: base !== null && same(box.x, it.x) ? base.x : emu(box.x),
      y: base !== null && same(box.y, it.y) ? base.y : emu(box.y),
      cx: base !== null && same(box.w, it.w) ? base.cx : emu(box.w),
      cy: base !== null && same(box.h, it.h) ? base.cy : emu(box.h),
    };
  }

  /** Preview `box` by transforming the drawn shape (inside the scaled slot,
   *  so slide px): no re-render per frame, no text re-layout. */
  function preview(cur: Gesture, box: Box): void {
    const el = cur.drawn;
    if (el === null) return;
    const it = cur.item;
    const sx = it.w > 0 ? box.w / it.w : 1;
    const sy = it.h > 0 ? box.h / it.h : 1;
    el.style.transformOrigin = "0 0";
    el.style.translate = `${box.x - it.x}px ${box.y - it.y}px`;
    el.style.scale = cur.kind === "resize" ? `${sx} ${sy}` : "";
  }

  function clearPreview(cur: Gesture): void {
    const el = cur.drawn;
    if (el === null) return;
    el.style.translate = "";
    el.style.scale = "";
  }

  function onPointerMove(e: PointerEvent): void {
    onHover(e);
    const cur = g;
    if (cur === null || e.pointerId !== cur.pointer) return;
    // The release was lost (a window switch mid-drag): end it here.
    if (e.buttons === 0) {
      finish(cur, true);
      return;
    }
    const at = toSlide(e);
    if (at === null) return;
    const dx = at.x - cur.start.x;
    const dy = at.y - cur.start.y;
    if (!cur.moved && Math.hypot(dx * scale, dy * scale) < 3) return;
    cur.moved = true;
    const it = cur.item;
    let box: Box =
      cur.kind === "move"
        ? { x: it.x + dx, y: it.y + dy, w: it.w, h: it.h }
        : resized(it, cur.handle, dx, dy, it.keepAspect !== e.shiftKey);
    if (cur.kind === "move" && e.shiftKey) {
      // Shift keeps a move on one axis.
      if (Math.abs(dx) > Math.abs(dy)) box.y = it.y;
      else box.x = it.x;
    }
    const s = snap(box, it.id, cur.kind === "move" ? "" : cur.handle, e.altKey);
    box = s.box;
    guides = s.shown;
    cur.box = box;
    live = box;
    preview(cur, box);
  }

  /** End the gesture: commit one operation (or none, for a still click or
   *  a cancel) and let the view redraw. */
  function finish(cur: Gesture, commit: boolean): void {
    if (layerEl?.hasPointerCapture(cur.pointer)) layerEl.releasePointerCapture(cur.pointer);
    g = null;
    live = null;
    guides = { v: [], h: [] };
    if (commit && cur.moved) {
      const to = toEmu(cur.box, cur.item, cur.base);
      const ok = edits.edit({ kind: "xfrm", part: cur.part, shape: cur.item.id, from: cur.base, to });
      if (!ok) clearPreview(cur);
    } else clearPreview(cur);
    holding = textFor !== null && !boxed;
    // A still click on the already-selected text shape puts the caret there.
    if (commit && !cur.moved && cur.kind === "move" && cur.wasSelected && cur.item.text) {
      startText(cur.item, { x: cur.start.x * scale, y: cur.start.y * scale, client: true });
    }
  }

  function onPointerUp(e: PointerEvent): void {
    const cur = g;
    if (cur === null || e.pointerId !== cur.pointer) return;
    finish(cur, true);
  }

  function onPointerCancel(e: PointerEvent): void {
    const cur = g;
    if (cur === null || e.pointerId !== cur.pointer) return;
    finish(cur, false);
  }

  // --- keys ---------------------------------------------------------------------

  /** Consecutive nudges within a second are one undo step. */
  let nudgeGesture: string | null = null;
  let nudgeAt = 0;
  let seq = 0;
  $effect(() => {
    void selected;
    nudgeGesture = null;
  });

  function nudge(dx: number, dy: number): void {
    const it = sel;
    if (it === null) return;
    const base = baseXfrm(part, it.id);
    const now = Date.now();
    if (nudgeGesture === null || now - nudgeAt > 1000) nudgeGesture = `nudge-${it.id}-${++seq}`;
    nudgeAt = now;
    const box = { x: it.x + dx, y: it.y + dy, w: it.w, h: it.h };
    edits.edit({ kind: "xfrm", part, shape: it.id, from: base, to: toEmu(box, it, base) }, nudgeGesture);
  }

  /** Keys the layer handles: true when it took the key, "typing" when the
   *  key starts text editing and must reach the text as typed (no
   *  `preventDefault`, and the deck mustn't act on it either). */
  export function key(e: KeyboardEvent): boolean | "typing" {
    if (texting) return false;
    if (e.key === "Escape" && g !== null) {
      // Esc mid-drag puts the shape back.
      finish(g, false);
      return true;
    }
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
        if (sel.text) startText(sel);
        else say(cannotEdit(sel));
        return true;
      case "Tab": {
        // Through the shapes, then out of the slide (focus isn't trapped).
        const n = items.length;
        const at = items.findIndex((it) => it.id === selected);
        const next = at < 0 ? (e.shiftKey ? n - 1 : 0) : at + (e.shiftKey ? -1 : 1);
        if (n === 0 || next < 0 || next >= n) {
          selected = null;
          return false;
        }
        selected = items[next]!.id;
        return true;
      }
      case "Escape":
        if (selected === null) return false;
        selected = null;
        return true;
      default:
        // Typing on a selected text shape replaces its text, as in PowerPoint:
        // the key lands in the body that takes focus here.
        if (sel !== null && sel.text && e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey) {
          startText(sel, null, true);
          return textFor !== null ? "typing" : false;
        }
        return false;
    }
  }

  function cannotEdit(it: EditItem): string {
    if (it.kind === "table") return "edit a table's cells in PowerPoint";
    if (it.kind === "chart") return "edit a chart in PowerPoint";
    if (it.kind === "group") return "edit inside a group in PowerPoint";
    return "this shape has no text to edit here";
  }

  // --- text -----------------------------------------------------------------------
  //
  // Text is edited where it is drawn: the renderer lays a text body out as a
  // column of paragraph <div>s holding run <span>s (bullets are absolutely
  // positioned marker spans), so the shape's own body becomes editable and
  // you type into the slide in its real fonts. Edits go to the history as you
  // type; the slide's redraw is held until the typing ends (autofit and all
  // then). A body whose drawn text doesn't read back as the file's falls back
  // to a box editor under the shape, which redraws the slide as you type.

  /** U+2028 in the fallback editor stands for a line break (`a:br`). */
  const LS = " ";
  const NBSP = / /g;
  let textFor = $state<string | null>(null);
  /** The part the text session writes (the one it started on). */
  let textPart = "";
  /** The fallback box editor is up (the in-place one couldn't be used). */
  let boxed = $state(false);
  let draft = $state("");
  let textEl = $state<HTMLTextAreaElement | null>(null);
  let textFrom: string[] = [];
  let textGesture = "";
  /** This session already wrote an operation (later writes fold into it). */
  let textWrote = false;
  let textTimer: ReturnType<typeof setTimeout> | null = null;
  let textError = $state<string | null>(null);
  /** The renderer's text body being edited in place, and its teardown. */
  let body: HTMLElement | null = null;
  let undoBody: (() => void) | null = null;

  function isMarker(el: Element): boolean {
    return el instanceof HTMLElement && el.style.position === "absolute";
  }

  /** One drawn paragraph's text: runs' text, `<br>` as a line break, bullet
   *  markers skipped; the placeholder `<br>` an empty block carries is not a
   *  break (`readBody` drops one trailing break). */
  function readBlock(el: Node): string {
    let s = "";
    for (const n of Array.from(el.childNodes)) {
      if (n.nodeType === Node.TEXT_NODE) s += n.textContent ?? "";
      else if (n instanceof Element) {
        if (isMarker(n)) continue;
        if (n.tagName === "BR") s += LINE_BREAK;
        else s += readBlock(n);
      }
    }
    return s;
  }

  /** The body's paragraphs as the editor holds them now. Text a browser puts
   *  straight into an emptied body (no block) is a paragraph too. NBSPs read
   *  as spaces (a browser writes them for typed spaces at a run's edge). */
  function readBody(el: HTMLElement): string[] {
    const out: string[] = [];
    let loose: string | null = null;
    for (const n of Array.from(el.childNodes)) {
      if (n instanceof HTMLElement && (n.tagName === "DIV" || n.tagName === "P")) {
        if (loose !== null) out.push(loose);
        loose = null;
        out.push(readBlock(n));
      } else if (n.nodeType === Node.TEXT_NODE) {
        loose = (loose ?? "") + (n.textContent ?? "");
      } else if (n instanceof Element && !isMarker(n)) {
        loose = (loose ?? "") + (n.tagName === "BR" ? LINE_BREAK : readBlock(n));
      }
    }
    if (loose !== null) out.push(loose);
    return (out.length === 0 ? [""] : out).map((p) => {
      const t = p.replace(NBSP, " ");
      return t.endsWith(LINE_BREAK) ? t.slice(0, -1) : t;
    });
  }

  function textBodyOf(shape: HTMLElement): HTMLElement | null {
    for (const el of Array.from(shape.querySelectorAll<HTMLElement>("div"))) {
      if (el.style.display === "flex" && el.style.flexDirection === "column") return el;
    }
    return null;
  }

  /**
   * Edit `it`'s text. `at` places the caret (client coordinates, or slide
   * coordinates in screen px with `client: true` relative to the layer);
   * `replace` selects all of it so the next key replaces it.
   */
  function startText(
    it: EditItem,
    at: { x: number; y: number; client?: boolean } | null = null,
    replace = false,
  ): void {
    if (textFor !== null) endText();
    const xml = edits.partText(part);
    if (xml === null) return;
    let paras: string[] | null;
    try {
      paras = readText(xml, it.id);
    } catch (e) {
      if (!(e instanceof PptxXmlError)) throw e;
      paras = null;
    }
    if (paras === null) {
      say(cannotEdit(it));
      return;
    }
    textFrom = paras;
    textPart = part;
    textGesture = `text-${it.id}-${++seq}`;
    textWrote = false;
    textFor = it.id;
    textError = null;
    texting = true;
    const shape = drawnShape(it);
    const el = shape === null ? null : textBodyOf(shape);
    const norm = (p: string) => p.replace(NBSP, " ");
    const same = (a: readonly string[], b: readonly string[]) =>
      a.length === b.length && a.every((v, k) => norm(v) === norm(b[k]!));
    if (el !== null && same(readBody(el), paras)) {
      holding = true;
      let point: { x: number; y: number } | null = null;
      if (at !== null) {
        const r = layerEl?.getBoundingClientRect();
        point = at.client === true && r !== undefined ? { x: r.left + at.x, y: r.top + at.y } : at;
      }
      beginInPlace(el, replace ? null : point);
    } else {
      boxed = true;
      draft = paras.map((p) => p.split(LINE_BREAK).join(LS)).join("\n");
      queueMicrotask(() => {
        textEl?.focus();
        textEl?.select();
      });
    }
  }

  /** Formatting the file model can't take yet; everything else types. */
  const BLOCKED_INPUT = /^format|^insertFromDrop$|^insertLink$/;

  function beginInPlace(el: HTMLElement, at: { x: number; y: number } | null): void {
    body = el;
    const original = new Set(Array.from(el.children));
    const markers = Array.from(el.querySelectorAll<HTMLElement>("span")).filter(isMarker);
    for (const m of markers) m.contentEditable = "false";
    el.contentEditable = "true";
    el.spellcheck = true;
    el.classList.add("pptx-editing");
    const onBefore = (e: InputEvent) => {
      if (BLOCKED_INPUT.test(e.inputType)) e.preventDefault();
    };
    const onPaste = (e: ClipboardEvent) => {
      e.preventDefault();
      const text = e.clipboardData?.getData("text/plain") ?? "";
      // A pasted line break is a new paragraph, as typing Enter would be.
      document.execCommand("insertText", false, text.replace(/\r\n?/g, "\n"));
    };
    const onInput = () => {
      bulletNewBlocks(el, original);
      if (textTimer !== null) clearTimeout(textTimer);
      textTimer = setTimeout(writeText, 160);
    };
    const onKey = (e: KeyboardEvent) => {
      e.stopPropagation();
      if (e.isComposing) return;
      if (e.key === "Escape") {
        e.preventDefault();
        endText();
      } else if (e.key === "Tab") {
        // Tab would carry focus out of the text (and the text session with it).
        e.preventDefault();
      } else if ((e.metaKey || e.ctrlKey) && /^[biu]$/i.test(e.key)) e.preventDefault();
      else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        endText();
        onsave();
      }
    };
    // Focus leaving the text (a click elsewhere, the app's own chords)
    // finishes it: the session must never outlive its caret.
    const onBlur = (e: FocusEvent) => {
      const to = e.relatedTarget as Node | null;
      if (to !== null && el.contains(to)) return;
      queueMicrotask(() => {
        if (body === el) endText();
      });
    };
    el.addEventListener("beforeinput", onBefore);
    el.addEventListener("paste", onPaste);
    el.addEventListener("input", onInput);
    el.addEventListener("keydown", onKey);
    el.addEventListener("focusout", onBlur);
    undoBody = () => {
      el.removeEventListener("beforeinput", onBefore);
      el.removeEventListener("paste", onPaste);
      el.removeEventListener("input", onInput);
      el.removeEventListener("keydown", onKey);
      el.removeEventListener("focusout", onBlur);
      el.removeAttribute("contenteditable");
      el.classList.remove("pptx-editing");
      for (const m of markers) m.removeAttribute("contenteditable");
    };
    el.focus({ preventScroll: true });
    const sel = window.getSelection();
    // The layer still covers the slide until its `texting` class lands; the
    // caret's hit test must see the text under it.
    if (layerEl !== null) layerEl.style.pointerEvents = "none";
    const range = at === null ? null : caretAt(at.x, at.y);
    if (layerEl !== null) layerEl.style.pointerEvents = "";
    if (sel !== null) {
      sel.removeAllRanges();
      if (range !== null && el.contains(range.startContainer)) sel.addRange(range);
      else {
        const all = document.createRange();
        all.selectNodeContents(el);
        sel.addRange(all);
      }
    }
  }

  /** A paragraph Enter just made is a copy of its block without the marker
   *  (the marker isn't editable, so the browser doesn't carry it): give it
   *  the previous paragraph's, so a bullet shows while typing, as it will
   *  once the slide is redrawn from the file. */
  function bulletNewBlocks(el: HTMLElement, original: ReadonlySet<Element>): void {
    let prevMarker: HTMLElement | null = null;
    for (const block of Array.from(el.children)) {
      if (!(block instanceof HTMLElement)) continue;
      const own = Array.from(block.children).find(isMarker) as HTMLElement | undefined;
      if (own !== undefined) prevMarker = own;
      else if (original.has(block)) prevMarker = null;
      else if (prevMarker !== null && block.dataset.pptxNew === undefined) {
        block.dataset.pptxNew = "";
        const copy = prevMarker.cloneNode(true) as HTMLElement;
        copy.contentEditable = "false";
        block.prepend(copy);
      }
    }
  }

  function caretAt(x: number, y: number): Range | null {
    const d = document as Document & {
      caretRangeFromPoint?: (x: number, y: number) => Range | null;
      caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
    };
    if (d.caretPositionFromPoint !== undefined) {
      const p = d.caretPositionFromPoint(x, y);
      if (p === null) return null;
      const r = document.createRange();
      r.setStart(p.offsetNode, p.offset);
      return r;
    }
    return d.caretRangeFromPoint?.(x, y) ?? null;
  }

  function writeText(): void {
    if (textTimer !== null) clearTimeout(textTimer);
    textTimer = null;
    const id = textFor;
    if (id === null) return;
    const read = body !== null ? readBody(body) : draft.split("\n").map((p) => p.split(LS).join(LINE_BREAK));
    // A paragraph the editor didn't change keeps the file's exact text (its
    // NBSPs included): reading the DOM back is not an edit.
    const to = read.map((p, k) => {
      const was = textFrom[k];
      return was !== undefined && was.replace(NBSP, " ") === p ? was : p;
    });
    const unchanged = to.length === textFrom.length && to.every((p, k) => p === textFrom[k]);
    if (unchanged && !textWrote) return;
    const ok = edits.edit({ kind: "text", part: textPart, shape: id, from: textFrom, to }, textGesture);
    if (ok) textWrote = true;
    textError = ok ? null : edits.error;
  }

  function onTextInput(): void {
    if (textTimer !== null) clearTimeout(textTimer);
    textTimer = setTimeout(writeText, 140);
  }

  function endText(): void {
    if (textFor === null) return;
    writeText();
    undoBody?.();
    undoBody = null;
    body = null;
    boxed = false;
    textFor = null;
    window.getSelection()?.removeAllRanges();
    texting = false;
    // Releasing the hold lets the view redraw the slide from the file.
    holding = g !== null;
    layerEl?.focus({ preventScroll: true });
  }

  // Paging away (or anything that swaps the slide) ends the session.
  $effect(() => {
    if (textFor !== null && part !== untrack(() => textPart)) untrack(endText);
  });

  // A press outside the text being edited finishes it; on another shape it
  // also selects (and can start dragging) that shape, as in PowerPoint.
  $effect(() => {
    if (textFor === null || boxed) return;
    const onDown = (e: PointerEvent) => {
      const t = e.target as Node | null;
      if (body !== null && t !== null && body.contains(t)) return;
      endText();
      const r = layerEl?.getBoundingClientRect();
      if (r !== undefined && e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom) {
        e.preventDefault();
        e.stopPropagation();
        onPointerDown(e);
      }
    };
    window.addEventListener("pointerdown", onDown, true);
    return () => window.removeEventListener("pointerdown", onDown, true);
  });

  function onTextKey(e: KeyboardEvent): void {
    e.stopPropagation();
    if (e.isComposing) return;
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
    } else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
      e.preventDefault();
      endText();
      onsave();
    }
  }

  $effect(() => () => {
    if (textFor !== null) endText();
    if (g !== null) finish(g, false);
    if (noticeTimer !== null) clearTimeout(noticeTimer);
  });

  const textItem = $derived(items.find((it) => it.id === textFor) ?? null);
  /** The fallback editor sits under the shape, or above it when there's no room. */
  const textBox = $derived.by(() => {
    const it = textItem;
    if (it === null || !boxed) return null;
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

  // --- hover ----------------------------------------------------------------------

  /** The shape under the pointer (outlined; the cursor says what a press does). */
  let hoverId = $state<string | null>(null);
  function onHover(e: PointerEvent): void {
    if (g !== null || texting) return;
    const at = toSlide(e);
    hoverId = at === null ? null : (hit(at)?.id ?? null);
  }
  /** Over the selected text shape a press edits its text: an I-beam. */
  const overText = $derived(hoverId !== null && hoverId === selected && sel?.text === true);

  // --- drawing ------------------------------------------------------------------

  const shown = $derived(live ?? (sel === null ? null : { x: sel.x, y: sel.y, w: sel.w, h: sel.h }));
  /** Where the shape is (or is going), in points, while a gesture runs. */
  const readout = $derived.by(() => {
    const b = live;
    if (b === null) return null;
    const pt = (px: number) => Math.round(px * 0.75);
    return g?.kind === "resize" ? `${pt(b.w)} × ${pt(b.h)} pt` : `${pt(b.x)}, ${pt(b.y)} pt`;
  });
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
  aria-label="slide editor: click a shape to select it, drag to move, double-click to edit its text"
  class:texting={texting && !boxed}
  class:over-shape={hoverId !== null && !overText}
  class:over-text={overText}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerCancel}
  onpointerleave={() => (hoverId = null)}
  ondblclick={(e) => {
    const at = toSlide(e);
    const it = at === null ? null : hit(at);
    if (it === null) return;
    selected = it.id;
    if (it.text) startText(it, { x: e.clientX, y: e.clientY });
    else say(cannotEdit(it));
  }}
>
  {#each items as it, k (k)}
    {#if empty.has(it.id) && it.id !== selected}
      <div
        class="empty-box"
        aria-hidden="true"
        style:left="{it.x * scale}px"
        style:top="{it.y * scale}px"
        style:width="{it.w * scale}px"
        style:height="{it.h * scale}px"
      ></div>
    {/if}
    {#if it.id !== selected && it.id === hoverId}
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
      class:editing={textFor !== null && !boxed}
      style:left="{shown.x * scale}px"
      style:top="{shown.y * scale}px"
      style:width="{shown.w * scale}px"
      style:height="{shown.h * scale}px"
      style:transform={sel.rotation !== 0 ? `rotate(${sel.rotation}deg)` : undefined}
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
    {#if readout !== null}
      <span class="readout" aria-hidden="true" style:left="{shown.x * scale}px" style:top="{(shown.y + shown.h) * scale + 6}px"
        >{readout}</span
      >
    {/if}
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
  <span class="sr-only" aria-live="polite">{announce}</span>
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

  .edit-layer.over-shape {
    cursor: move;
  }

  .edit-layer.over-text {
    cursor: text;
  }

  /* Editing text in place: the slide's own text takes the pointer. */
  .edit-layer.texting {
    pointer-events: none;
  }

  /* The shape under the pointer, quietly. */
  .hover-box {
    position: absolute;
    box-sizing: border-box;
    border: 1px solid color-mix(in srgb, var(--accent) 55%, transparent);
    transform-origin: center;
    pointer-events: none;
  }

  /* An empty text placeholder: there to find, not to read. */
  .empty-box {
    position: absolute;
    box-sizing: border-box;
    border: 1px dashed color-mix(in srgb, var(--muted) 60%, transparent);
    border-radius: 2px;
    pointer-events: none;
  }

  .sel-box {
    position: absolute;
    box-sizing: border-box;
    border: 1.5px solid var(--accent);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--overlay-bg) 70%, transparent);
    transform-origin: center;
    pointer-events: none;
  }

  .sel-box.moving {
    border-style: dashed;
  }

  .sel-box.editing {
    border-style: dashed;
    border-width: 1px;
    box-shadow: none;
  }

  .handle {
    position: absolute;
    width: 9px;
    height: 9px;
    margin: -5px 0 0 -5px;
    box-sizing: border-box;
    border: 1.5px solid var(--accent);
    border-radius: 2px;
    background: var(--overlay-bg);
    pointer-events: auto;
  }

  /* A larger target than the drawn square: easy to catch on a trackpad. */
  .handle::before {
    content: "";
    position: absolute;
    inset: -5px;
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
    background: var(--accent);
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

  .readout {
    position: absolute;
    z-index: 2;
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--overlay-bg);
    color: var(--fg);
    font-family: var(--mono);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    pointer-events: none;
    box-shadow: 0 1px 4px color-mix(in srgb, var(--fg) 18%, transparent);
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  /* The renderer's text body while it is being typed into. */
  :global(.pptx-editing) {
    outline: none;
    cursor: text;
    user-select: text;
    -webkit-user-select: text;
    caret-color: var(--accent);
  }

  :global(.pptx-editing ::selection) {
    background: color-mix(in srgb, var(--accent) 30%, transparent);
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
    box-shadow: 0 6px 24px color-mix(in srgb, var(--fg) 22%, transparent);
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
