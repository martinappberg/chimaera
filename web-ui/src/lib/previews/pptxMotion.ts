/**
 * What an animation effect does over time, read from its behaviours (not
 * from a table of presets, so a preset nobody listed still plays). Pure:
 * no DOM. `pptxPlay.ts` samples these into Web Animations keyframes.
 *
 * An effect becomes segments, each one channel of one target over time:
 * - `x`, `y`, `w`, `h`: the shape's centre and size in slide fractions
 *   (`p:anim` on `ppt_x` `ppt_y` `ppt_w` `ppt_h`, its time values read with
 *   `pptxFormula.ts`), absolute: the latest begun wins;
 * - `mx`, `my`: a motion path's offset in slide fractions, added up;
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
import { progress, targetKey, type Behaviour, type Effect } from "./pptxTiming";

export { progress, warp } from "./pptxTiming";

/** A target's place (centre and size, in fractions of the slide) and what
 *  it holds before the step (R26–R27): a `to` scale or turn starts there. */
export type Geom = FormulaVars & { sx?: number; sy?: number; turn?: number };

export type Channel = "x" | "y" | "w" | "h" | "mx" | "my" | "sx" | "sy" | "sxAbs" | "syAbs" | "rot" | "rotAbs" | "op" | "opAbs" | "clip";

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
}

/** A visible region: a CSS `clip-path` for progress `p` on a `w`×`h` box. */
export type Region = (p: number, w: number, h: number) => string;

export interface EffectMotion {
  segments: Segment[];
  /** The span the segments change in, ms from the step's start. */
  from: number;
  to: number;
  /** Why it plays other than PowerPoint would; null when it plays as written. */
  note: string | null;
}

// --- time ---------------------------------------------------------------------------

/** A set whose dur is 1 is PowerPoint's instant (README R10). */
function bDur(b: Behaviour): number | null {
  if (b.dur === null) return null;
  return b.kind === "set" && b.dur === 1 ? 0 : b.dur;
}

/** The effect's own span before its repeat: its latest finite behaviour end. */
function baseSpan(e: Effect): number {
  let span = 0;
  for (const b of e.behaviours) {
    const d = bDur(b);
    if (d === null || b.repeatCount === null) continue;
    span = Math.max(span, b.delay + (d * b.repeatCount * (b.autoRev ? 2 : 1)) / 1000);
  }
  return span;
}

/** Step time → the effect's own time (its accel/decel/repeat/autoRev). */
function effectClock(e: Effect): (t: number) => number | null {
  const span = baseSpan(e);
  const plain = e.accel === 0 && e.decel === 0 && !e.autoRev && e.repeatCount === 1000;
  return (t) => {
    const tau = t - e.start;
    if (tau < 0) return null;
    if (plain || span <= 0) return tau;
    const q = progress(tau, span, e.repeatCount, e.autoRev, e.accel, e.decel);
    return q === null ? null : q * span;
  };
}

// --- values -----------------------------------------------------------------------

/** Keyframes: progress (0–1) → value, sorted by progress. */
type Keys = { at: number; v: number }[];

function interpolate(keys: Keys, q: number, discrete: boolean): number {
  if (q <= keys[0]!.at) return keys[0]!.v;
  for (let i = 1; i < keys.length; i++) {
    const k = keys[i]!;
    if (q < k.at || (q === k.at && i === keys.length - 1)) {
      const j = keys[i - 1]!;
      if (q === k.at) return k.v;
      if (discrete) return j.v;
      const span = k.at - j.at;
      return span <= 0 ? k.v : j.v + ((k.v - j.v) * (q - j.at)) / span;
    }
  }
  return keys[keys.length - 1]!.v;
}

function valueOf(v: string | number | boolean | null, vars: Geom): number | null {
  if (typeof v === "number") return Number.isFinite(v) ? v : null;
  if (typeof v === "string") return formulaValue(v, vars);
  return null;
}

/** `p:anim`'s values as keyframes, or null when they aren't plain numbers
 *  and the four shape variables. `cur` is the attribute's value now. */
function animKeys(b: Behaviour & { kind: "anim" }, vars: Geom, cur: number): Keys | null {
  if (b.valueType !== null && b.valueType !== "num") return null;
  if (b.calcmode === "fmla") return null;
  if (b.tav.length > 0) {
    const n = b.tav.length;
    const keys: Keys = [];
    for (const [i, tav] of b.tav.entries()) {
      if (tav.fmla !== null && tav.fmla.trim() !== "") return null;
      const v = valueOf(tav.val, vars);
      if (v === null) return null;
      const at = tav.tm === null ? (n === 1 ? 0 : i / (n - 1)) : tav.tm / 100000;
      if (!(at >= 0 && at <= 1)) return null;
      if (keys.length > 0 && at < keys[keys.length - 1]!.at) return null;
      keys.push({ at, v });
    }
    return keys;
  }
  const from = b.from === null ? cur : formulaValue(b.from, vars);
  if (from === null) return null;
  let to: number | null = cur;
  if (b.to !== null) to = formulaValue(b.to, vars);
  else if (b.by !== null) {
    const by = formulaValue(b.by, vars);
    to = by === null ? null : from + by;
  }
  return to === null ? null : [
    { at: 0, v: from },
    { at: 1, v: to },
  ];
}

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
      const r = q * Math.hypot(w / 2, h / 2);
      const out: Pt[] = [];
      for (let i = 0; i < CIRCLE_POINTS; i++) {
        const a = Math.PI + (2 * Math.PI * i) / CIRCLE_POINTS;
        out.push([w / 2 + r * Math.cos(a), h / 2 + r * Math.sin(a)]);
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
      const [cx, cy, L, H, t, u] = [w / 2, h / 2, q * w, q * h, (q * w) / 2, (q * h) / 2];
      return [
        [cx - L, cy], [cx - L, cy - u], [cx - t, cy - u], [cx - t, cy - H], [cx + t, cy - H], [cx + t, cy - u],
        [cx + L, cy - u], [cx + L, cy + u], [cx + t, cy + u], [cx + t, cy + H], [cx - t, cy + H], [cx - t, cy + u], [cx - L, cy + u],
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
  return (p, w, h) => {
    // The whole box shown is no clip at all.
    if (out ? p <= 0 : p >= 1) return "none";
    // The region covers the box and a margin around it, so a shape's
    // shadow (drawn past its box) is revealed with it, not after.
    const e = clipMargin(w, h);
    const { pts, evenodd } = region(p, w + 2 * e, h + 2 * e);
    return polygon(pts.map(([x, y]): Pt => [x - e, y - e]), evenodd);
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

// --- paths ----------------------------------------------------------------------

/** Most points a sampled motion path keeps. */
export const PATH_MAX_POINTS = 4096;
const CURVE_STEPS = 16;
const PATH_ARITY = new Map<string, number>([
  ["M", 2],
  ["L", 2],
  ["C", 6],
]);

/**
 * A motion path as a polyline from (0, 0), walked by the timing
 * contract's rules (R19, as `pathEnd`): its last point is `pathEnd(path)`.
 * Cubic curves are sampled; a move is a jump (`jump` marks the point it
 * jumps to). Null past `PATH_MAX_POINTS`.
 */
export function pathPolyline(path: string): { pts: Pt[]; jump: boolean[] } | null {
  const tokens = path.match(/[A-Za-z]|[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?/g) ?? [];
  const pts: Pt[] = [[0, 0]];
  const jump: boolean[] = [false];
  let x = 0;
  let y = 0;
  let sx = 0;
  let sy = 0;
  let cmd = "";
  let i = 0;
  const add = (p: Pt, j: boolean) => {
    pts.push(p);
    jump.push(j);
  };
  while (i < tokens.length) {
    if (pts.length > PATH_MAX_POINTS) return null;
    const tok = tokens[i]!;
    if (/^[A-Za-z]$/.test(tok)) {
      i++;
      if (tok === "E" || tok === "e") break;
      if (tok === "Z" || tok === "z") {
        x = sx;
        y = sy;
        add([x, y], false);
        cmd = "";
        continue;
      }
      if (!PATH_ARITY.has(tok.toUpperCase())) break;
      cmd = tok;
      continue;
    }
    const arity = PATH_ARITY.get(cmd.toUpperCase());
    if (arity === undefined || i + arity > tokens.length) break;
    const v = tokens.slice(i, i + arity).map(Number);
    if (v.some((n) => !Number.isFinite(n))) break;
    i += arity;
    const rel = cmd === cmd.toLowerCase();
    const ox = rel ? x : 0;
    const oy = rel ? y : 0;
    const nx = v[arity - 2]! + ox;
    const ny = v[arity - 1]! + oy;
    if (cmd === "C" || cmd === "c") {
      const [c1x, c1y, c2x, c2y] = [v[0]! + ox, v[1]! + oy, v[2]! + ox, v[3]! + oy];
      for (let s = 1; s < CURVE_STEPS; s++) {
        const t = s / CURVE_STEPS;
        const u = 1 - t;
        add(
          [
            u * u * u * x + 3 * u * u * t * c1x + 3 * u * t * t * c2x + t * t * t * nx,
            u * u * u * y + 3 * u * u * t * c1y + 3 * u * t * t * c2y + t * t * t * ny,
          ],
          false,
        );
      }
    }
    const move = cmd === "M" || cmd === "m";
    x = nx;
    y = ny;
    add([x, y], move);
    if (move) {
      sx = x;
      sy = y;
      cmd = cmd === "M" ? "L" : "l";
    }
  }
  return { pts, jump };
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

  const clock = effectClock(e);
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
        const v = typeof b.to === "number" ? b.to : typeof b.to === "string" ? Number(b.to) : NaN;
        if (!Number.isFinite(v)) return fade("an opacity it can't read");
        segments.push({ key, ch: "opAbs", value: (s) => { const c = clock(s); return c === null || c < b.delay ? null : v; } });
        continue;
      }
      continue;
    }
    const d = bDur(b);
    if (d === null || b.repeatCount === null) return fade("a part that never ends");
    if (g === null) continue;
    const local = (s: number): number | null => {
      const c = clock(s);
      return c === null ? null : progress(c - b.delay, d, b.repeatCount, b.autoRev, b.accel, b.decel);
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
        const keys = animKeys(b, g, curOf(ch, g));
        if (keys === null) return fade(`a value it can't read on ${attr}`);
        const discrete = b.calcmode === "discrete";
        segments.push({ key, ch, value: (s) => { const q = local(s); return q === null ? null : interpolate(keys, q, discrete); } });
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
