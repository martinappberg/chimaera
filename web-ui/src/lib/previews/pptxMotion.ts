/**
 * What an animation effect does over time, read from its behaviours (not
 * from a table of presets, so a preset nobody listed still plays). Pure:
 * no DOM. `pptxPlay.ts` samples these into Web Animations keyframes.
 *
 * An effect becomes segments, each one channel of one target over time:
 * - `x`, `y`, `w`, `h`: the shape's centre and size in slide fractions
 *   (`p:anim` on `ppt_x` `ppt_y` `ppt_w` `ppt_h`, its time values read with
 *   `pptxFormula.ts`), absolute: the latest begun wins;
 * - `mx`, `my`: a motion path's offset from the layout place in slide
 *   fractions, absolute: the latest begun wins (R18);
 * - `sx`, `sy`: `p:animScale` `by` factors, multiplied; `sxAbs`, `syAbs`:
 *   its `to`, from the factor held before (absolute, as R26);
 * - `rot` (added up: `p:animRot by`) and `rotAbs` (`p:animRot from/to`,
 *   `p:anim` on a rotation), in degrees;
 * - `op` (a fade filter, multiplied) and `opAbs` (`style.opacity`);
 * - `clip`: a transition filter's visible region (`clipRegion`).
 * Visibility sets are not segments: the step planner replays them.
 *
 * What it can't play: an entrance or exit becomes a fade over its
 * duration, anything else plays nothing; either way the click ends on its
 * build state, and `note` says so for the slide's "approximate" tooltip.
 * Colour changes are not segments: `pptxPlay.ts` blends the drawn colours
 * between the states before and after the click. A set that never ends
 * (transparency) is instant, as in PowerPoint, and the state holds it.
 */

import { formulaValue, type FormulaVars } from "./pptxFormula";
import {
  animKeys,
  behaviourProgress,
  decimalOf,
  effectTime,
  instantDur,
  keyValue,
  targetKey,
  walkPath,
  type Effect,
  type TimeValue,
} from "./pptxTiming";

export { progress, warp } from "./pptxTiming";

/** A target's place (centre and size, in fractions of the slide) and what
 *  it holds before the step (R26–R28): a `to` scale, turn or opacity
 *  without a `from` starts there. */
export type Geom = FormulaVars & { sx?: number; sy?: number; turn?: number; op?: number };

export type Channel = "x" | "y" | "w" | "h" | "mx" | "my" | "sx" | "sy" | "sxAbs" | "syAbs" | "rot" | "rotAbs" | "op" | "opAbs" | "clip" | "mask";

export interface Segment {
  /** The target key (`targetKey`). */
  key: string;
  ch: Channel;
  /** The channel's value at `t` ms from the step's start; null before the
   *  behaviour begins. Past its end it holds its last value. For `clip`,
   *  the filter's progress (0–1). */
  value(t: number): number | null;
  /** For `clip`: the visible region at a progress, in the element's px. */
  region?: Region;
  /** For `mask`: the random pattern that shows the element cell by cell. */
  pattern?: Pattern;
}

/**
 * A random pattern a filter reveals its target through (no polygon can
 * draw it): `dissolve`'s cells, `randombar`'s bars across (`horizontal`)
 * or down. `out`: it hides instead. The value of a `mask` segment is the
 * filter's progress; the shown fraction is that, or 1 minus it going out.
 */
export interface Pattern {
  kind: "dissolve" | "bars";
  horizontal: boolean;
  out: boolean;
}

/** A visible region: a CSS `clip-path` for progress `p` on a `w`×`h` box
 *  whose top-left corner is at (`ox`, `oy`) in the element (a paragraph's
 *  text inside its block). */
export type Region = (p: number, w: number, h: number, ox?: number, oy?: number) => string;

export interface EffectMotion {
  segments: Segment[];
  /** The span the segments change in, ms from the step's start. */
  from: number;
  to: number;
  /** Why it plays other than PowerPoint would; null when it plays as written. */
  note: string | null;
}

// --- values -----------------------------------------------------------------------

function valueOf(v: TimeValue | null, vars: Geom): number | null {
  if (typeof v === "number") return Number.isFinite(v) ? v : null;
  if (typeof v === "string") return formulaValue(v, vars);
  return null;
}

/** Channels the state holds (R27, R28) read their values as the held
 *  state does, so the last frame is the landing. */
const HELD_CHANNELS = new Set<Channel>(["opAbs", "rotAbs"]);

const ANIM_CHANNEL = new Map<string, Channel>([
  ["ppt_x", "x"],
  ["ppt_y", "y"],
  ["ppt_w", "w"],
  ["ppt_h", "h"],
  ["style.opacity", "opAbs"],
  // Rotation values in `p:anim` are degrees (the corpus's boomerang runs
  // `style.rotation` from -90 to 0); only `p:animRot` counts 60000ths.
  ["style.rotation", "rotAbs"],
  ["r", "rotAbs"],
  ["ppt_r", "rotAbs"],
]);
const COLOUR_ATTRS = /^(fill|stroke|style\.color|fillcolor|ppt_c)/i;

function curOf(ch: Channel, g: Geom): number {
  switch (ch) {
    case "x":
      return g.ppt_x;
    case "y":
      return g.ppt_y;
    case "w":
      return g.ppt_w;
    case "h":
      return g.ppt_h;
    case "opAbs":
      return g.op ?? 1;
    case "rotAbs":
      return g.turn ?? 0;
    case "sx":
    case "sy":
    case "op":
      return 1;
    default:
      return 0;
  }
}

// --- filters --------------------------------------------------------------------

type Pt = [number, number];

function polygon(pts: readonly Pt[], evenodd: boolean): string {
  const r = (n: number) => Math.round(n * 100) / 100;
  return `polygon(${evenodd ? "evenodd, " : ""}${pts.map(([x, y]) => `${r(x)}px ${r(y)}px`).join(", ")})`;
}

/** A shape centred on the box growing with `q` (0: nothing, 1: covers the
 *  box), its first vertex its leftmost at mid height (the hole's bridge is
 *  horizontal, which no rasterizer draws). */
type Grow = (q: number, w: number, h: number) => Pt[];

const CIRCLE_POINTS = 48;

const GROW = new Map<string, Grow>([
  [
    "box",
    (q, w, h) => {
      const [cx, cy, a, b] = [w / 2, h / 2, (q * w) / 2, (q * h) / 2];
      return [[cx - a, cy], [cx - a, cy - b], [cx + a, cy - b], [cx + a, cy + b], [cx - a, cy + b]];
    },
  ],
  [
    "circle",
    (q, w, h) => {
      // An ellipse with the shape's aspect, through its corners at q = 1
      // (PowerPoint's iris: about 0.70·w by 0.66·h halfway).
      const rx = q * (w / 2) * Math.SQRT2;
      const ry = q * (h / 2) * Math.SQRT2;
      const out: Pt[] = [];
      for (let i = 0; i < CIRCLE_POINTS; i++) {
        const a = Math.PI + (2 * Math.PI * i) / CIRCLE_POINTS;
        out.push([w / 2 + rx * Math.cos(a), h / 2 + ry * Math.sin(a)]);
      }
      return out;
    },
  ],
  [
    "diamond",
    (q, w, h) => {
      const [cx, cy] = [w / 2, h / 2];
      return [[cx - q * w, cy], [cx, cy - q * h], [cx + q * w, cy], [cx, cy + q * h]];
    },
  ],
  [
    "plus",
    (q, w, h) => {
      // PowerPoint's film: the cross's arms span the whole box from the
      // start and thicken with the box's own aspect.
      const [cx, cy, t, u] = [w / 2, h / 2, (q * w) / 2, (q * h) / 2];
      return [
        [0, cy], [0, cy - u], [cx - t, cy - u], [cx - t, 0], [cx + t, 0], [cx + t, cy - u],
        [w, cy - u], [w, cy + u], [cx + t, cy + u], [cx + t, h], [cx - t, h], [cx - t, cy + u], [0, cy + u],
      ];
    },
  ],
  // Split: a band across the box (doors on its vertical or horizontal axis).
  ["barnVertical", (q, w, h) => { const a = (q * w) / 2; return [[w / 2 - a, h / 2], [w / 2 - a, 0], [w / 2 + a, 0], [w / 2 + a, h], [w / 2 - a, h]]; }],
  ["barnHorizontal", (q, w, h) => { const b = (q * h) / 2; return [[0, h / 2], [0, h / 2 - b], [w, h / 2 - b], [w, h / 2 + b], [0, h / 2 + b]]; }],
]);

/** The box less a grown shape (even-odd, bridged at mid height). */
function hole(shape: Pt[], w: number, h: number): Shape {
  const ring: Pt[] = [[0, h / 2], [0, 0], [w, 0], [w, h], [0, h], [0, h / 2]];
  return { pts: [...ring, ...shape, shape[0]!], evenodd: true };
}

function rect(x0: number, y0: number, x1: number, y1: number): Pt[] {
  return [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
}

const BLINDS = 6;
const CHECKER_ROWS = 6;
const WHEEL_ARC = 24;

/**
 * A transition filter's visible region at progress `p`, for an effect
 * coming in (`out` false) or going out. A direction names where the
 * motion starts: `wipe(down)` (PowerPoint's "From Bottom", subtype 4)
 * reveals the bottom first; `in` closes from the edges to the centre,
 * `out` opens from the centre. Going out, the same edge moves the same
 * way and hides instead of revealing. Null: a filter with no region
 * (`fade`, or one this doesn't draw: it fades).
 */
export function clipRegion(filter: string, out: boolean): Region | null {
  const region = regionOf(filter, out);
  if (region === null) return null;
  return (p, w, h, ox = 0, oy = 0) => {
    // The whole box shown is no clip at all.
    if (out ? p <= 0 : p >= 1) return "none";
    // The pattern is laid out on the shape's own box (PowerPoint's six
    // slats start at its top); a point on the box's edge is then pushed
    // out by a margin, so a shadow or an outline drawn past the box is
    // revealed with it, without rescaling the pattern.
    const e = clipMargin(w, h);
    const { pts, evenodd } = region(p, w, h);
    const push = (v: number, size: number) => (v <= 0.001 ? v - e : v >= size - 0.001 ? v + e : v);
    return polygon(pts.map(([x, y]): Pt => [push(x, w) + ox, push(y, h) + oy]), evenodd);
  };
}

/** px a region reaches past the box on each side. */
export function clipMargin(w: number, h: number): number {
  return Math.round(Math.max(w, h) * 0.08 + 6);
}

type Shape = { pts: Pt[]; evenodd: boolean };

function regionOf(filter: string, out: boolean): ((p: number, w: number, h: number) => Shape) | null {
  const m = /^([a-z]+)(?:\(([A-Za-z0-9]*)\))?$/.exec(filter.trim());
  if (m === null) return null;
  const name = m[1]!;
  const arg = m[2] ?? "";
  if (name === "wipe") {
    const side = arg === "" ? "left" : arg;
    if (!["left", "right", "up", "down"].includes(side)) return null;
    return (p, w, h) => {
      // The revealed span, from the named side.
      const a = out ? p : 0;
      const b = out ? 1 : p;
      const pts =
        side === "left" ? rect(a * w, 0, b * w, h)
        : side === "right" ? rect((1 - b) * w, 0, (1 - a) * w, h)
        : side === "up" ? rect(0, a * h, w, b * h)
        : rect(0, (1 - b) * h, w, (1 - a) * h);
      return { pts, evenodd: false };
    };
  }
  if (name === "blinds") {
    const horizontal = arg !== "vertical";
    if (arg !== "" && arg !== "horizontal" && arg !== "vertical") return null;
    return (p, w, h) => {
      const pts: Pt[] = [];
      for (let i = 0; i < BLINDS; i++) {
        // Each slat opens from its top (left), and closes the same way.
        const lo = (i + (out ? p : 0)) / BLINDS;
        const hi = (i + (out ? 1 : p)) / BLINDS;
        // Slats are joined along the box's own edge (x = 0 or y = 0).
        pts.push(...(horizontal ? rect(0, lo * h, w, hi * h) : rect(lo * w, 0, hi * w, h)));
        pts.push(horizontal ? [0, lo * h] : [lo * w, 0]);
      }
      return { pts, evenodd: false };
    };
  }
  if (name === "checkerboard") {
    // PowerPoint's film: six rows of squares a sixth of the box high, every
    // other row half a square along, each square filling from its left
    // edge (`across`; `down` is the same turned).
    if (arg !== "" && arg !== "across" && arg !== "down") return null;
    const down = arg === "down";
    return (p, w, h) => {
      const [U, V] = down ? [h, w] : [w, h];
      const s = V / CHECKER_ROWS;
      const q = out ? 1 - p : p;
      const pts: Pt[] = [];
      for (let r = 0; r < CHECKER_ROWS; r++) {
        // Squares are laid out from the box's centre, odd rows half a square on.
        const phase = U / 2 + (r % 2 === 1 ? s / 2 : 0);
        const first = Math.floor(-phase / s) - 1;
        for (let k = first; phase + k * s < U; k++) {
          const u0 = Math.max(0, phase + k * s);
          const u1 = Math.min(U, phase + k * s + q * s);
          if (u1 <= u0) continue;
          const [v0, v1] = [r * s, (r + 1) * s];
          const cell = down ? rect(v0, u0, v1, u1) : rect(u0, v0, u1, v1);
          pts.push(...cell, cell[0]!, [0, 0]);
        }
      }
      return { pts: pts.length > 0 ? [[0, 0], ...pts] : [[0, 0], [0, 0], [0, 0]], evenodd: false };
    };
  }
  if (name === "strips") {
    // A diagonal edge crossing the box from the corner it names away from
    // (`downLeft` starts at the top right and runs down to the left).
    const corner = new Map([["downLeft", [1, 0]], ["upLeft", [1, 1]], ["downRight", [0, 0]], ["upRight", [0, 1]]]).get(arg === "" ? "downRight" : arg);
    if (corner === undefined) return null;
    return (p, w, h) => {
      const [cx, cy] = [corner[0]! * w, corner[1]! * h];
      // Points at distance d along the diagonal: |x − cx| + |y − cy| ≤ d (in w+h units).
      const q = out ? 1 - p : p;
      const d = q * (w + h);
      const sx = corner[0] === 0 ? 1 : -1;
      const sy = corner[1] === 0 ? 1 : -1;
      const tri: Pt[] = [[cx, cy], [cx + sx * d, cy], [cx, cy + sy * d]];
      // Going out, the swept corner is what is hidden.
      return out ? hole(tri, w, h) : { pts: tri, evenodd: false };
    };
  }
  if (name === "wedge") {
    // Two edges sweeping down from 12 o'clock about the box's centre, one
    // each way (PowerPoint's film).
    if (arg !== "") return null;
    return (p, w, h) => {
      const [cx, cy] = [w / 2, h / 2];
      const r = Math.hypot(w, h);
      const q = out ? 1 - p : p;
      const pts: Pt[] = [[cx, cy]];
      for (let i = 0; i <= WHEEL_ARC; i++) {
        const a = -Math.PI / 2 - q * Math.PI + (2 * q * Math.PI * i) / WHEEL_ARC;
        pts.push([cx + r * Math.cos(a), cy + r * Math.sin(a)]);
      }
      return { pts, evenodd: false };
    };
  }
  if (name === "wheel") {
    const n = /^[1-8]$/.test(arg) ? Number(arg) : arg === "" ? 1 : NaN;
    if (!Number.isInteger(n)) return null;
    return (p, w, h) => {
      const [cx, cy] = [w / 2, h / 2];
      const r = Math.hypot(w, h);
      const pts: Pt[] = [];
      const sector = (2 * Math.PI) / n;
      for (let k = 0; k < n; k++) {
        // Clockwise from 12 o'clock; going out, the swept part is hidden.
        const a0 = k * sector + (out ? p * sector : 0);
        const a1 = k * sector + (out ? sector : p * sector);
        pts.push([cx, cy]);
        for (let i = 0; i <= WHEEL_ARC; i++) {
          const a = a0 + ((a1 - a0) * i) / WHEEL_ARC - Math.PI / 2;
          pts.push([cx + r * Math.cos(a), cy + r * Math.sin(a)]);
        }
      }
      return { pts, evenodd: false };
    };
  }
  let grow: Grow | undefined;
  let opens: boolean;
  if (name === "barn") {
    const bm = /^(in|out)(Vertical|Horizontal)$/.exec(arg);
    if (bm === null) return null;
    grow = GROW.get(`barn${bm[2]}`);
    opens = bm[1] === "out";
  } else {
    if (arg !== "in" && arg !== "out" && arg !== "") return null;
    grow = GROW.get(name);
    opens = arg !== "in";
  }
  if (grow === undefined) return null;
  const g = grow;
  // "out" opens from the centre (the shape grows); "in" closes from the
  // edges (the box less a shrinking shape). Going out is the complement of
  // coming in at the same progress.
  return (p, w, h) => {
    const showShape = opens !== out;
    const q = opens ? p : 1 - p;
    return showShape ? { pts: g(q, w, h), evenodd: false } : hole(g(q, w, h), w, h);
  };
}

/** A random-pattern filter (`dissolve`, `randombar(horizontal|vertical)`), else null. */
export function patternOf(filter: string, out: boolean): Pattern | null {
  const f = filter.trim();
  if (f === "dissolve") return { kind: "dissolve", horizontal: false, out };
  const m = /^randombar(?:\((horizontal|vertical)\))?$/.exec(f);
  return m === null ? null : { kind: "bars", horizontal: m[1] !== "vertical", out };
}

// --- paths ----------------------------------------------------------------------

/** Most points a sampled motion path keeps. */
export const PATH_MAX_POINTS = 4096;
const CURVE_STEPS = 16;
/**
 * A motion path as a polyline from (0, 0), walked by the timing
 * contract's own walker (`walkPath`, R19): its last point is `pathEnd(path)`.
 * Cubic curves are sampled; a move is a jump (`jump` marks the point it
 * jumps to). Null past `PATH_MAX_POINTS`.
 */
export function pathPolyline(path: string): { pts: Pt[]; jump: boolean[] } | null {
  const pts: Pt[] = [[0, 0]];
  const jump: boolean[] = [false];
  let over = false;
  walkPath(path, (p) => {
    if (pts.length > PATH_MAX_POINTS) {
      over = true;
      return false;
    }
    if (p.kind === "curve") {
      const [x, y] = p.from;
      for (let s = 1; s < CURVE_STEPS; s++) {
        const t = s / CURVE_STEPS;
        const u = 1 - t;
        pts.push([
          u * u * u * x + 3 * u * u * t * p.c1[0] + 3 * u * t * t * p.c2[0] + t * t * t * p.to[0],
          u * u * u * y + 3 * u * u * t * p.c1[1] + 3 * u * t * t * p.c2[1] + t * t * t * p.to[1],
        ]);
        jump.push(false);
      }
    }
    pts.push(p.to);
    jump.push(p.kind === "move");
  });
  return over ? null : { pts, jump };
}

/** The point at fraction `u` of the path's length (paced: constant speed;
 *  a jump takes no time). */
export function pacedSampler(line: { pts: Pt[]; jump: boolean[] }): (u: number) => Pt {
  const { pts, jump } = line;
  const cum = [0];
  for (let i = 1; i < pts.length; i++) {
    const d = jump[i] ? 0 : Math.hypot(pts[i]![0] - pts[i - 1]![0], pts[i]![1] - pts[i - 1]![1]);
    cum.push(cum[i - 1]! + d);
  }
  const total = cum[cum.length - 1]!;
  const last = pts[pts.length - 1]!;
  return (u) => {
    if (u >= 1 || total === 0) return u >= 1 || pts.length === 1 ? last : pts[0]!;
    const at = Math.max(0, u) * total;
    let lo = 1;
    let hi = pts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (cum[mid]! < at) lo = mid + 1;
      else hi = mid;
    }
    const a = pts[lo - 1]!;
    const b = pts[lo]!;
    const span = cum[lo]! - cum[lo - 1]!;
    const f = span === 0 ? 1 : (at - cum[lo - 1]!) / span;
    return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f];
  };
}

// --- an effect ------------------------------------------------------------------

function parseFilter(f: string | null): { name: string; full: string } {
  const full = (f ?? "").trim();
  return { name: /^[a-z]*/.exec(full)?.[0] ?? "", full };
}

/** Whether an effect brings its target in, takes it out, or neither. */
function direction(e: Effect): "in" | "out" | null {
  if (e.sets === "visible" || e.presetClass === "entr") return "in";
  if (e.sets === "hidden" || e.presetClass === "exit") return "out";
  return null;
}

/**
 * The segments of one effect, its times from the step's start. `geom`
 * gives a target's place (null: not drawn; its behaviours are skipped).
 */
export function effectMotion(e: Effect, geom: (key: string) => Geom | null): EffectMotion {
  const none = (note: string | null): EffectMotion => ({ segments: [], from: e.start, to: e.start, note });
  if (e.dur === null) {
    // A set that never ends (transparency's) acts at once and the state
    // holds it (R24); anything else that never ends can't be played.
    const instant = e.behaviours.every((b) => (b.dur !== null && b.repeatCount !== null) || b.kind === "set" || (b.kind === "effect" && b.filter === "image"));
    return none(instant ? null : `${e.name}: not animated (it never ends)`);
  }
  const end = e.start + e.dur;
  const dir = direction(e);
  const fade = (why: string): EffectMotion => {
    const key = e.target === null ? null : targetKey(e.target);
    if (dir === null || key === null || e.dur === null || e.dur <= 0) return none(dir === null ? `${e.name}: not animated (${why})` : null);
    const d = e.dur;
    const value = (t: number) => (t < e.start ? null : dir === "in" ? Math.min(1, (t - e.start) / d) : Math.max(0, 1 - (t - e.start) / d));
    return { segments: [{ key, ch: "op", value }], from: e.start, to: end, note: `${e.name}: plays as a fade (${why})` };
  };

  const clock = (t: number) => effectTime(e, t - e.start);
  const segments: Segment[] = [];
  let note: string | null = null;
  let colour = false;
  for (const b of e.behaviours) {
    const t = b.target ?? e.target;
    if (t === null) continue;
    const key = targetKey(t);
    const g = geom(key);
    // Commands and media have no picture to move.
    if (b.kind === "other") continue;
    if (b.kind === "color") continue;
    if (b.kind === "set") {
      if (b.attrs.includes("style.visibility")) continue;
      if (b.attrs.includes("style.opacity")) {
        const v = decimalOf(b.to) ?? NaN;
        if (!Number.isFinite(v)) return fade("an opacity it can't read");
        segments.push({ key, ch: "opAbs", value: (s) => { const c = clock(s); return c === null || c < b.delay ? null : v; } });
        continue;
      }
      continue;
    }
    if (instantDur(b) === null || b.repeatCount === null) return fade("a part that never ends");
    if (g === null) continue;
    const local = (s: number): number | null => {
      const c = clock(s);
      return c === null ? null : behaviourProgress(b, c);
    };
    switch (b.kind) {
      case "effect": {
        const f = parseFilter(b.filter);
        const goingOut = b.transition === "out";
        if (f.name === "fade") {
          segments.push({ key, ch: "op", value: (s) => { const q = local(s); return q === null ? null : goingOut ? 1 - q : q; } });
          break;
        }
        if (f.name === "image" || f.name === "") break;
        const pattern = patternOf(f.full, goingOut);
        if (pattern !== null) {
          segments.push({ key, ch: "mask", value: local, pattern });
          break;
        }
        const region = clipRegion(f.full, goingOut);
        if (region === null) {
          // A filter with no region here (dissolve, checkerboard, …) fades.
          note ??= `${e.name}: plays as a fade (${f.name})`;
          segments.push({ key, ch: "op", value: (s) => { const q = local(s); return q === null ? null : goingOut ? 1 - q : q; } });
          break;
        }
        segments.push({ key, ch: "clip", value: local, region });
        break;
      }
      case "anim": {
        const attr = b.attrs[0] ?? "";
        const ch = ANIM_CHANNEL.get(attr);
        if (ch === undefined) {
          if (COLOUR_ATTRS.test(attr)) {
            colour = true;
            break;
          }
          return fade(`${attr || "an attribute"} isn't animated here`);
        }
        const read = HELD_CHANNELS.has(ch) ? decimalOf : (v: TimeValue | null) => valueOf(v, g);
        const keys = animKeys(b, read, curOf(ch, g));
        if (keys === null) return fade(`a value it can't read on ${attr}`);
        const discrete = b.calcmode === "discrete";
        segments.push({ key, ch, value: (s) => { const q = local(s); return q === null ? null : keyValue(keys, q, discrete); } });
        break;
      }
      case "scale": {
        const k = (pt: { x: number; y: number } | null, axis: "x" | "y") => (pt === null ? null : pt[axis] / 100000);
        for (const axis of ["x", "y"] as const) {
          const held = (axis === "x" ? g.sx : g.sy) ?? 1;
          const from = k(b.from, axis);
          const to = k(b.to, axis);
          const by = k(b.by, axis);
          // `to` wins over `by` and is of the shape's own size (R26).
          if (to !== null) {
            const a0 = from ?? held;
            if (!Number.isFinite(a0) || !Number.isFinite(to)) return fade("a scale it can't read");
            segments.push({ key, ch: axis === "x" ? "sxAbs" : "syAbs", value: (s) => { const q = local(s); return q === null ? null : a0 + (to - a0) * q; } });
          } else if (by !== null) {
            if (!Number.isFinite(by)) return fade("a scale it can't read");
            const a0 = from === null ? null : from / held;
            segments.push({ key, ch: axis === "x" ? "sx" : "sy", value: (s) => { const q = local(s); return q === null ? null : (a0 ?? 1) * (1 + (by - 1) * q); } });
          }
        }
        break;
      }
      case "rot": {
        if (b.from !== null || b.to !== null) {
          const a = b.from === null ? (g.turn ?? 0) : b.from / 60000;
          const z = b.to !== null ? b.to / 60000 : a + (b.by ?? 0) / 60000;
          segments.push({ key, ch: "rotAbs", value: (s) => { const q = local(s); return q === null ? null : a + (z - a) * q; } });
        } else if (b.by !== null) {
          const by = b.by / 60000;
          segments.push({ key, ch: "rot", value: (s) => { const q = local(s); return q === null ? null : by * q; } });
        }
        break;
      }
      case "motion": {
        if (b.path === null) break;
        const line = pathPolyline(b.path);
        if (line === null) return fade("a path too long to follow");
        const at = pacedSampler(line);
        segments.push({ key, ch: "mx", value: (s) => { const q = local(s); return q === null ? null : at(q)[0]; } });
        segments.push({ key, ch: "my", value: (s) => { const q = local(s); return q === null ? null : at(q)[1]; } });
        break;
      }
    }
  }
  if (colour) note ??= `${e.name}: a colour change on an attribute other than fill, line or text isn't shown`;
  return { segments, from: e.start, to: end, note };
}

/** Why each effect on a slide plays other than as written (deduplicated,
 *  in order): what the slide's "approximate" tooltip lists. Geometry-free. */
export function motionNotes(steps: readonly (readonly Effect[])[]): string[] {
  const unit: Geom = { ppt_x: 0.5, ppt_y: 0.5, ppt_w: 0.25, ppt_h: 0.25 };
  const out = new Set<string>();
  for (const step of steps) for (const e of step) {
    const n = effectMotion(e, () => unit).note;
    if (n !== null) out.add(n);
  }
  return [...out];
}
