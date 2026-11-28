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
 */

import type { BuildState } from "./pptxTiming";
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
