/**
 * A slide's build state put on its drawn shapes: what is on screen after
 * click k (`pptxTiming.ts`'s `BuildState`), with no motion. Hidden is
 * inline `opacity: 0` plus `inert` (never `visibility`/`display`: they
 * inherit and re-resolve the subtree, and `display` would reflow the text
 * around a hidden paragraph, which PowerPoint keeps in place). A path's
 * offset is the individual `translate` property, which composes with the
 * renderer's own `transform` (rotation, flips).
 *
 * Keys combine as the timing contract's R5 says: a shape is drawn unless
 * its key is hidden; a paragraph unless its range or its shape is hidden.
 * The renderer draws no separate element for a text box's background
 * (`:bg`), so a hidden `:bg` hides the shape only when every paragraph is
 * hidden too; otherwise the box stays drawn and the slide is approximate.
 *
 * `applyState` is idempotent and `clearState` leaves the elements exactly
 * as the renderer drew them (the original inline values are kept aside).
 *
 * `playStep` plays a click as motion between two of those states: it puts
 * the state before the click, runs one Web Animation per element it moves
 * (`fill: "both"`, sampled from `pptxMotion.ts`: two behaviours on one
 * CSS property, as a zoom's width and height on `scale`, can't be two
 * animations without `composite`), and at the end — or on `finish()` —
 * cancels them and puts the state after the click. Animations write no
 * inline style, so the build states stay the only source of truth for
 * where a click ends.
 */

import { effectMotion, type Geom, type Segment } from "./pptxMotion";
import { buildStates, targetKey, type BuildState, type Effect, type SlideTimeline } from "./pptxTiming";
import type { TaggedSlide } from "./pptxTag";

/** What the drawn shapes need, by shape id; pure so it can be tested. */
export interface StatePlan {
  hideShapes: Set<string>;
  /** Shape id → paragraph indexes to hide (only shapes with matched paragraphs). */
  hideParas: Map<string, Set<number>>;
  /** Shape id → offset in slide fractions. */
  moveShapes: Map<string, [number, number]>;
  /** Shape id → paragraph index → offset in slide fractions. */
  moveParas: Map<string, Map<number, [number, number]>>;
  /** Something in the state couldn't be shown as PowerPoint would. */
  approximate: boolean;
}

/** What the planner needs to know of a drawn shape. */
export interface ShapeInfo {
  paraCount: number;
  /** Its paragraphs were matched one to one. */
  paras: boolean;
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

export function planState(shapes: ReadonlyMap<string, ShapeInfo>, state: BuildState): StatePlan {
  const plan: StatePlan = {
    hideShapes: new Set(),
    hideParas: new Map(),
    moveShapes: new Map(),
    moveParas: new Map(),
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

interface Original {
  hadStyle: boolean;
  opacity: string;
  translate: string;
  inert: boolean;
}

/** Inline values an element had before a state touched it. */
const originals = new WeakMap<HTMLElement, Original>();

function keep(el: HTMLElement): void {
  if (originals.has(el)) return;
  originals.set(el, {
    hadStyle: el.hasAttribute("style"),
    opacity: el.style.opacity,
    translate: el.style.translate,
    inert: el.hasAttribute("inert"),
  });
}

function restore(el: HTMLElement): void {
  const o = originals.get(el);
  if (o === undefined) return;
  originals.delete(el);
  el.style.opacity = o.opacity;
  el.style.translate = o.translate;
  if (!o.inert) el.removeAttribute("inert");
  if (!o.hadStyle && el.style.cssText === "") el.removeAttribute("style");
}

/** `px`: the offset in the element's own px. */
function put(el: HTMLElement, hidden: boolean, px: [number, number] | undefined): void {
  const moved = px !== undefined && (px[0] !== 0 || px[1] !== 0);
  if (!hidden && !moved) {
    restore(el);
    return;
  }
  keep(el);
  const o = originals.get(el)!;
  el.style.opacity = hidden ? "0" : o.opacity;
  if (hidden) el.setAttribute("inert", "");
  else if (!o.inert) el.removeAttribute("inert");
  el.style.translate = moved ? `${px[0]}px ${px[1]}px` : o.translate;
}

/**
 * Show `state` on a tagged slide drawn `w`×`h` slide px. True when some of
 * it could only be approximated (a target with no tagged element, a
 * paragraph build on unmatched paragraphs, a `:bg` drawn with its text).
 */
export function applyState(tagged: TaggedSlide, state: BuildState, w: number, h: number): boolean {
  const info = new Map<string, ShapeInfo>();
  for (const [id, s] of tagged.shapes) info.set(id, { paraCount: s.paraCount, paras: s.paras !== null });
  const plan = planState(info, state);
  let approximate = plan.approximate;
  for (const [id, s] of tagged.shapes) {
    const off = plan.moveShapes.get(id);
    // The shape's own `translate` acts in the slide's px, before its transform.
    put(s.el, plan.hideShapes.has(id), off === undefined ? undefined : [off[0] * w, off[1] * h]);
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
      put(p, hide?.has(i) === true, px);
    });
  }
  return approximate;
}

/** The slide as drawn: every shape and paragraph back as the renderer left it. */
export function clearState(tagged: TaggedSlide): void {
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
/** Keyframes an element gets, at most, for a whole step. */
const MAX_FRAMES = 600;

/** An element the step moves: `"4"` a shape, `"4#1"` its paragraph 1. */
export type ElementId = string;

/** What playback needs to know of a drawn element before the step. */
export interface ElementBase {
  /** Its place in slide fractions (centre and size). */
  geom: Geom;
  /** Its own px per slide px (a paragraph inside a scaled text body). */
  k: number;
  /** Its `translate` before the step, own px. */
  tx: number;
  ty: number;
  /** Its opacity when shown. */
  shown: number;
  /** Its own box, px (what `clip-path` measures). */
  bw: number;
  bh: number;
}

export interface PlayKeyframe {
  offset: number;
  opacity?: number;
  translate?: string;
  scale?: string;
  rotate?: string;
  clipPath?: string;
}

export interface StepPlan {
  /** ms; 0 when nothing moves (the step lands at once). */
  duration: number;
  tracks: { id: ElementId; keyframes: PlayKeyframe[] }[];
  /** Why some of it plays other than as written. */
  notes: string[];
}

/** The elements a target key acts on. */
function elementsOf(key: string, shapes: ReadonlyMap<string, ShapeInfo>): ElementId[] {
  const m = KEY.exec(key);
  if (m === null) return [];
  const info = shapes.get(m[1]!);
  if (info === undefined) return [];
  if (m[3] === undefined || !info.paras) return [m[1]!];
  const out: ElementId[] = [];
  for (let i = Number(m[3]); i <= Math.min(Number(m[4]), info.paraCount - 1); i++) out.push(`${m[1]}#${i}`);
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
  return { ppt_x: (x0 + x1) / 2, ppt_y: (y0 + y1) / 2, ppt_w: x1 - x0, ppt_h: y1 - y0 };
}

const r3 = (n: number) => Math.round(n * 1000) / 1000;

/**
 * The keyframes step `effects` plays from state `before`: one track per
 * element it moves, on one clock from the step's start (`duration`, capped
 * at `maxMs`). Pure: `base` measures the drawn elements, `w`×`h` is the
 * slide in px. Visibility follows the step's sets at their begin times,
 * through `planState` (the same keys-to-elements rule `applyState` uses).
 */
export function planStep(
  effects: readonly Effect[],
  before: BuildState,
  shapes: ReadonlyMap<string, ShapeInfo>,
  base: (id: ElementId) => ElementBase | null,
  w: number,
  h: number,
  maxMs = PLAY_MAX_MS,
): StepPlan {
  const bases = new Map<ElementId, ElementBase | null>();
  const baseOf = (id: ElementId) => {
    if (!bases.has(id)) bases.set(id, base(id));
    return bases.get(id) ?? null;
  };
  const geom = (key: string): Geom | null =>
    union(elementsOf(key, shapes).map(baseOf).filter((b): b is ElementBase => b !== null).map((b) => b.geom));

  const motions = effects.map((e) => effectMotion(e, geom));
  const notes = [...new Set(motions.map((m) => m.note).filter((n): n is string => n !== null))];
  let duration = 0;
  for (const e of effects) if (e.dur !== null) duration = Math.max(duration, e.start + e.dur);
  duration = Math.min(duration, maxMs);
  if (!(duration > 0)) return { duration: 0, tracks: [], notes };

  // Visibility over the step: the sets in begin order (ties: document order).
  const events: { at: number; key: string; to: "visible" | "hidden" }[] = [];
  for (const e of effects) {
    for (const b of e.behaviours) {
      if (b.kind !== "set" || !b.attrs.includes("style.visibility") || b.target === null) continue;
      if (b.to !== "visible" && b.to !== "hidden") continue;
      events.push({ at: e.start + b.delay, key: targetKey(b.target), to: b.to });
    }
  }
  events.sort((a, b) => a.at - b.at);
  const hidden = new Set(before.hidden);
  const snapshots: { at: number; plan: StatePlan }[] = [{ at: -Infinity, plan: planState(shapes, { hidden: [...hidden], moved: {} }) }];
  for (let i = 0; i < events.length; ) {
    const at = events[i]!.at;
    for (; i < events.length && events[i]!.at === at; i++) {
      const ev = events[i]!;
      if (ev.to === "hidden") hidden.add(ev.key);
      else hidden.delete(ev.key);
    }
    snapshots.push({ at, plan: planState(shapes, { hidden: [...hidden], moved: {} }) });
  }
  const hiddenAt = (id: ElementId, t: number, pre: boolean): boolean => {
    let plan = snapshots[0]!.plan;
    for (const s of snapshots) if (pre ? s.at < t : s.at <= t) plan = s.plan;
    const [spid, para] = id.split("#");
    return para === undefined ? plan.hideShapes.has(spid!) : plan.hideParas.get(spid!)?.has(Number(para)) === true;
  };

  // Each element's segments, and the elements whose visibility changes.
  const per = new Map<ElementId, Segment[]>();
  for (const m of motions) {
    for (const seg of m.segments) {
      for (const id of elementsOf(seg.key, shapes)) {
        if (baseOf(id) === null) continue;
        const list = per.get(id) ?? [];
        list.push(seg);
        per.set(id, list);
      }
    }
  }
  const visIds = new Set<ElementId>();
  for (const ev of events) for (const id of elementsOf(ev.key, shapes)) if (baseOf(id) !== null) visIds.add(id);
  for (const id of visIds) if (!per.has(id)) per.set(id, []);

  // When to sample: the step's ends, densely through each moving span,
  // and both sides of each visibility change.
  const spans = motions.filter((m) => m.segments.length > 0).map((m) => [m.from, Math.min(m.to, duration)] as const);
  const times: { t: number; pre: boolean }[] = [
    { t: 0, pre: false },
    { t: duration, pre: false },
  ];
  const grid = Math.max(1000 / FRAMES_PER_S, spans.reduce((n, [a, b]) => n + Math.max(0, b - a), 0) / MAX_FRAMES);
  for (const [a, b] of spans) {
    for (let t = a; t < b; t += grid) times.push({ t, pre: false });
    times.push({ t: b, pre: false });
  }
  for (const ev of events) if (ev.at > 0 && ev.at <= duration) times.push({ t: ev.at, pre: true }, { t: ev.at, pre: false });
  times.sort((a, b) => a.t - b.t || (a.pre === b.pre ? 0 : a.pre ? -1 : 1));
  const samples = times.filter((s, i) => i === 0 || s.t !== times[i - 1]!.t || s.pre !== times[i - 1]!.pre);

  const tracks: StepPlan["tracks"] = [];
  for (const [id, segs] of per) {
    const b = baseOf(id)!;
    const has = (...chs: string[]) => segs.some((s) => chs.includes(s.ch));
    const moves = has("x", "y", "mx", "my");
    const scales = has("w", "h", "sx", "sy");
    const turns = has("rot", "rotAbs");
    const clips = has("clip");
    const fades = has("op", "opAbs") || visIds.has(id);
    if (!moves && !scales && !turns && !clips && !fades) continue;
    const g = b.geom;
    const keyframes: PlayKeyframe[] = samples.map(({ t, pre }) => {
      const at = pre ? t - 0.001 : t;
      // Absolute channels: the last segment (in document order) that has begun.
      const abs: Partial<Record<string, number>> = {};
      let mx = 0;
      let my = 0;
      let sx = 1;
      let sy = 1;
      let rot = 0;
      let op = 1;
      let clip: string | null = null;
      for (const s of segs) {
        const v = s.value(at);
        if (v === null) continue;
        switch (s.ch) {
          case "mx":
            mx += v;
            break;
          case "my":
            my += v;
            break;
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
          case "clip":
            clip = s.region!(Math.min(1, Math.max(0, v)), b.bw, b.bh);
            break;
          default:
            abs[s.ch] = v;
        }
      }
      const f: PlayKeyframe = { offset: Math.min(1, Math.max(0, t / duration)) };
      if (fades) f.opacity = r3(hiddenAt(id, t, pre) ? 0 : b.shown * Math.max(0, Math.min(1, op * (abs.opAbs ?? 1))));
      if (moves) {
        const dx = (abs.x ?? g.ppt_x) - g.ppt_x + mx;
        const dy = (abs.y ?? g.ppt_y) - g.ppt_y + my;
        f.translate = `${r3(b.tx + dx * w * b.k)}px ${r3(b.ty + dy * h * b.k)}px`;
      }
      if (scales) {
        const kx = g.ppt_w > 0 ? (abs.w ?? g.ppt_w) / g.ppt_w : 1;
        const ky = g.ppt_h > 0 ? (abs.h ?? g.ppt_h) / g.ppt_h : 1;
        f.scale = `${r3(kx * sx)} ${r3(ky * sy)}`;
      }
      if (turns) f.rotate = `${r3((abs.rotAbs ?? 0) + rot)}deg`;
      if (clips) f.clipPath = clip ?? "none";
      return f;
    });
    tracks.push({ id, keyframes });
  }
  return { duration, tracks, notes };
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
  /** No motion: stand on the state at once. */
  reduced?: boolean;
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

function shownOpacity(el: HTMLElement): number {
  const s = originals.get(el)?.opacity ?? el.style.opacity;
  const n = s === "" ? 1 : Number(s);
  return Number.isFinite(n) ? Math.max(0, Math.min(1, n)) : 1;
}

/** Measure a tagged element (after the step's starting state is on it). */
function measure(tagged: TaggedSlide, id: ElementId, w: number, h: number): ElementBase | null {
  const [spid, para] = id.split("#");
  const shape = tagged.shapes.get(spid!);
  if (shape === undefined) return null;
  const el = para === undefined ? shape.el : shape.paras?.[Number(para)];
  if (el === undefined) return null;
  const t = pxPair(el.style.translate);
  if (t === null) return null;
  let geom: Geom;
  let k = 1;
  if (para === undefined) {
    const px = (v: string) => (v.endsWith("px") ? parseFloat(v) : NaN);
    const [x, y, bw, bh] = [px(el.style.left), px(el.style.top), px(el.style.width), px(el.style.height)];
    if (![x, y, bw, bh].every(Number.isFinite)) return null;
    geom = { ppt_x: (x + bw / 2) / w, ppt_y: (y + bh / 2) / h, ppt_w: bw / w, ppt_h: bh / h };
  } else {
    const slideEl = shape.el.parentElement;
    if (slideEl === null) return null;
    const s = slideEl.getBoundingClientRect();
    const r = el.getBoundingClientRect();
    const scale = s.width > 0 ? s.width / w : 1;
    geom = {
      ppt_x: ((r.left + r.right) / 2 - s.left) / scale / w,
      ppt_y: ((r.top + r.bottom) / 2 - s.top) / scale / h,
      ppt_w: r.width / scale / w,
      ppt_h: r.height / scale / h,
    };
    const chain: string[] = [];
    for (let e = el.parentElement; e !== null; e = e.parentElement) {
      chain.push(e.style.transform);
      if (e === shape.el) break;
    }
    const body = transformScale(chain);
    k = body === null || body <= 0 ? 1 : 1 / body;
  }
  return { geom, k, tx: t[0], ty: t[1], shown: shownOpacity(el), bw: el.offsetWidth, bh: el.offsetHeight };
}

function elementOf(tagged: TaggedSlide, id: ElementId): HTMLElement | undefined {
  const [spid, para] = id.split("#");
  const shape = tagged.shapes.get(spid!);
  return para === undefined ? shape?.el : shape?.paras?.[Number(para)];
}

/**
 * Play step `k` of `timeline` on a tagged slide (step 0 is what runs on
 * entry): from the state before it to `states[k]`. The returned `finish`
 * lands on that state at once; so does the end of the longest effect, the
 * `maxMs` cap, `reduced`, and a step with nothing to move.
 */
export function playStep(tagged: TaggedSlide, timeline: SlideTimeline, k: number, opts: PlayOptions): Playing {
  const states = opts.states ?? buildStates(timeline);
  const after = states[k];
  const effects = timeline.steps[k] ?? [];
  const before = k === 0 ? entryState(timeline) : states[k - 1];
  let resolve!: () => void;
  const finished = new Promise<void>((r) => (resolve = r));
  const playing: Playing = { finished, finish: () => {}, approximate: false };
  const land = () => {
    if (after !== undefined) playing.approximate = applyState(tagged, after, opts.w, opts.h) || tagged.fellBack;
    resolve();
  };
  if (after === undefined || before === undefined || opts.reduced === true || effects.length === 0) {
    land();
    return playing;
  }
  applyState(tagged, before, opts.w, opts.h);
  const info = new Map<string, ShapeInfo>();
  for (const [id, s] of tagged.shapes) info.set(id, { paraCount: s.paraCount, paras: s.paras !== null });
  // What the landing will say, known now (the landing sets it exactly).
  playing.approximate = planState(info, after).approximate || tagged.fellBack;
  const plan = planStep(effects, before, info, (id) => measure(tagged, id, opts.w, opts.h), opts.w, opts.h, opts.maxMs);
  const anims: Animation[] = [];
  if (plan.duration > 0) {
    for (const tr of plan.tracks) {
      const el = elementOf(tagged, tr.id);
      if (el === undefined || typeof el.animate !== "function") continue;
      try {
        anims.push(el.animate(tr.keyframes as unknown as Keyframe[], { duration: plan.duration, fill: "both", easing: "linear" }));
      } catch {
        // A keyframe this engine refuses: that element lands with the rest.
      }
    }
  }
  if (anims.length === 0) {
    land();
    return playing;
  }
  let done = false;
  const timer = setTimeout(() => playing.finish(), plan.duration);
  playing.finish = () => {
    if (done) return;
    done = true;
    clearTimeout(timer);
    // The state goes on first, in the same task: no frame shows neither.
    land();
    for (const a of anims) a.cancel();
  };
  return playing;
}
