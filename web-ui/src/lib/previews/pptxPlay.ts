/**
 * A slide's build state put on its drawn shapes: what is on screen after
 * click k (`pptxTiming.ts`'s `BuildState`). Hidden is inline `opacity: 0`
 * plus `inert` (never `visibility`/`display`: they inherit and re-resolve
 * the subtree, and `display` would reflow the text around a hidden
 * paragraph, which PowerPoint keeps in place). A path's offset is the
 * individual `translate` property; what emphasis holds (R24–R31) is the
 * individual `scale` and `rotate` properties (about the element's centre,
 * on top of the renderer's own `transform`: its rotation and flips),
 * `opacity` times the drawn one, and colours painted over the renderer's
 * own: `fill` on the shape's SVG paths (a `to` colour replaces any fill,
 * gradient and picture included; a `by` change recolours a solid fill and
 * each stop of a gradient, and only a picture gets a CSS filter, noted as
 * approximate), `stroke` on its outline, `color` on its text's spans.
 *
 * Keys combine as the timing contract's R5 says: a shape is drawn unless
 * its key is hidden; a paragraph unless its range or its shape is hidden.
 * The renderer draws no separate element for a text box's background
 * (`:bg`), so a hidden `:bg` hides the shape only when every paragraph is
 * hidden too; otherwise the box stays drawn and the slide is approximate.
 *
 * `applyState` is idempotent and `clearState` leaves the elements exactly
 * as the renderer drew them (each inline value is kept aside the first
 * time a state touches it, and put back).
 *
 * `playStep` plays a click as motion between two of those states: it puts
 * the state before the click, runs one Web Animation per element it moves
 * (`fill: "both"`, sampled from `pptxMotion.ts`: two behaviours on one
 * CSS property, as a zoom's width and height on `scale`, can't be two
 * animations without `composite`) and one per colour it changes, and at
 * the end — or on `finish()` — puts the state after the click and cancels
 * them. Every animation's last frame is the landed value, so the landing
 * moves nothing.
 */

import { heldColor, parseCss, pictureFilter, toCss, type Palette } from "./pptxColor";
import { CHECKER_ROWS, clipMargin, effectMotion, patternShare, revealAt, type Geom, type MaskFrame, type Pattern, type Segment } from "./pptxMotion";
import { behaviourProgress, buildStates, effectTime, splitShapes, targetKey, type BuildState, type Effect, type HeldColor, type HeldColors, type SlideTimeline } from "./pptxTiming";
import type { TaggedShape, TaggedSlide } from "./pptxTag";

export type { Palette } from "./pptxColor";

/** An element a state acts on: `"4"` a shape, `"4#1"` its paragraph 1,
 *  `"4!bg"` the box drawn behind its text (a `:bg` key's motion and what
 *  it holds, and a split shape's whole-shape emphasis: README R32). */
export type ElementId = string;

/** What one element holds (R26–R29). */
export interface HeldPlan {
  scale?: [number, number];
  turn?: number;
  opacity?: number;
  colors?: HeldColors;
}

/** What the drawn shapes need, by shape id; pure so it can be tested. */
export interface StatePlan {
  hideShapes: Set<string>;
  /** Shape id → paragraph indexes to hide (only shapes with matched paragraphs). */
  hideParas: Map<string, Set<number>>;
  /** Shape id → offset in slide fractions. */
  moveShapes: Map<string, [number, number]>;
  /** Shape id → paragraph index → offset in slide fractions. */
  moveParas: Map<string, Map<number, [number, number]>>;
  /** Held size, turn, opacity and colours, by element. */
  held: Map<ElementId, HeldPlan>;
  /** Something in the state couldn't be shown as PowerPoint would. */
  approximate: boolean;
}

/** What the planner needs to know of a drawn shape. */
export interface ShapeInfo {
  paraCount: number;
  /** Its paragraphs were matched one to one. */
  paras: boolean;
  /** It draws its box as an element of its own (its `:bg` can move apart from its text). */
  bg?: boolean;
  /** Its text is animated by paragraph (`splitShapes`): emphasis on the
   *  whole shape plays on its body alone (R32). */
  split?: boolean;
}

const KEY = /^([0-9]{1,10})(?::(?:(bg)|p([0-9]{1,9})-([0-9]{1,9})))?$/;

interface Key {
  spid: string;
  bg: boolean;
  range: [number, number] | null;
}

function parseKey(k: string): Key | null {
  const m = KEY.exec(k);
  if (m === null) return null;
  return { spid: m[1]!, bg: m[2] !== undefined, range: m[3] === undefined ? null : [Number(m[3]), Number(m[4])] };
}

/** The elements a held value on target key `raw` lands on. */
function heldIds(raw: string, shapes: ReadonlyMap<string, ShapeInfo>, plan: StatePlan): ElementId[] {
  const k = parseKey(raw);
  if (k === null) return [];
  const info = shapes.get(k.spid);
  if (info === undefined) {
    plan.approximate = true;
    return [];
  }
  if (k.bg) {
    if (info.bg === true) return [`${k.spid}!bg`];
    plan.approximate = true;
    return [k.spid];
  }
  if (k.range === null) return [k.spid];
  if (!info.paras) {
    plan.approximate = true;
    return [k.spid];
  }
  const out: ElementId[] = [];
  for (let i = k.range[0]; i <= Math.min(k.range[1], info.paraCount - 1); i++) out.push(`${k.spid}#${i}`);
  return out;
}

export function planState(shapes: ReadonlyMap<string, ShapeInfo>, state: BuildState): StatePlan {
  const plan: StatePlan = {
    hideShapes: new Set(),
    hideParas: new Map(),
    moveShapes: new Map(),
    moveParas: new Map(),
    held: new Map(),
    approximate: false,
  };
  // Per shape: is the shape, the background, which paragraphs hidden.
  const per = new Map<string, { shape: boolean; bg: boolean; ranges: [number, number][] }>();
  for (const raw of state.hidden) {
    const k = parseKey(raw);
    if (k === null) continue;
    const p = per.get(k.spid) ?? { shape: false, bg: false, ranges: [] };
    if (k.bg) p.bg = true;
    else if (k.range !== null) p.ranges.push(k.range);
    else p.shape = true;
    per.set(k.spid, p);
  }
  for (const [spid, p] of per) {
    const info = shapes.get(spid);
    if (info === undefined) {
      plan.approximate = true;
      continue;
    }
    if (p.shape) {
      plan.hideShapes.add(spid);
      continue;
    }
    const hiddenPara = (i: number) => p.ranges.some(([a, b]) => a <= i && i <= b);
    let every = true;
    for (let i = 0; i < info.paraCount; i++) if (!hiddenPara(i)) every = false;
    const anyPara = p.ranges.length > 0;
    if (p.bg) {
      if (every) {
        plan.hideShapes.add(spid);
        continue;
      }
      plan.approximate = true;
    }
    if (!anyPara) continue;
    if (!info.paras) {
      // The drawn paragraphs aren't known one by one: the shape comes and
      // goes with its text as a whole.
      plan.approximate = true;
      if (every) plan.hideShapes.add(spid);
      continue;
    }
    const set = new Set<number>();
    for (let i = 0; i < info.paraCount; i++) if (hiddenPara(i)) set.add(i);
    if (set.size > 0) plan.hideParas.set(spid, set);
  }
  for (const [raw, off] of Object.entries(state.moved)) {
    const k = parseKey(raw);
    if (k === null) continue;
    const info = shapes.get(k.spid);
    if (info === undefined) {
      plan.approximate = true;
      continue;
    }
    if (k.range !== null && info.paras) {
      const m = plan.moveParas.get(k.spid) ?? new Map<number, [number, number]>();
      for (let i = k.range[0]; i <= Math.min(k.range[1], info.paraCount - 1); i++) m.set(i, off);
      plan.moveParas.set(k.spid, m);
      continue;
    }
    if (k.range !== null || k.bg) plan.approximate = true;
    plan.moveShapes.set(k.spid, off);
  }
  const heldOf = (id: ElementId): HeldPlan => {
    let h = plan.held.get(id);
    if (h === undefined) plan.held.set(id, (h = {}));
    return h;
  };
  for (const [raw, v] of Object.entries(state.scale ?? {})) for (const id of heldIds(raw, shapes, plan)) heldOf(id).scale = v;
  for (const [raw, v] of Object.entries(state.turn ?? {})) for (const id of heldIds(raw, shapes, plan)) heldOf(id).turn = v;
  for (const [raw, v] of Object.entries(state.opacity ?? {})) for (const id of heldIds(raw, shapes, plan)) heldOf(id).opacity = v;
  for (const [raw, v] of Object.entries(state.color ?? {})) {
    for (const id of heldIds(raw, shapes, plan)) {
      const h = heldOf(id);
      h.colors = { ...h.colors, ...v };
    }
  }
  return plan;
}

/**
 * The uniform scale a chain of inline transforms applies (`scale(s)` or
 * `scale(s, s)` only; none is 1), else null: a rotation, a flip or an
 * uneven scale turns an offset, and only a scale can be undone by dividing.
 */
export function transformScale(transforms: readonly string[]): number | null {
  let k = 1;
  for (const t of transforms) {
    const s = t.trim();
    if (s === "" || s === "none") continue;
    const rest = s.replace(/scale\(\s*([0-9.eE+-]+)\s*(?:,\s*([0-9.eE+-]+)\s*)?\)/g, (_, a: string, b: string | undefined) => {
      const x = Number(a);
      const y = b === undefined ? x : Number(b);
      if (!Number.isFinite(x) || x <= 0 || x !== y) k = NaN;
      else k *= x;
      return "";
    });
    if (rest.trim() !== "" || Number.isNaN(k)) return null;
  }
  return k;
}

/**
 * A paragraph's offset in its own px: the slide fractions in slide px,
 * divided by the scale of the transforms between it and the slide (the
 * renderer shrinks an overflowing text body with `scale()`). Null where
 * those transforms do more than scale (the offset would turn).
 */
export function paraOffset(off: readonly [number, number], w: number, h: number, transforms: readonly string[]): [number, number] | null {
  const k = transformScale(transforms);
  return k === null ? null : [(off[0] * w) / k, (off[1] * h) / k];
}


// --- the DOM side ---------------------------------------------------------------------

type Styled = HTMLElement | SVGElement;

/** Inline values an element had before a state touched it. */
interface Kept {
  hadStyle: boolean;
  inert: boolean;
  props: Map<string, string>;
}
const kept = new WeakMap<Element, Kept>();
/** Every element a state touched on a slide, so clearing finds them. */
const touched = new WeakMap<TaggedSlide, Set<Styled>>();

/** The value `prop` had before any state touched `el` ("" when unset). */
function original(el: Styled, prop: string): string {
  return kept.get(el)?.props.get(prop) ?? el.style.getPropertyValue(prop);
}

function setProp(t: TaggedSlide, el: Styled, prop: string, value: string): void {
  let k = kept.get(el);
  if (k === undefined) {
    k = { hadStyle: el.hasAttribute("style"), inert: el.hasAttribute("inert"), props: new Map() };
    kept.set(el, k);
  }
  if (!k.props.has(prop)) k.props.set(prop, el.style.getPropertyValue(prop));
  el.style.setProperty(prop, value);
  let set = touched.get(t);
  if (set === undefined) touched.set(t, (set = new Set()));
  set.add(el);
}

function setInert(t: TaggedSlide, el: Styled, on: boolean): void {
  setProp(t, el, "opacity", el.style.getPropertyValue("opacity"));
  if (on) el.setAttribute("inert", "");
  else if (!kept.get(el)!.inert) el.removeAttribute("inert");
}

function restore(el: Styled): void {
  const k = kept.get(el);
  if (k === undefined) return;
  kept.delete(el);
  for (const [prop, v] of k.props) {
    if (v === "") el.style.removeProperty(prop);
    else el.style.setProperty(prop, v);
  }
  if (!k.inert) el.removeAttribute("inert");
  if (!k.hadStyle && el.style.cssText === "") el.removeAttribute("style");
}

/** One inline value a state puts on: what `applyState` writes and what a
 *  click animates between. */
export interface Paint {
  el: Styled;
  /** A CSS property, kebab-case. */
  prop: string;
  value: string;
}

/** Bounds every value a file can push to the screen. */
const LIMIT = { px: 1e5, scale: 1000, deg: 1e6 };
const fin = (n: number, lo: number, hi: number, fallback: number) => (Number.isFinite(n) ? Math.min(hi, Math.max(lo, n)) : fallback);
const r3 = (n: number) => Math.round(n * 1000) / 1000;
/** The smallest scale a frame shows. A frame at scale exactly 0 makes
 *  WebKit build the animated layer's backing at no size, and it draws the
 *  shape washed out for the rest of the click (zoom in, measured in a
 *  WKWebView); 0.001 is as invisible and keeps the backing whole. */
const MIN_SCALE = 0.001;

const SHAPE_TAGS = "path, rect, ellipse, circle, polygon, polyline, line";
const NOT_DRAWN = "defs, clipPath, mask, pattern, filter, marker, symbol";

/** The drawn (not defining) shapes of the SVGs inside `el`. */
function drawnShapes(el: Element): SVGElement[] {
  return Array.from(el.querySelectorAll<SVGElement>(`svg ${SHAPE_TAGS.split(", ").join(", svg ")}`)).filter((s) => s.closest(NOT_DRAWN) === null);
}

/** A gradient's first stop colour (its lowest offset). */
function firstStop(grad: Element): string {
  const stops = Array.from(grad.querySelectorAll("stop"));
  const at = (st: Element) => parseFloat(st.getAttribute("offset") ?? "0") || 0;
  const first = stops.sort((a, b) => at(a) - at(b))[0];
  return first?.getAttribute("stop-color") ?? "#000000";
}

function gradientOf(shape: SVGElement, fill: string): Element | null {
  const m = /^url\(\s*["']?#([^"')]+)["']?\s*\)$/.exec(fill.trim());
  if (m === null) return null;
  const svg = shape.ownerSVGElement;
  if (svg === null) return null;
  for (const g of Array.from(svg.querySelectorAll("linearGradient, radialGradient"))) if (g.id === m[1]) return g;
  return null;
}

/** The paints that make `colors` show on a drawn shape `el` (its own
 *  spans, or `spans` when the colour is a paragraph's). False when some of
 *  it couldn't be shown exactly. */
function colorPaints(el: Element, colors: HeldColors, palette: Palette | null, spans: readonly Element[] | null, out: Paint[]): boolean {
  let exact = true;
  const pal = palette ?? { scheme: new Map(), map: new Map() };
  const shapes = drawnShapes(el);
  const resolved = (base: string | null, held: HeldColor): string | null => {
    const c = heldColor(base === null ? null : parseCss(base), held, pal);
    if (c === null) exact = false;
    return c === null ? null : toCss(c);
  };
  const fill = colors.fill;
  if (fill !== undefined) {
    const pictures = Array.from(el.querySelectorAll<SVGElement>("svg image")).filter((i) => i.closest(NOT_DRAWN) === null);
    if (fill.to !== undefined) {
      // One colour over whatever was drawn (gradient, picture, none).
      const c = resolved(null, fill);
      if (c !== null) {
        const filled = shapes.filter((s) => (s.getAttribute("fill") ?? "") !== "none");
        const outline = filled.length > 0 ? filled : pictures.length > 0 || fill.solid || fill.on ? shapes.slice(0, 1) : [];
        for (const s of outline) out.push({ el: s, prop: "fill", value: c });
      }
    } else {
      for (const s of shapes) {
        const f = s.getAttribute("fill") ?? "";
        if (f === "none" || f === "") continue;
        const grad = gradientOf(s, f);
        if (grad !== null && fill.solid === true) {
          // PowerPoint makes the fill solid, from the gradient's first stop
          // (R29; matched against its own film: darken, lighten,
          // desaturate and complementary colour on a gradient).
          const c = resolved(firstStop(grad), fill);
          if (c !== null) out.push({ el: s, prop: "fill", value: c });
        } else if (grad !== null) {
          for (const stop of Array.from(grad.querySelectorAll<SVGElement>("stop"))) {
            const c = resolved(stop.getAttribute("stop-color") ?? "#000000", fill);
            if (c !== null) out.push({ el: stop, prop: "stop-color", value: c });
          }
        } else {
          const c = resolved(f, fill);
          if (c !== null) out.push({ el: s, prop: "fill", value: c });
        }
      }
      for (const img of pictures) {
        const f = pictureFilter(fill);
        exact = false;
        if (f !== null) out.push({ el: img, prop: "filter", value: f });
      }
    }
  }
  const line = colors.line;
  if (line !== undefined) {
    const stroked = shapes.filter((s) => !["none", ""].includes(s.getAttribute("stroke") ?? ""));
    if (stroked.length === 0 && line.to !== undefined && line.on === true && shapes[0] !== undefined) {
      const c = resolved(null, line);
      if (c !== null) out.push({ el: shapes[0], prop: "stroke", value: c }, { el: shapes[0], prop: "stroke-width", value: "1" });
    }
    for (const s of stroked) {
      const c = resolved(s.getAttribute("stroke"), line);
      if (c !== null) out.push({ el: s, prop: "stroke", value: c });
    }
  }
  const text = colors.text;
  if (text !== undefined) {
    for (const node of spans ?? Array.from(el.querySelectorAll<HTMLElement>("span"))) {
      const span = node as HTMLElement;
      if (span.querySelector("span") !== null) continue;
      const c = resolved(original(span, "color") || "#000000", text);
      if (c !== null) out.push({ el: span, prop: "color", value: c });
    }
  }
  return exact;
}

const textOrigins = new WeakMap<Element, string | null>();

/**
 * Where a paragraph grows and turns about, as a `transform-origin` in its
 * own px: the centre of its text (the union of its lines' boxes), not of
 * its block. PowerPoint's film of the `order` deck: paragraphs holding a
 * 1.5x grow stay centred on their words (left-aligned text 634-687 px of
 * 1920 grows to 620-700), where the block's centre would put them 95 px
 * left. Read once per drawn paragraph, before any state transforms it
 * (the renderer draws a fresh element for each layout); null where it has
 * no text or no layout yet.
 */
function textOrigin(el: Styled): string | null {
  const hit = textOrigins.get(el);
  if (hit !== undefined) return hit;
  let out: string | null = null;
  if (typeof document !== "undefined" && typeof document.createRange === "function" && el instanceof HTMLElement && el.offsetWidth > 0) {
    const block = el.getBoundingClientRect();
    const range = document.createRange();
    range.selectNodeContents(el);
    const rs = Array.from(range.getClientRects()).filter((r) => r.width > 0 && r.height > 0);
    // Screen px per own px (the stage's fit, a scaled text body).
    const f = block.width / el.offsetWidth;
    if (rs.length > 0 && f > 0 && Number.isFinite(f)) {
      const x0 = Math.min(...rs.map((r) => r.left));
      const x1 = Math.max(...rs.map((r) => r.right));
      const y0 = Math.min(...rs.map((r) => r.top));
      const y1 = Math.max(...rs.map((r) => r.bottom));
      out = `${r3(((x0 + x1) / 2 - block.left) / f)}px ${r3(((y0 + y1) / 2 - block.top) / f)}px`;
    }
  }
  // No layout yet (a hidden pane): read again next time.
  if (out !== null) textOrigins.set(el, out);
  return out;
}

function drawnOpacity(el: Styled): number {
  const s = original(el, "opacity");
  const n = s === "" ? 1 : Number(s);
  return Number.isFinite(n) ? Math.max(0, Math.min(1, n)) : 1;
}

/**
 * The inline values `state` needs on a tagged slide drawn `w`×`h` slide
 * px, and whether some of it could only be approximated. Pure of the DOM
 * apart from reading it: nothing is written.
 */
export function statePaints(tagged: TaggedSlide, state: BuildState, w: number, h: number, palette: Palette | null): { paints: Paint[]; inert: Styled[]; approximate: boolean } {
  const info = new Map<string, ShapeInfo>();
  for (const [id, s] of tagged.shapes) info.set(id, shapeInfo(s));
  const plan = planState(info, state);
  let approximate = plan.approximate;
  const paints: Paint[] = [];
  const inert: Styled[] = [];
  const place = (el: Styled, id: ElementId, hidden: boolean, px: [number, number] | undefined) => {
    const held = plan.held.get(id);
    if (hidden) {
      paints.push({ el, prop: "opacity", value: "0" });
      inert.push(el);
    } else if (held?.opacity !== undefined) paints.push({ el, prop: "opacity", value: String(r3(drawnOpacity(el) * held.opacity)) });
    if (px !== undefined && (px[0] !== 0 || px[1] !== 0)) {
      paints.push({ el, prop: "translate", value: `${r3(fin(px[0], -LIMIT.px, LIMIT.px, 0))}px ${r3(fin(px[1], -LIMIT.px, LIMIT.px, 0))}px` });
    }
    if (held?.scale !== undefined) {
      const [x, y] = held.scale.map((v) => r3(fin(v, 0, LIMIT.scale, 1)));
      paints.push({ el, prop: "scale", value: `${x} ${y}` });
    }
    if (held?.turn !== undefined) paints.push({ el, prop: "rotate", value: `${r3(fin(held.turn, -LIMIT.deg, LIMIT.deg, 0))}deg` });
    if (id.includes("#") && (held?.scale !== undefined || held?.turn !== undefined)) {
      const o = textOrigin(el);
      if (o !== null) paints.push({ el, prop: "transform-origin", value: o });
    }
  };
  for (const [id, s] of tagged.shapes) {
    const off = plan.moveShapes.get(id);
    // The shape's own `translate` acts in the slide's px, before its transform.
    place(s.el, id, plan.hideShapes.has(id), off === undefined ? undefined : [off[0] * w, off[1] * h]);
    const colors = plan.held.get(id)?.colors;
    if (colors !== undefined && !colorPaints(s.el, colors, palette, null, paints)) approximate = true;
    // The body a `:bg` key holds a value for: the box's own drawing.
    const box = plan.held.has(`${id}!bg`) ? s.el.querySelector<SVGElement>(":scope > svg") : null;
    if (box !== null) {
      place(box, `${id}!bg`, false, undefined);
      const bc = plan.held.get(`${id}!bg`)?.colors;
      if (bc !== undefined && !colorPaints(box, bc, palette, null, paints)) approximate = true;
    }
    if (s.paras === null) continue;
    const hide = plan.hideParas.get(id);
    const move = plan.moveParas.get(id);
    s.paras.forEach((p, i) => {
      const m = move?.get(i);
      let px: [number, number] | undefined;
      if (m !== undefined) {
        const chain: string[] = [];
        for (let e = p.parentElement; e !== null; e = e.parentElement) {
          chain.push(e.style.transform);
          if (e === s.el) break;
        }
        const exact = paraOffset(m, w, h, chain);
        if (exact === null) approximate = true;
        px = exact ?? [m[0] * w, m[1] * h];
      }
      place(p, `${id}#${i}`, hide?.has(i) === true, px);
      const pc = plan.held.get(`${id}#${i}`)?.colors;
      if (pc !== undefined && !colorPaints(s.el, { text: pc.text }, palette, Array.from(p.querySelectorAll("span")), paints)) approximate = true;
    });
  }
  return { paints, inert, approximate: approximate || tagged.fellBack };
}

/** Put paints on a slide (after clearing what a previous state left). */
function paint(tagged: TaggedSlide, paints: readonly Paint[], inert: readonly Styled[]): void {
  clearState(tagged);
  for (const p of paints) setProp(tagged, p.el, p.prop, p.value);
  for (const el of inert) setInert(tagged, el, true);
}

/**
 * Show `state` on a tagged slide drawn `w`×`h` slide px; `palette` is the
 * slide's theme (held colours that name a scheme colour need it). True
 * when some of it could only be approximated (a target with no tagged
 * element, a paragraph build on unmatched paragraphs, a `:bg` drawn with
 * its text, a colour it couldn't resolve, a picture recoloured by filter).
 */
export function applyState(tagged: TaggedSlide, state: BuildState, w: number, h: number, palette: Palette | null = null): boolean {
  const { paints, inert, approximate } = statePaints(tagged, state, w, h, palette);
  paint(tagged, paints, inert);
  return approximate;
}

/** The slide as drawn: every shape and paragraph back as the renderer left it. */
export function clearState(tagged: TaggedSlide): void {
  const set = touched.get(tagged);
  if (set !== undefined) {
    for (const el of set) restore(el);
    set.clear();
  }
  // Elements a state touched before this slide was tracked (none in
  // practice; cheap): the tagged shapes and paragraphs themselves.
  for (const s of tagged.shapes.values()) {
    restore(s.el);
    for (const p of s.paras ?? []) restore(p);
  }
}

// --- playback ---------------------------------------------------------------------

/** The slide before step 0 runs: every entrance's target hidden, nothing moved. */
export function entryState(timeline: SlideTimeline): BuildState {
  return buildStates({ ...timeline, steps: [[], ...timeline.steps] })[0]!;
}

/** Longest a click plays before it lands on its state. */
export const PLAY_MAX_MS = 20_000;
/** Keyframes a second, at most, while something moves. */
const FRAMES_PER_S = 60;
/** Keyframes an element gets for a whole step, at most (event samples included). */
export const MAX_FRAMES = 600;
/** Past these a click lands without playing: what a deck at the parser's
 *  caps would otherwise cost to plan. */
export const PLAY_BUDGET = { effects: 300, events: 200 } as const;

/** What playback needs to know of a drawn element before the step. */
export interface ElementBase {
  /** Its place in slide fractions (centre and size). */
  geom: Geom;
  /** A paragraph's `transform-origin`: the centre of its text (`textOrigin`). */
  origin?: string;
  /** Its own px per slide px (a paragraph inside a scaled text body). */
  k: number;
  /** Its `translate` before the step, own px. */
  tx: number;
  ty: number;
  /** Its drawn opacity (the renderer's), and what it holds before the step. */
  drawn: number;
  held: number;
  /** Its held scale and turn before the step. */
  sx: number;
  sy: number;
  rot: number;
  /** The box a clip effect runs across, in its own px: the shape's box, or
   *  a paragraph's text (not its block: PowerPoint wipes across the
   *  words), at (`bx`, `by`) in the element. */
  bw: number;
  bh: number;
  bx?: number;
  by?: number;
  /** A paragraph's font size, its own px (its first run's). */
  em?: number;
}

/**
 * How far past a paragraph's words a wipe from the left or right lays its
 * sweep, on each side, in the paragraph's font size. PowerPoint's film
 * (one-line paragraphs at 40 pt, left and centred, a 253 px and a 1,287 px
 * line, wiped from either side over 1 s; and the `paragraphs` deck's 28 pt
 * line over 500 ms) moves the edge at (words + 1.4 em) × 1.15 per duration:
 * 424 and 421 px/s against 1.15 × (253 + 112) = 420, 1,631 and 1,636
 * against 1,609, 414 when centred; the D9 line 0.58 px/ms against 0.57.
 * The words alone (as for a shape's box) ran 30 % slow on a short line.
 */
export const WIPE_PAD_EM = 0.7;
/** A horizontal wipe: PowerPoint's film shows the padded sweep for these. */
const HORIZONTAL_WIPE = /^wipe(?:\((?:left|right)?\))?$/;

/** An element's inline values once the click has landed (`none` where the
 *  landed state sets nothing): every track's last frame. */
export interface Landed {
  opacity: number;
  translate: string;
  scale: string;
  rotate: string;
}

export interface PlayKeyframe {
  offset: number;
  opacity?: number;
  translate?: string;
  scale?: string;
  rotate?: string;
  /** A paragraph's origin while it grows or turns (`ElementBase.origin`). */
  transformOrigin?: string;
  /** A transition filter's mask (`maskImage` "none" once it has landed). */
  maskImage?: string;
  maskSize?: string;
  maskPosition?: string;
  maskRepeat?: string;
  maskComposite?: string;
  /** A pattern filter's progress (0–1), drawn by `withMasks` as its picture. */
  mask?: number;
}

export interface StepPlan {
  /** ms; 0 when nothing moves (the step lands at once). */
  duration: number;
  tracks: { id: ElementId; keyframes: PlayKeyframe[]; pattern?: Pattern; box?: [number, number, number, number]; margin?: number; levels?: number }[];
  /** Why some of it plays other than as written. */
  notes: string[];
  /** Past `PLAY_BUDGET` or `MAX_FRAMES`: the click lands without playing. */
  overBudget: boolean;
}

/** What the planner reads of a tagged shape. */
function shapeInfo(s: TaggedShape): ShapeInfo {
  return { paraCount: s.paraCount, paras: s.paras !== null, bg: s.el.querySelector(":scope > svg") !== null && s.paraCount > 0 };
}

/** The elements a target key acts on; a `:bg` key's motion moves the box
 *  alone where the shape draws one (`motion`). */
function elementsOf(key: string, shapes: ReadonlyMap<string, ShapeInfo>, motion = false): ElementId[] {
  const k = parseKey(key);
  if (k === null) return [];
  const info = shapes.get(k.spid);
  if (info === undefined) return [];
  if (k.bg && motion && info.bg === true) return [`${k.spid}!bg`];
  if (k.range === null || !info.paras) return [k.spid];
  const out: ElementId[] = [];
  for (let i = k.range[0]; i <= Math.min(k.range[1], info.paraCount - 1); i++) out.push(`${k.spid}#${i}`);
  return out;
}

function union(gs: Geom[]): Geom | null {
  if (gs.length === 0) return null;
  let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
  for (const g of gs) {
    x0 = Math.min(x0, g.ppt_x - g.ppt_w / 2);
    x1 = Math.max(x1, g.ppt_x + g.ppt_w / 2);
    y0 = Math.min(y0, g.ppt_y - g.ppt_h / 2);
    y1 = Math.max(y1, g.ppt_y + g.ppt_h / 2);
  }
  return { ppt_x: (x0 + x1) / 2, ppt_y: (y0 + y1) / 2, ppt_w: x1 - x0, ppt_h: y1 - y0, sx: gs[0]!.sx, sy: gs[0]!.sy, turn: gs[0]!.turn, op: gs[0]!.op };
}

/** The last snapshot at or before `t` (strictly before with `pre`). */
function snapshotAt<T>(snaps: readonly { at: number; v: T }[], t: number, pre: boolean): T {
  let lo = 0;
  let hi = snaps.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    const at = snaps[mid]!.at;
    if (pre ? at < t : at <= t) lo = mid;
    else hi = mid - 1;
  }
  return snaps[lo]!.v;
}

const parseDeg = (s: string) => (s === "none" || s === "" ? 0 : parseFloat(s));

/**
 * The keyframes step `effects` plays from state `before`: one track per
 * element it moves or shows, on one clock from the step's start
 * (`duration`, capped at `maxMs`). Pure: `base` measures the drawn
 * elements, `landed` gives each one's values after the click, `elements`
 * lists every element on the slide (shapes and `spid#i` paragraphs), and
 * `w`×`h` is the slide in px. Visibility follows the step's sets at their
 * begin times: each element's own hidden flag is read off `planState` for
 * the hidden keys at each time, so an element no set names (a paragraph a
 * box's background brought in with it) still shows exactly what the
 * states say.
 */
export function planStep(
  effects: readonly Effect[],
  before: BuildState,
  shapes: ReadonlyMap<string, ShapeInfo>,
  elements: readonly ElementId[],
  base: (id: ElementId) => ElementBase | null,
  landed: (id: ElementId) => Landed | null,
  w: number,
  h: number,
  maxMs = PLAY_MAX_MS,
): StepPlan {
  const empty = (overBudget: boolean, notes: string[] = []): StepPlan => ({ duration: 0, tracks: [], notes, overBudget });
  if (effects.length > PLAY_BUDGET.effects) return empty(true);
  const events: { at: number; key: string; to: "visible" | "hidden" }[] = [];
  for (const e of effects) {
    for (const b of e.behaviours) {
      if (b.kind !== "set" || !b.attrs.includes("style.visibility") || b.target === null) continue;
      if (b.to !== "visible" && b.to !== "hidden") continue;
      events.push({ at: e.start + b.delay, key: targetKey(b.target), to: b.to });
    }
  }
  if (events.length > PLAY_BUDGET.events) return empty(true);

  const bases = new Map<ElementId, ElementBase | null>();
  const baseOf = (id: ElementId) => {
    if (!bases.has(id)) bases.set(id, base(id));
    return bases.get(id) ?? null;
  };
  const geom = (key: string): Geom | null =>
    union(
      elementsOf(key, shapes)
        .map(baseOf)
        .filter((b): b is ElementBase => b !== null)
        .map((b) => ({ ...b.geom, sx: b.sx, sy: b.sy, turn: b.rot, op: b.held })),
    );

  const motions = effects.map((e) => effectMotion(e, geom));
  const notes = [...new Set(motions.map((m) => m.note).filter((n): n is string => n !== null))];
  let duration = 0;
  for (const e of effects) if (e.dur !== null) duration = Math.max(duration, e.start + e.dur);
  duration = Math.min(duration, maxMs);
  if (!(duration > 0)) return empty(false, notes);

  // Each element's own hidden flag over the step: one plan per event time.
  events.sort((a, b) => a.at - b.at);
  const hidden = new Set(before.hidden);
  const snaps: { at: number; v: StatePlan }[] = [{ at: -Infinity, v: planState(shapes, { hidden: [...hidden], moved: {} }) }];
  for (let i = 0; i < events.length; ) {
    const at = events[i]!.at;
    for (; i < events.length && events[i]!.at === at; i++) {
      const ev = events[i]!;
      if (ev.to === "hidden") hidden.add(ev.key);
      else hidden.delete(ev.key);
    }
    snaps.push({ at, v: planState(shapes, { hidden: [...hidden], moved: {} }) });
  }
  const hiddenIn = (plan: StatePlan, id: ElementId): boolean => {
    const bang = id.indexOf("!");
    if (bang >= 0) return plan.hideShapes.has(id.slice(0, bang));
    const [spid, para] = id.split("#");
    return para === undefined ? plan.hideShapes.has(spid!) : plan.hideParas.get(spid!)?.has(Number(para)) === true;
  };
  const hiddenAt = (id: ElementId, t: number, pre: boolean) => hiddenIn(snapshotAt(snaps, t, pre), id);

  // The elements whose own hidden flag changes during the step.
  const visIds = new Set<ElementId>();
  for (const id of elements) {
    const first = hiddenIn(snaps[0]!.v, id);
    if (snaps.some((s) => hiddenIn(s.v, id) !== first) && baseOf(id) !== null) visIds.add(id);
  }
  const per = new Map<ElementId, Segment[]>();
  for (const [mi, m] of motions.entries()) {
    const emph = effects[mi]!.presetClass === "emph";
    for (const seg of m.segments) {
      // Emphasis on a split shape's whole plays on its body (R32).
      const key = emph && /^[0-9]+$/.test(seg.key) && shapes.get(seg.key)?.split === true ? `${seg.key}:bg` : seg.key;
      for (const id of elementsOf(key, shapes, true)) {
        if (baseOf(id) === null) continue;
        const list = per.get(id) ?? [];
        list.push(seg);
        per.set(id, list);
      }
    }
  }
  for (const id of visIds) if (!per.has(id)) per.set(id, []);

  // When to sample: the step's ends, both sides of each visibility change,
  // and evenly through each moving span, all within MAX_FRAMES.
  const times: { t: number; pre: boolean }[] = [
    { t: 0, pre: false },
    { t: duration, pre: false },
  ];
  for (const at of new Set(events.map((e) => e.at))) if (at > 0 && at <= duration) times.push({ t: at, pre: true }, { t: at, pre: false });
  const spans = motions.filter((m) => m.segments.length > 0).map((m) => [m.from, Math.min(m.to, duration)] as const);
  const room = MAX_FRAMES - times.length;
  if (room < 2 && spans.length > 0) return empty(true, notes);
  const total = spans.reduce((n, [a, b]) => n + Math.max(0, b - a), 0);
  const step = Math.max(1000 / FRAMES_PER_S, total / room);
  let grid: number[] = [];
  for (const [a, b] of spans) {
    for (let t = a; t < b; t += step) grid.push(t);
    grid.push(b);
  }
  // Per-span ends can push past the cap: thin the even samples, never the
  // step's ends or a visibility change.
  if (grid.length > room) {
    const every = Math.ceil(grid.length / room);
    grid = grid.filter((_, i) => i % every === 0);
  }
  for (const t of grid) times.push({ t, pre: false });
  times.sort((a, b) => a.t - b.t || (a.pre === b.pre ? 0 : a.pre ? -1 : 1));
  const samples = times.filter((s, i) => i === 0 || s.t !== times[i - 1]!.t || s.pre !== times[i - 1]!.pre);

  const tracks: StepPlan["tracks"] = [];
  for (const [id, segs] of per) {
    const b = baseOf(id)!;
    const has = (...chs: string[]) => segs.some((s) => chs.includes(s.ch));
    const moves = has("x", "y", "mx", "my");
    const scales = has("w", "h", "sx", "sy", "sxAbs", "syAbs");
    const turns = has("rot", "rotAbs");
    const clips = has("clip");
    const pattern = segs.find((s) => s.ch === "mask")?.pattern;
    const fades = has("op", "opAbs") || visIds.has(id);
    if (!moves && !scales && !turns && !clips && !fades && pattern === undefined) continue;
    const g = b.geom;
    const keyframes: PlayKeyframe[] = samples.map(({ t, pre }) => {
      const at = pre ? t - 0.001 : t;
      // Absolute channels: the last segment (in document order) that has begun.
      const abs: Partial<Record<string, number>> = {};
      let sx = 1;
      let sy = 1;
      let rot = 0;
      let op = 1;
      let clip: MaskFrame | null = null;
      let maskP: number | null = null;
      for (const s of segs) {
        const v = s.value(at);
        if (v === null || !Number.isFinite(v)) continue;
        switch (s.ch) {
          case "sx":
            sx *= v;
            break;
          case "sy":
            sy *= v;
            break;
          case "rot":
            rot += v;
            break;
          case "op":
            op *= v;
            break;
          case "clip": {
            // A paragraph's horizontal wipe sweeps past its words (WIPE_PAD_EM).
            const pad = id.includes("#") && b.em !== undefined && HORIZONTAL_WIPE.test(s.filter ?? "") ? WIPE_PAD_EM * b.em : 0;
            clip = s.region!(Math.min(1, Math.max(0, v)), b.bw + 2 * pad, b.bh, (b.bx ?? 0) - pad, b.by ?? 0);
            break;
          }
          case "mask":
            maskP = Math.min(1, Math.max(0, v));
            break;
          default:
            abs[s.ch] = v;
        }
      }
      const f: PlayKeyframe = { offset: Math.min(1, Math.max(0, t / duration)) };
      if (fades) f.opacity = r3(hiddenAt(id, t, pre) ? 0 : b.drawn * fin(op * (abs.opAbs ?? b.held), 0, 1, 1));
      if (moves) {
        const dx = (abs.x ?? g.ppt_x) - g.ppt_x + (abs.mx ?? 0);
        const dy = (abs.y ?? g.ppt_y) - g.ppt_y + (abs.my ?? 0);
        // A path that has begun is an offset from the layout place: the
        // offset an earlier path left goes (R18).
        const tx = abs.mx === undefined ? b.tx : 0;
        const ty = abs.my === undefined ? b.ty : 0;
        f.translate = `${r3(fin(tx + dx * w * b.k, -LIMIT.px, LIMIT.px, b.tx))}px ${r3(fin(ty + dy * h * b.k, -LIMIT.px, LIMIT.px, b.ty))}px`;
      }
      if (scales) {
        const kx = g.ppt_w > 0 ? (abs.w ?? g.ppt_w) / g.ppt_w : 1;
        const ky = g.ppt_h > 0 ? (abs.h ?? g.ppt_h) / g.ppt_h : 1;
        f.scale = `${r3(fin(kx * (abs.sxAbs ?? b.sx) * sx, MIN_SCALE, LIMIT.scale, b.sx))} ${r3(fin(ky * (abs.syAbs ?? b.sy) * sy, MIN_SCALE, LIMIT.scale, b.sy))}`;
      }
      if (turns) f.rotate = `${r3(fin((abs.rotAbs ?? b.rot) + rot, -LIMIT.deg, LIMIT.deg, b.rot))}deg`;
      if ((scales || turns) && b.origin !== undefined) f.transformOrigin = b.origin;
      if (clips) Object.assign(f, clip ?? { maskImage: "none" });
      // Before its pattern begins, an exit shows whole (an entrance is hidden).
      if (pattern !== undefined) f.mask = r3(maskP ?? (pattern.out ? 0 : 1));
      return f;
    });
    land(keyframes, landed(id));
    if (pattern === undefined) {
      // A clip's mask covers the box pushed out by the margin (`clipRegion`).
      const e = clipMargin(b.bw, b.bh);
      tracks.push(clips ? { id, keyframes, box: [(b.bx ?? 0) - e, (b.by ?? 0) - e, b.bw + 2 * e, b.bh + 2 * e] } : { id, keyframes });
    }
    else {
      // The pattern covers the box, pushed out like a clip so a shadow or
      // outline drawn past the box goes with it.
      const e = clipMargin(b.bw, b.bh);
      tracks.push({ id, keyframes, pattern, box: [(b.bx ?? 0) - e, (b.by ?? 0) - e, b.bw + 2 * e, b.bh + 2 * e], margin: e, levels: maskLevels(duration) });
    }
  }
  return { duration, tracks, notes, overBudget: false };
}

/** Make a track's last frame the landed values exactly. A turn that ends a
 *  whole number of circles off the landed one is shifted by them, so the
 *  last step doesn't spin back. */
function land(kf: PlayKeyframe[], to: Landed | null): void {
  const last = kf[kf.length - 1];
  if (last === undefined || to === null) return;
  if (last.opacity !== undefined) last.opacity = to.opacity;
  if (last.translate !== undefined) last.translate = to.translate;
  if (last.scale !== undefined) last.scale = to.scale;
  if (last.rotate !== undefined) {
    const shift = parseDeg(to.rotate) - parseDeg(last.rotate);
    const turns = Math.round(shift / 360);
    if (turns !== 0 && Math.abs(shift - turns * 360) < 0.01) {
      for (const f of kf) if (f.rotate !== undefined) f.rotate = `${r3(parseDeg(f.rotate) + turns * 360)}deg`;
    }
    last.rotate = to.rotate;
  }
  if (last.maskImage !== undefined) last.maskImage = "none";
  if (last.mask !== undefined) last.mask = 1;
}

/** A click being played. */
export interface Playing {
  /** Resolves once the step stands on its state (played out or finished). */
  finished: Promise<void>;
  /** Stop now and stand on the step's state. Idempotent. */
  finish(): void;
  /** The step's state could only be approximated on this slide. */
  approximate: boolean;
}

export interface PlayOptions {
  /** The slide in px. */
  w: number;
  h: number;
  /** `buildStates(timeline)`, when the caller has it. */
  states?: readonly BuildState[];
  /** The slide's theme (held colours that name scheme colours need it). */
  palette?: Palette | null;
  /** No motion: stand on the state at once. */
  reduced?: boolean;
  /** This click planned ahead (`prepareStep`); used only when it is for
   *  the same slide, timeline and step. */
  prepared?: PreparedStep | null;
  maxMs?: number;
}

function pxPair(v: string): [number, number] | null {
  if (v.trim() === "" || v === "none") return [0, 0];
  const m = /^\s*(-?[0-9.eE+-]+)px(?:\s+(-?[0-9.eE+-]+)px)?\s*$/.exec(v);
  if (m === null) return null;
  const x = Number(m[1]);
  const y = m[2] === undefined ? 0 : Number(m[2]);
  return Number.isFinite(x) && Number.isFinite(y) ? [x, y] : null;
}

function elementOf(tagged: TaggedSlide, id: ElementId): Styled | undefined {
  const bang = id.indexOf("!");
  if (bang >= 0) {
    const el = tagged.shapes.get(id.slice(0, bang))?.el;
    return el?.querySelector<SVGElement>(":scope > svg") ?? undefined;
  }
  const [spid, para] = id.split("#");
  const shape = tagged.shapes.get(spid!);
  return para === undefined ? shape?.el : shape?.paras?.[Number(para)];
}

/** A box on screen (client px, as `getBoundingClientRect` gives it). */
export interface ClientBox {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * A drawn paragraph's place in slide fractions, from its box and the
 * slide's box on screen. The screen px per slide px is measured (the
 * slide's box over its own width `w`), never assumed: the show fits the
 * slide with `zoom` and browsing with a transform, and engines report
 * boxes under `zoom` differently, but a box and the slide's box are
 * always in the same units.
 */
export function boxGeom(box: ClientBox, slide: ClientBox, w: number, h: number): Geom {
  const k = slide.width > 0 ? slide.width / w : 1;
  return {
    ppt_x: (box.left + box.width / 2 - slide.left) / k / w,
    ppt_y: (box.top + box.height / 2 - slide.top) / k / h,
    ppt_w: box.width / k / w,
    ppt_h: box.height / k / h,
  };
}

/** Measure a tagged element (after the step's starting state is on it).
 *  Every length comes from inline slide px or from `boxGeom`'s measured
 *  factor; nothing reads `offset*`/`client*`, whose meaning under `zoom`
 *  differs between engines. */
function measure(tagged: TaggedSlide, id: ElementId, w: number, h: number): ElementBase | null {
  const spid = id.split(/[#!]/)[0]!;
  const shape = tagged.shapes.get(spid);
  const el = elementOf(tagged, id);
  if (shape === undefined || el === undefined) return null;
  const t = pxPair(el.style.translate);
  if (t === null) return null;
  const sc = el.style.scale.trim();
  const [sx, sy] = sc === "" || sc === "none" ? [1, 1] : sc.split(/\s+/).map(Number).concat(Number(sc.split(/\s+/)[0])).slice(0, 2);
  const rot = parseDeg(el.style.rotate.trim());
  const opNow = el.style.opacity;
  const drawn = drawnOpacity(el);
  const held = opNow === "" || opNow === "0" || drawn === 0 ? 1 : Math.min(1, Number(opNow) / drawn);
  let geom: Geom;
  let k = 1;
  let textAt: [number, number] = [0, 0];
  if (!id.includes("#")) {
    const px = (v: string) => (v.endsWith("px") ? parseFloat(v) : NaN);
    const s = shape.el.style;
    const [x, y, bw, bh] = [px(s.left), px(s.top), px(s.width), px(s.height)];
    if (![x, y, bw, bh].every(Number.isFinite)) return null;
    geom = { ppt_x: (x + bw / 2) / w, ppt_y: (y + bh / 2) / h, ppt_w: bw / w, ppt_h: bh / h };
  } else {
    const slideEl = shape.el.parentElement;
    if (slideEl === null) return null;
    const slideBox = slideEl.getBoundingClientRect();
    const block = el.getBoundingClientRect();
    // The paragraph's text, not its block: the union of its lines' boxes.
    let text: ClientBox = block;
    if (typeof document !== "undefined" && typeof document.createRange === "function") {
      const range = document.createRange();
      range.selectNodeContents(el);
      const rs = Array.from(range.getClientRects()).filter((r) => r.width > 0 && r.height > 0);
      if (rs.length > 0) {
        const x0 = Math.min(...rs.map((r) => r.left));
        const y0 = Math.min(...rs.map((r) => r.top));
        text = { left: x0, top: y0, width: Math.max(...rs.map((r) => r.right)) - x0, height: Math.max(...rs.map((r) => r.bottom)) - y0 };
      }
    }
    geom = boxGeom(text, slideBox, w, h);
    const f = slideBox.width > 0 ? slideBox.width / w : 1;
    textAt = [(text.left - block.left) / f, (text.top - block.top) / f];
    const chain: string[] = [];
    for (let e = el.parentElement; e !== null; e = e.parentElement) {
      chain.push(e.style.transform);
      if (e === shape.el) break;
    }
    const body = transformScale(chain);
    k = body === null || body <= 0 ? 1 : 1 / body;
  }
  // The clip box in the element's own px: slide px, times a paragraph's
  // text-body factor (its own px are the body's, scaled into the slide).
  // A whole shape's is what it draws, outline and shadow included.
  const pad = id.includes("#") ? [0, 0, 0, 0] : inkPad(shape.el);
  const box = [geom.ppt_w * w * k + pad[0]! + pad[2]!, geom.ppt_h * h * k + pad[1]! + pad[3]!];
  if (!id.includes("#")) textAt = [-pad[0]!, -pad[1]!];
  return {
    geom,
    k,
    tx: t[0],
    ty: t[1],
    drawn,
    held: Number.isFinite(held) ? held : 1,
    sx: Number.isFinite(sx) ? sx! : 1,
    sy: Number.isFinite(sy) ? sy! : 1,
    rot: Number.isFinite(rot) ? rot : 0,
    bw: box[0]!,
    bh: box[1]!,
    bx: textAt[0] * k,
    by: textAt[1] * k,
    ...(id.includes("#") ? { origin: textOrigin(el) ?? undefined, em: fontPx(el) } : {}),
  };
}

/** An element's first run's font size in its own px, if it reads as one. */
function fontPx(el: Styled): number | undefined {
  const run = el.querySelector("span") ?? el;
  const v = Number.parseFloat(getComputedStyle(run).fontSize);
  return Number.isFinite(v) && v > 0 && v < 10000 ? v : undefined;
}

/**
 * How far a shape's drawing reaches past its box, px (left, top, right,
 * bottom): half its widest outline, its shadow's blur (2σ) moved by its
 * offset. PowerPoint lays a filter over all of it: in its film of the
 * `entrances` deck a shadowed 318 px shape's six blinds are 55 px apart
 * and its wipe crosses 401 px of a 388 px box, the shadow's reach.
 */
function inkPad(el: HTMLElement): [number, number, number, number] {
  let line = 0;
  for (const n of el.querySelectorAll(":scope > svg [stroke-width]")) {
    const stroke = n.getAttribute("stroke");
    if (stroke !== null && stroke !== "none") line = Math.max(line, Number.parseFloat(n.getAttribute("stroke-width") ?? "") || 0);
  }
  const pad: [number, number, number, number] = [line / 2, line / 2, line / 2, line / 2];
  for (const d of el.querySelectorAll(":scope > svg feDropShadow")) {
    const num = (a: string) => Number.parseFloat(d.getAttribute(a) ?? "") || 0;
    const [dx, dy, b] = [num("dx"), num("dy"), 2 * num("stdDeviation")];
    if (![dx, dy, b].every(Number.isFinite) || b > 1000) continue;
    pad[0] = Math.max(pad[0], b - dx);
    pad[1] = Math.max(pad[1], b - dy);
    pad[2] = Math.max(pad[2], b + dx);
    pad[3] = Math.max(pad[3], b + dy);
  }
  // A shadow the renderer draws as a blurred, scaled copy of the outline
  // (PowerPoint's scaled shadows): its box is the shape's scaled about the
  // anchor, moved by the group's translate, and reaches the shadow's blur
  // radius past it, which the renderer draws as 3σ. PowerPoint's film of a
  // 92 % shadow (blur 25 pt, 10 pt at 135°, aligned top right) on a 317 px
  // tall shape lays its blinds 65 px apart: over 390 px, the scaled box
  // plus the radius (49 slide px past the box), not 2σ (27).
  const [W0, H0] = [parseFloat(el.style.width), parseFloat(el.style.height)];
  for (const g of el.querySelectorAll(':scope > svg g[data-pptx-outer-shadow="scaled-silhouette"]')) {
    if (!Number.isFinite(W0) || !Number.isFinite(H0)) break;
    const num = (a: string) => Number.parseFloat(g.getAttribute(a) ?? "");
    const [sx, sy, ax, ay] = [num("data-pptx-shadow-scale-x"), num("data-pptx-shadow-scale-y"), num("data-pptx-shadow-anchor-x"), num("data-pptx-shadow-anchor-y")];
    const m = /translate\(\s*(-?[\d.e]+)[\s,]+(-?[\d.e]+)\s*\)/.exec(g.getAttribute("transform") ?? "");
    const filter = /url\(#([^)]+)\)/.exec(g.getAttribute("filter") ?? "")?.[1];
    const blur = filter === undefined ? null : el.querySelector(`:scope > svg filter[id="${CSS.escape(filter)}"] feGaussianBlur`);
    const b = 3 * (Number.parseFloat(blur?.getAttribute("stdDeviation") ?? "") || 0);
    const [dx, dy] = m === null ? [0, 0] : [Number(m[1]), Number(m[2])];
    if (![sx, sy, ax, ay, dx, dy, b].every(Number.isFinite) || b > 1000) continue;
    const xs = [ax + (0 - ax) * sx + dx, ax + (W0 - ax) * sx + dx];
    const ys = [ay + (0 - ay) * sy + dy, ay + (H0 - ay) * sy + dy];
    pad[0] = Math.max(pad[0], b - Math.min(...xs));
    pad[1] = Math.max(pad[1], b - Math.min(...ys));
    pad[2] = Math.max(pad[2], Math.max(...xs) + b - W0);
    pad[3] = Math.max(pad[3], Math.max(...ys) + b - H0);
  }
  return pad.map((v) => Math.min(v, 200)) as [number, number, number, number];
}

const camel = (p: string) => p.replace(/-([a-z])/g, (_, c: string) => c.toUpperCase());
const COLOR_PROPS = new Set(["fill", "stroke", "stroke-width", "stop-color", "color", "filter"]);

/** When a shape's colour changes run in the step (ms from its start) and
 *  how far along they are at a time: the colour behaviour's own progress
 *  on its effect's clock (accel, decel, repeat, autoRev), as every other
 *  channel plays. */
export interface ColorWindow {
  from: number;
  to: number;
  progress(t: number): number;
}

export function colorWindows(effects: readonly Effect[]): Map<string, ColorWindow> {
  const out = new Map<string, ColorWindow>();
  for (const e of effects) {
    for (const b of e.behaviours) {
      if (b.target === null || (b.kind !== "color" && !(b.kind === "set" && b.attrs.some((a) => /^(fill|stroke)\./.test(a))))) continue;
      const begin = e.start + b.delay;
      const end = e.start + (e.dur ?? 0);
      const progress = (t: number) => {
        const c = effectTime(e, t - e.start);
        const q = c === null ? null : behaviourProgress(b, c);
        return q === null ? 0 : Math.min(1, Math.max(0, q));
      };
      const was = out.get(b.target.spid);
      // The colour behaviour (not a fill.type set) that ends last paces the blend.
      const paces = b.kind === "color" && (was === undefined || end >= was.to);
      out.set(b.target.spid, {
        from: was === undefined ? begin : Math.min(was.from, begin),
        to: was === undefined ? end : Math.max(was.to, end),
        progress: paces || was === undefined ? progress : was.progress,
      });
    }
  }
  return out;
}

/** Samples of a colour window's progress as a CSS `linear()` easing over
 *  the window (null when it is linear already, or empty). */
export function colorEasing(w: ColorWindow, samples = 32): string | null {
  const span = w.to - w.from;
  if (!(span > 0)) return null;
  const pts: number[] = [];
  for (let i = 0; i <= samples; i++) pts.push(Math.round(w.progress(w.from + (span * i) / samples) * 1e4) / 1e4);
  if (pts.every((v, i) => Math.abs(v - i / samples) < 1e-4)) return null;
  return `linear(${pts.join(", ")})`;
}

/** Whether this engine takes a `linear()` easing (Safari 17.2, Chrome 113). */
function linearEasing(): boolean {
  return typeof CSS !== "undefined" && typeof CSS.supports === "function" && CSS.supports("transition-timing-function", "linear(0, 0.5, 1)");
}

/** Pictures a pattern steps through over a step of `ms`: one per ~40 ms
 *  (12–50), so a cell's soft edge moves in small steps. */
export function maskLevels(ms: number): number {
  return Math.min(50, Math.max(12, Math.round(ms / 40)));
}

/** A dissolve cell, a random bar and a strips square, in the slide's px
 *  (CSS px: 96 to the inch, 4/3 to the point). PowerPoint's film (1920 px
 *  to a 13.33 in slide): dissolve's grain 2 px (1 pt), random bars 1.5 px
 *  on average (3/4 pt), strips' squares 20 px (10 pt). */
const DISSOLVE_CELL = 4 / 3;
const BAR = 1;
const STRIP = 40 / 3;
/** A dissolve repeats a tile this many cells square, each cell 2 picture
 *  px (crisp at a 1.5× fit: the film's 2 px): grain at any size of shape,
 *  bounded memory. */
const DISSOLVE_TILE = 192;
/** Picture px per slide px for checkerboard cells and strips' squares
 *  (soft-edged: a picture scaled up stays true); past `MASK_MAX` px a side
 *  fewer, for bounded memory and a few ms a picture. */
const MASK_SCALE = 1;
const MASK_MAX = 768;
/** Most bars a random-bars picture draws, most strips squares a side, most
 *  picture px a checkerboard cell is drawn with: past them bars, squares
 *  and cells are drawn wider, so a picture's size is bounded whatever the
 *  shape's (nothing bounds `a:ext`). */
const MAX_BARS = 384;
const MAX_SQUARES = 96;
const CHECKER_PX = 64;
/** Dissolve and random bars are seeded by one of this many variants of the
 *  element: neighbours still differ, and a click that dissolves hundreds of
 *  shapes draws a handful of picture sets, not hundreds. */
const SEEDS = 8;
/** Distinct picture sets one click may use; past it the click lands. */
export const MASK_SETS_PER_CLICK = 24;
/** Sets kept between clicks. A set a playing click holds is never evicted
 *  (its `blob:` URLs are in its keyframes: a revoked one hides the shape). */
const MASK_CACHE_MAX = 32;
const maskCache = new Map<string, MaskSet & { holds: number }>();

/** Hold a playing click's picture sets until `releaseMasks` (exported for tests). */
export function retainMasks(sets: readonly MaskSet[]): void {
  for (const s of sets) (s as MaskSet & { holds: number }).holds++;
}
export function releaseMasks(sets: readonly MaskSet[]): void {
  for (const s of sets) (s as MaskSet & { holds: number }).holds--;
  trimMasks(MASK_CACHE_MAX);
}
const revoke = (s: MaskSet) => {
  for (const u of s.urls) {
    const m = /^url\("(blob:.*)"\)$/.exec(u);
    if (m !== null) URL.revokeObjectURL(m[1]!);
  }
};
/** Evict the oldest sets no click holds until at most `max` remain. */
function trimMasks(max: number): void {
  for (const [key, s] of maskCache) {
    if (maskCache.size <= max) return;
    if (s.holds > 0) continue;
    maskCache.delete(key);
    revoke(s);
  }
}
/** Revoke every picture set no click holds: leaving the show, a deck
 *  re-read, the view's unmount. */
export function clearMasks(): void {
  trimMasks(0);
}
/** How many picture sets are cached and how many a click holds (tests). */
export function maskCacheStats(): { size: number; held: number } {
  let held = 0;
  for (const s of maskCache.values()) if (s.holds > 0) held++;
  return { size: maskCache.size, held };
}

/** The cache key of a pattern's pictures (`maskImages`). */
function maskKey(pattern: Pattern, W: number, H: number, margin: number, seedKey: string, levels: number): string {
  const { kind, out } = pattern;
  return `${kind}|${pattern.horizontal}|${pattern.corner ?? ""}|${out}|${W}x${H}|${margin}|${levels}|${kind === "dissolve" || kind === "bars" ? seedOf(seedKey) % SEEDS : ""}`;
}
function seedOf(key: string): number {
  let seed = 2166136261;
  for (let i = 0; i < key.length; i++) seed = Math.imul(seed ^ key.charCodeAt(i), 16777619);
  return seed >>> 0;
}

/** A seeded PRNG (mulberry32): the same element dissolves the same way. */
function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A data URL as a short `blob:` URL: a keyframe list of many masks parses
 *  in a fraction of the time (WKWebView: `animate()` took ~50 ms with the
 *  data URLs inline). Falls back to the data URL. */
function blobUrl(data: string): string {
  if (typeof URL.createObjectURL !== "function" || typeof atob !== "function") return data;
  const bin = atob(data.slice(data.indexOf(",") + 1));
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return URL.createObjectURL(new Blob([bytes], { type: "image/png" }));
}

/** A pattern's pictures: one per level from progress 0 to 1, each shown
 *  `size` px (element px) at `offset` from the masked box's corner,
 *  tiled when `repeat`. */
export interface MaskSet {
  urls: string[];
  size: [number, number];
  offset: [number, number];
  repeat: boolean;
}

/**
 * The pictures a pattern filter shows its element through on a `W`×`H`
 * box (the shape's box pushed out by `margin` on each side), `levels + 1`
 * of them. Dissolve and random bars: each cell (bar) opaque once the
 * share shown (`patternShare`) passes its random rank, seeded by
 * `seedKey`. Checkerboard (one tile of two rows, repeated) and strips:
 * each cell's soft edge (`revealAt`). Null where there is no canvas.
 */
export function maskImages(pattern: Pattern, W: number, H: number, margin: number, seedKey: string, levels: number): MaskSet | null {
  if (![W, H, margin].every(Number.isFinite) || W <= 0 || H <= 0 || margin < 0) return null;
  levels = Math.min(50, Math.max(1, Math.round(levels) || 1));
  const [w, h] = [Math.max(1, W - 2 * margin), Math.max(1, H - 2 * margin)];
  const { kind, out } = pattern;
  const cacheKey = maskKey(pattern, W, H, margin, seedKey, levels);
  const hit = maskCache.get(cacheKey);
  if (hit !== undefined) {
    // Most recently used last: eviction takes the oldest.
    maskCache.delete(cacheKey);
    maskCache.set(cacheKey, hit);
    return hit;
  }
  if (typeof document === "undefined") return null;
  let set: Omit<MaskSet, "urls">;
  let cw: number;
  let ch: number;
  // Each picture px's cell, and each cell's alpha (0–255) at a progress.
  let cellOf: Uint32Array;
  let alphaOf: (cell: number, p: number) => number;
  const seed = (seedOf(seedKey) % SEEDS) + 1;
  const random = (n: number) => {
    const rand = rng(seed);
    return Float32Array.from({ length: n }, () => rand());
  };
  if (kind === "dissolve") {
    cw = ch = 2 * DISSOLVE_TILE;
    cellOf = Uint32Array.from({ length: cw * ch }, (_, i) => Math.floor(i / cw / 2) * DISSOLVE_TILE + Math.floor((i % cw) / 2));
    const rank = random(DISSOLVE_TILE * DISSOLVE_TILE);
    alphaOf = (c, p) => (rank[c]! < (out ? 1 - patternShare(kind, p) : patternShare(kind, p)) ? 255 : 0);
    set = { size: [DISSOLVE_TILE * DISSOLVE_CELL, DISSOLVE_TILE * DISSOLVE_CELL], offset: [0, 0], repeat: true };
  } else if (kind === "bars") {
    // Two picture px per bar, one px across: drawn stretched over the box.
    const n = Math.max(1, Math.min(MAX_BARS, Math.ceil((pattern.horizontal ? H : W) / BAR)));
    [cw, ch] = pattern.horizontal ? [1, 2 * n] : [2 * n, 1];
    cellOf = Uint32Array.from({ length: cw * ch }, (_, i) => Math.floor(i / 2));
    const rank = random(n);
    alphaOf = (c, p) => (rank[c]! < (out ? 1 - patternShare(kind, p) : patternShare(kind, p)) ? 255 : 0);
    set = { size: pattern.horizontal ? [W, n * BAR] : [n * BAR, H], offset: [0, 0], repeat: false };
  } else if (kind === "checker") {
    // One tile: a cell wide, two rows high (across), the second row half a
    // cell on; each cell fills from its leading edge. Cells are laid out
    // from the box's centre (PowerPoint's film of the `filters` deck).
    const across = pattern.horizontal;
    const c = (across ? h : w) / CHECKER_ROWS;
    const U = across ? w : h;
    const n = Math.max(2, Math.round(Math.min(2 * c, CHECKER_PX)));
    [cw, ch] = across ? [n, 2 * n] : [2 * n, n];
    // A cell here is a column of the tile's px: its level is its place in the cell.
    cellOf = new Uint32Array(cw * ch);
    for (let y = 0; y < ch; y++) {
      for (let x = 0; x < cw; x++) {
        const [u, v] = across ? [x, y] : [y, x];
        const row = v < n ? 0 : 1;
        cellOf[y * cw + x] = (u - row * (n / 2) + n) % n;
      }
    }
    alphaOf = (u, p) => Math.round(255 * revealAt((u + 0.5) / n, p, out));
    const phase = ((U / 2) % c) - c + margin;
    set = { size: across ? [c, 2 * c] : [2 * c, c], offset: across ? [phase, margin] : [margin, phase], repeat: true };
  } else {
    // Squares from the corner, a diagonal at a time; the margin takes the
    // value of the square at the box's edge.
    const k = Math.min(MASK_SCALE, MASK_MAX / Math.max(W, H));
    [cw, ch] = [Math.max(1, Math.ceil(W * k)), Math.max(1, Math.ceil(H * k))];
    const side = Math.max(STRIP, Math.max(w, h) / MAX_SQUARES);
    const [nx, ny] = [Math.ceil(w / side), Math.ceil(h / side)];
    const [fx, fy] = pattern.corner ?? [0, 0];
    cellOf = new Uint32Array(cw * ch);
    const index = (v: number, len: number, n: number, from: number) => {
      const d = Math.min(len - 1e-6, Math.max(0, v / k - margin));
      return Math.min(n - 1, Math.floor((from === 1 ? len - d : d) / side));
    };
    for (let y = 0; y < ch; y++) {
      const j = index(y + 0.5, h, ny, fy);
      for (let x = 0; x < cw; x++) cellOf[y * cw + x] = index(x + 0.5, w, nx, fx) + j;
    }
    const diagonals = nx + ny - 1;
    alphaOf = (d, p) => Math.round(255 * revealAt(d / diagonals, p, out));
    set = { size: [W, H], offset: [0, 0], repeat: false };
  }
  const canvas = document.createElement("canvas");
  canvas.width = cw;
  canvas.height = ch;
  const ctx = canvas.getContext("2d");
  if (ctx === null) return null;
  const img = ctx.createImageData(cw, ch);
  const cells = cellOf.reduce((m, c) => Math.max(m, c), 0) + 1;
  const alpha = new Uint8ClampedArray(cells);
  const urls: string[] = [];
  for (let l = 0; l <= levels; l++) {
    const p = l / levels;
    for (let c = 0; c < cells; c++) alpha[c] = alphaOf(c, p);
    for (let i = 0; i < cellOf.length; i++) img.data[i * 4 + 3] = alpha[cellOf[i]!]!;
    ctx.putImageData(img, 0, 0);
    urls.push(`url("${blobUrl(canvas.toDataURL("image/png"))}")`);
  }
  const done = { ...set, urls, holds: 0 };
  maskCache.set(cacheKey, done);
  trimMasks(MASK_CACHE_MAX);
  return done;
}

/** A track's keyframes with its pattern's progress as the pictures over
 *  the box (`x y W H` in the element's px, `margin` past the shape's box);
 *  the last frame (landed) has no mask. */
function withMasks(kf: readonly PlayKeyframe[], tr: { box: [number, number, number, number] }, set: MaskSet | null): Keyframe[] {
  const [x, y] = tr.box;
  if (set === null) return kf.map(({ mask: _, ...f }) => f) as Keyframe[];
  const frame = {
    maskSize: `${r3(set.size[0])}px ${r3(set.size[1])}px`,
    maskPosition: `${r3(x + set.offset[0])}px ${r3(y + set.offset[1])}px`,
    maskRepeat: set.repeat ? "repeat" : "no-repeat",
  };
  return kf.map(({ mask, ...f }, i) => {
    if (mask === undefined || i === kf.length - 1) return { ...f, maskImage: "none" };
    return { ...f, maskImage: set.urls[Math.min(set.urls.length - 1, Math.max(0, Math.round(mask * (set.urls.length - 1))))]!, ...frame };
  }) as Keyframe[];
}

/** Whether a track draws through a mask on any frame. */
function masked(kf: readonly object[]): boolean {
  return kf.some((f) => "maskImage" in f && (f as { maskImage?: string }).maskImage !== "none");
}

/**
 * A mask is painted only inside its element's border box: WebKit ignores
 * `mask-clip: no-clip` and Chrome clips a masked element to an ink box that
 * falls short of an SVG shadow, so a shape's shadow (and in WebKit the
 * outer half of its outline) vanished for the length of a wipe, a pattern
 * or an iris and popped in when it landed. PowerPoint's film shows the
 * filter laid over the shadow too. So while a whole shape (or its `!bg`
 * picture) plays through a mask, its border box is grown to the mask's
 * box (`box`: the shape's box pushed out by its ink and the margin) with
 * a transparent border, and a negative margin keeps its content where it
 * was. Engines snap border widths to device px in their own ways (Chrome
 * after `zoom`, WebKit before it), so the margin is corrected by what
 * layout did, measured on the content with the element's transforms off;
 * the correction is in rect units over rect units, never assumed. Returns
 * where the content sits in the grown border box (the masks' shift) and
 * how to put every touched value back; null where it can't (a
 * `transform-origin` of its own, nothing to measure): the mask then
 * clips as before.
 */
function inflate(el: Styled, size: readonly [number, number], box: readonly [number, number, number, number]): { shift: [number, number]; restore(): void } | null {
  const [W0, H0] = size;
  const [x, y, bw, bh] = box;
  const [L, T, R, B] = [-x, -y, x + bw - W0, y + bh - H0].map((v) => Math.max(0, Math.ceil(v)));
  if (![W0, H0, L, T, R, B].every(Number.isFinite) || L + T + R + B === 0) return null;
  const s = el.style;
  const origin = s.getPropertyValue("transform-origin").trim();
  if (origin !== "" && origin !== "center" && origin !== "50% 50%") return null;
  const probe = "ownerSVGElement" in el || el.tagName.toLowerCase() === "svg" ? el.querySelector("path, rect, ellipse, polygon, image, use") : el.firstElementChild;
  if (probe === null) return null;
  const keys = ["border-style", "border-color", "border-top-width", "border-right-width", "border-bottom-width", "border-left-width", "margin-left", "margin-top", "box-sizing", "transform-origin"];
  const moves = ["transform", "translate", "rotate", "scale"];
  let saved: (readonly [string, string, string])[];
  try {
    saved = [...keys, ...moves].map((k) => [k, s.getPropertyValue(k), s.getPropertyPriority(k)] as const);
  } catch {
    return null;
  }
  const put = (only: readonly string[]) => {
    for (const [k, v, pr] of saved) if (only.includes(k)) v === "" ? s.removeProperty(k) : s.setProperty(k, v, pr);
  };
  const restore = () => put([...keys, ...moves]);
  try {
    for (const k of moves) s.setProperty(k, "none");
    const c0 = probe.getBoundingClientRect();
    s.setProperty("box-sizing", "content-box");
    s.setProperty("border-style", "solid");
    s.setProperty("border-color", "transparent");
    s.setProperty("border-width", `${T}px ${R}px ${B}px ${L}px`);
    const at = (dx: number, dy: number) => {
      s.setProperty("margin-left", `${-L - dx}px`);
      s.setProperty("margin-top", `${-T - dy}px`);
      return probe.getBoundingClientRect();
    };
    const c2 = at(8, 8);
    const c1 = at(0, 0);
    const [fx, fy] = [(c1.left - c2.left) / 8, (c1.top - c2.top) / 8];
    if (!(fx > 0 && fy > 0)) {
      restore();
      return null;
    }
    const [cx, cy] = [(c1.left - c0.left) / fx, (c1.top - c0.top) / fy];
    if (!Number.isFinite(cx) || !Number.isFinite(cy) || Math.abs(cx) > 4 || Math.abs(cy) > 4) {
      restore();
      return null;
    }
    at(cx, cy);
    const shift: [number, number] = [L + cx, T + cy];
    s.setProperty("transform-origin", `${r3(shift[0] + W0 / 2)}px ${r3(shift[1] + H0 / 2)}px`);
    put(moves);
    return { shift, restore };
  } catch {
    restore();
    return null;
  }
}

/** Keyframes with each mask moved by `shift` (the grown border box's origin
 *  to the content's). */
function shiftMasks(kf: readonly Keyframe[], [dx, dy]: readonly [number, number]): Keyframe[] {
  return kf.map((f) => {
    const pos = typeof f.maskPosition === "string" ? /^(-?[\d.]+)px (-?[\d.]+)px$/.exec(f.maskPosition) : null;
    return pos === null ? f : { ...f, maskPosition: `${r3(Number(pos[1]) + dx)}px ${r3(Number(pos[2]) + dy)}px` };
  });
}

/** A click planned ahead: everything `playStep` needs but the animations. */
export interface PreparedStep {
  tagged: TaggedSlide;
  timeline: SlideTimeline;
  k: number;
  approximate: boolean;
  plan: StepPlan;
  duration: number;
  windows: Map<string, ColorWindow>;
  blends: { el: Styled; prop: string; a: string; b: string; spid: string }[];
  landedBy: Map<Styled, Map<string, string>>;
  /** Filtered drawings inside an element that grows or shrinks, repainted every frame (`filteredIn`). */
  repaint: { el: SVGElement; opacity: number }[];
}

/**
 * The SVG nodes carrying a filter (a shadow, a glow, a soft edge) inside
 * the elements a step scales. WebKit renders an SVG filter at the scale
 * its element was first painted at and keeps that picture while the
 * compositor scales the element: a zoom from nothing showed its shape as
 * a pale smear the size of the shadow's region, crisp only once it landed
 * (the native app's engine, window captures). A barely-there opacity
 * change on each such node, for the length of the click, repaints it every
 * frame at the scale it stands at; its last frame is its own opacity.
 */
function filteredIn(tagged: TaggedSlide, tracks: StepPlan["tracks"]): { el: SVGElement; opacity: number }[] {
  const out: { el: SVGElement; opacity: number }[] = [];
  for (const tr of tracks) {
    if (!tr.keyframes.some((f) => f.scale !== undefined && f.scale !== "none")) continue;
    const el = elementOf(tagged, tr.id);
    if (el === undefined) continue;
    for (const n of el.querySelectorAll<SVGElement>("[filter]")) {
      const op = Number.parseFloat(n.style.opacity || (n.getAttribute("opacity") ?? ""));
      out.push({ el: n, opacity: Number.isFinite(op) ? Math.max(0, Math.min(1, op)) : 1 });
      if (out.length >= REPAINT_MAX) return out;
    }
  }
  return out;
}
/** At most this many filtered nodes repaint during one click; past it they
 *  keep WebKit's picture (a shape with hundreds of shadows is rare). */
const REPAINT_MAX = 64;

/**
 * Plan step `k` without touching the slide: what `playStep` would animate
 * from the state the slide stands on now (the state before step `k`, or a
 * slide drawn as is for its step 0). Null when the click would land at
 * once (nothing to move, past the budget). A viewer plans the next click
 * while idle and passes it to `playStep`, which then only starts
 * animations; any change to the slide or its place makes it stale.
 */
export function prepareStep(tagged: TaggedSlide, timeline: SlideTimeline, k: number, opts: PlayOptions): PreparedStep | null {
  const states = opts.states ?? buildStates(timeline);
  const after = states[k];
  const effects = timeline.steps[k] ?? [];
  const before = k === 0 ? entryState(timeline) : states[k - 1];
  const palette = opts.palette ?? null;
  if (after === undefined || before === undefined || effects.length === 0) return null;
    const target = statePaints(tagged, after, opts.w, opts.h, palette);
    const from = statePaints(tagged, before, opts.w, opts.h, palette);
    const info = new Map<string, ShapeInfo>();
    const elements: ElementId[] = [];
    const split = splitShapes(timeline);
    for (const [id, s] of tagged.shapes) {
      info.set(id, { ...shapeInfo(s), split: split.has(id) });
      elements.push(id, ...(s.paras ?? []).map((_, i) => `${id}#${i}`));
    }
    // An element's landed values: what the state after puts inline, else none.
    const landedBy = new Map<Styled, Map<string, string>>();
    for (const p of target.paints) {
      const m = landedBy.get(p.el) ?? new Map<string, string>();
      m.set(p.prop, p.value);
      landedBy.set(p.el, m);
    }
    const landed = (id: ElementId): Landed | null => {
      const el = elementOf(tagged, id);
      if (el === undefined) return null;
      const m = landedBy.get(el);
      const op = m?.get("opacity");
      return {
        opacity: op === undefined ? drawnOpacity(el) : Number(op),
        translate: m?.get("translate") ?? "none",
        scale: m?.get("scale") ?? "none",
        rotate: m?.get("rotate") ?? "none",
      };
    };
    const plan = planStep(effects, before, info, elements, (id) => measure(tagged, id, opts.w, opts.h), landed, opts.w, opts.h, opts.maxMs);
    if (plan.overBudget) return null;
    // Past a bound on the pictures one click draws, it lands.
    const keys = new Set<string>();
    for (const tr of plan.tracks) if (tr.pattern !== undefined && tr.box !== undefined) keys.add(maskKey(tr.pattern, tr.box[2], tr.box[3], tr.margin ?? 0, tr.id, tr.levels ?? maskLevels(plan.duration)));
    if (keys.size > MASK_SETS_PER_CLICK) return null;
    // A pattern's mask images are drawn now, while the show is idle, not
    // when the click starts them (they are cached by size and variant).
    for (const tr of plan.tracks) if (tr.pattern !== undefined && tr.box !== undefined) maskImages(tr.pattern, tr.box[2], tr.box[3], tr.margin ?? 0, tr.id, tr.levels ?? maskLevels(plan.duration));
    let duration = plan.duration;
    // Colours blend from the paint before to the paint after, over the
    // window their effects run in.
    const windows = colorWindows(effects);
    const fromBy = new Map<Styled, Map<string, string>>();
    for (const p of from.paints) {
      const m = fromBy.get(p.el) ?? new Map<string, string>();
      m.set(p.prop, p.value);
      fromBy.set(p.el, m);
    }
    const blends: { el: Styled; prop: string; a: string; b: string; spid: string }[] = [];
    const drawnValue = (el: Styled, prop: string) => original(el, prop) || el.getAttribute(prop) || (prop === "filter" ? "none" : "");
    for (const [el, m] of landedBy) {
      for (const [prop, b] of m) {
        if (!COLOR_PROPS.has(prop)) continue;
        const a = fromBy.get(el)?.get(prop) ?? drawnValue(el, prop);
        if (a === b) continue;
        const spid = (el.closest("[data-sid]") as HTMLElement | null)?.dataset.sid ?? "";
        const grad = prop === "fill" && "ownerSVGElement" in el ? gradientOf(el as SVGElement, a) : null;
        // As PowerPoint's film shows: a gradient going solid is solid at
        // the click, in its first stop's colour, and blends from there; a
        // picture going solid vanishes at the click and the colour blends
        // in from white.
        if (grad !== null) blends.push({ el, prop, a: toCss(parseCss(firstStop(grad)) ?? { r: 0, g: 0, b: 0, a: 1 }), b, spid });
        else if (a === "none" && prop === "fill") blends.push({ el, prop, a: "rgb(255, 255, 255)", b, spid });
        else blends.push({ el, prop, a, b, spid });
      }
    }
    for (const w of windows.values()) duration = Math.max(duration, Math.min(w.to, opts.maxMs ?? PLAY_MAX_MS));
    if (!(duration > 0)) return null;
    return { tagged, timeline, k, approximate: target.approximate, plan, duration, windows, blends, landedBy, repaint: filteredIn(tagged, plan.tracks) };
}

/**
 * Play step `k` of `timeline` on a tagged slide (step 0 is what runs on
 * entry): from the state before it to `states[k]`. The returned `finish`
 * lands on that state at once; so does the end of the longest effect, the
 * `maxMs` cap, `reduced`, a step past `PLAY_BUDGET`, a step with nothing
 * to move, and any error while planning.
 */
export function playStep(tagged: TaggedSlide, timeline: SlideTimeline, k: number, opts: PlayOptions): Playing {
  const states = opts.states ?? buildStates(timeline);
  const after = states[k];
  const effects = timeline.steps[k] ?? [];
  const before = k === 0 ? entryState(timeline) : states[k - 1];
  const palette = opts.palette ?? null;
  let resolve!: () => void;
  const finished = new Promise<void>((r) => (resolve = r));
  const playing: Playing = { finished, finish: () => {}, approximate: false };
  const anims: Animation[] = [];
  const held: MaskSet[] = [];
  const inflated: { restore(): void }[] = [];
  let done = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const land = () => {
    if (done) return;
    done = true;
    if (timer !== undefined) clearTimeout(timer);
    // The state goes on first, in the same task: no frame shows neither.
    // Whatever it throws, the animations end and `finished` resolves.
    try {
      if (after !== undefined) playing.approximate = applyState(tagged, after, opts.w, opts.h, palette);
    } finally {
      for (const g of inflated.splice(0)) g.restore();
      for (const a of anims) a.cancel();
      releaseMasks(held.splice(0));
      resolve();
    }
  };
  playing.finish = land;
  if (after === undefined || before === undefined || opts.reduced === true || effects.length === 0) {
    land();
    return playing;
  }
  try {
    applyState(tagged, before, opts.w, opts.h, palette);
    const p = opts.prepared;
    const prep = p != null && p.tagged === tagged && p.k === k && p.timeline === timeline ? p : prepareStep(tagged, timeline, k, opts);
    if (prep === null) {
      land();
      return playing;
    }
    playing.approximate = prep.approximate;
    const { plan, duration, windows, blends, repaint } = prep;
    for (const tr of plan.tracks) {
      const el = elementOf(tagged, tr.id);
      if (el === undefined || typeof el.animate !== "function") continue;
      // Tracks were planned for the step's motion; stretch to a longer colour window.
      const scale = plan.duration / duration;
      const planned = scale === 1 ? tr.keyframes : [...tr.keyframes.map((f) => ({ ...f, offset: f.offset * scale })), { ...tr.keyframes[tr.keyframes.length - 1]!, offset: 1 }];
      let kf: Keyframe[] | PlayKeyframe[] = planned;
      if (tr.pattern !== undefined && tr.box !== undefined) {
        const levels = tr.levels ?? maskLevels(plan.duration);
        const set = maskImages(tr.pattern, tr.box[2], tr.box[3], tr.margin ?? 0, tr.id, levels);
        if (set !== null) {
          retainMasks([set]);
          held.push(set);
        }
        kf = withMasks(planned, { box: tr.box }, set);
      }
      let frames = kf as Keyframe[];
      if (masked(kf)) {
        const spid = tr.id.split(/[#!]/)[0]!;
        const shape = tagged.shapes.get(spid);
        const size = shape === undefined ? null : ([parseFloat(shape.el.style.width), parseFloat(shape.el.style.height)] as const);
        const grown = !tr.id.includes("#") && tr.box !== undefined && size !== null ? inflate(el, size, tr.box) : null;
        if (grown !== null) {
          inflated.push(grown);
          frames = shiftMasks(frames, grown.shift);
        } else frames = frames.map((f) => ({ ...f, maskClip: "no-clip" }));
      }
      anims.push(el.animate(frames, { duration, fill: "both", easing: "linear" }));
    }
    for (const bl of blends) {
      if (typeof bl.el.animate !== "function") continue;
      const win = windows.get(bl.spid);
      const [t0, t1] = win === undefined ? [0, duration] : [win.from, win.to];
      const p = camel(bl.prop);
      const at = (t: number) => Math.min(1, Math.max(0, t / duration));
      // The blend runs on the colour's own clock, eased like every other
      // channel; an engine without `linear()` blends evenly.
      const ease = win !== undefined && linearEasing() ? colorEasing(win) : null;
      anims.push(
        bl.el.animate(
          [
            { offset: 0, [p]: bl.a },
            { offset: at(t0), [p]: bl.a, ...(ease === null ? {} : { easing: ease }) },
            { offset: Math.max(at(t0), at(t1)), [p]: bl.b },
            { offset: 1, [p]: bl.b },
          ] as Keyframe[],
          { duration, fill: "both", easing: "linear" },
        ),
      );
    }
    if (anims.length === 0) {
      land();
      return playing;
    }
    for (const r of repaint) {
      if (typeof r.el.animate !== "function") continue;
      const o = r.opacity;
      anims.push(r.el.animate([{ opacity: o }, { opacity: o * 0.9999 }, { opacity: o }], { duration, easing: "linear" }));
    }
    timer = setTimeout(land, duration);
  } catch {
    // A slide this can't plan never stays half-played: it lands.
    land();
  }
  return playing;
}
