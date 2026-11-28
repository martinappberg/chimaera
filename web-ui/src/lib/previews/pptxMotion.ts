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
 * - `clip`: a transition filter's mask, its soft edge moving (`clipRegion`);
 * - `mask`: a pattern filter's pictures (`patternOf`).
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
  /** For `clip`: the filter's mask at a progress, in the element's px. */
  region?: Region;
  /** For `clip`: the filter (`wipe(left)`), for where it lays its sweep. */
  filter?: string;
  /** For `mask`: the pattern whose pictures show the element (`value`: the filter's progress). */
  pattern?: Pattern;
}

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

/**
 * The soft edge of every transition filter, as a fraction of its sweep.
 * PowerPoint's film (`entrances`, `filters`, `exits-emphasis`): wipe,
 * split, blinds, box, circle, diamond, plus, wheel, wedge, checkerboard
 * and strips all draw their moving edge as a linear ramp 15% of the
 * distance the edge travels (47 px on a 388 px wipe, 27° on the wedge's
 * 180°, 8 px on a 53 px checkerboard cell), and that ramp enters and
 * leaves the shape inside the effect's duration.
 */
export const FEATHER = 0.15;

/** How much of a point at level `level` (0: the sweep reaches it first,
 *  1: last) shows at progress `p`; going `out`, how much is left. */
export function revealAt(level: number, p: number, out: boolean): number {
  const a = Math.min(1, Math.max(0, (p * (1 + FEATHER) - level) / FEATHER));
  return out ? 1 - a : a;
}

/** A level as a piecewise-linear function of one coordinate: `[t0, t1,
 *  level at t0, level at t1]`, pieces in order; past the ends it holds. */
type Piece = [number, number, number, number];

/** One gradient a filter's reveal is drawn with, in the box's own px:
 *  - `linear`: the level along a direction (`angle`, CSS degrees), `t` px
 *    from the box's centre;
 *  - `radial`: along an ellipse's radius (`rx`×`ry` at the centre), `t`
 *    from 0 to 1;
 *  - `conic`: by angle about the centre, clockwise from 12 o'clock, `t` in
 *    degrees of the box squared up (PowerPoint's wheel and wedge sweep the
 *    box's own aspect, not the screen's angle). */
type Layer = { kind: "linear"; angle: number; pieces: Piece[] } | { kind: "radial"; rx: number; ry: number; pieces: Piece[] } | { kind: "conic"; pieces: Piece[] };

/** A filter's reveal: one layer, or two combined (`intersect`: both,
 *  multiplied; `add`: either). Going out each layer is complemented and
 *  the combination swapped, so the shown part is exactly what coming in
 *  would show's complement. */
interface Reveal {
  layers: Layer[];
  op: "intersect" | "add";
}

/** A frame of a filter's mask, as inline CSS on the element (`maskImage`
 *  "none": nothing hidden). */
export interface MaskFrame {
  maskImage: string;
  maskSize: string;
  maskPosition: string;
  maskRepeat: string;
  maskComposite: string;
}

/** A filter's mask at progress `p` on a `w`×`h` box whose top-left corner
 *  is at (`ox`, `oy`) in the element (a paragraph's text inside its block). */
export type Region = (p: number, w: number, h: number, ox?: number, oy?: number) => MaskFrame;

const levelAt = (pieces: readonly Piece[], t: number): number => {
  const first = pieces[0]!;
  if (t <= first[0]) return first[2];
  for (const [t0, t1, l0, l1] of pieces) if (t <= t1) return t1 > t0 ? l0 + ((l1 - l0) * (t - t0)) / (t1 - t0) : l1;
  return pieces[pieces.length - 1]![3];
};

/** `t` of a point (box px, origin top-left) on a layer. */
function layerT(l: Layer, x: number, y: number, w: number, h: number): number {
  const [dx, dy] = [x - w / 2, y - h / 2];
  if (l.kind === "linear") {
    const a = (l.angle * Math.PI) / 180;
    return dx * Math.sin(a) - dy * Math.cos(a);
  }
  if (l.kind === "radial") return Math.hypot(dx / l.rx, dy / l.ry);
  // The squared-up angle: the box stretched to a square of side 2.
  const deg = (Math.atan2(dx / (w / 2), -dy / (h / 2)) * 180) / Math.PI;
  return deg < 0 ? deg + 360 : deg;
}

/** How much of the point (`x`, `y`) in the box shows, by the same layers
 *  the mask draws (tests read the model through this). */
export function revealPoint(filter: string, out: boolean, p: number, x: number, y: number, w: number, h: number): number | null {
  const r = revealOf(filter, w, h);
  if (r === null) return null;
  const as = r.layers.map((l) => revealAt(levelAt(l.pieces, layerT(l, x, y, w, h)), p, out));
  const op = out ? (r.op === "add" ? "intersect" : "add") : r.op;
  return as.reduce((a, b) => (op === "intersect" ? a * b : a + b - a * b));
}

const px = (n: number) => `${Math.round(n * 100) / 100}px`;
const deg = (n: number) => `${Math.round(n * 100) / 100}deg`;
const alpha = (a: number) => `rgba(0,0,0,${Math.round(a * 1000) / 1000})`;

/** The stops of one piece: its ends and where the ramp starts and stops.
 *  `at` maps `t` to the gradient's own position; `bend`: extra stops
 *  inside the ramp where `at` is not linear (a conic squared up). */
function pieceStops(piece: Piece, p: number, out: boolean, at: (t: number) => string, bend: number): string[] {
  const [t0, t1, l0, l1] = piece;
  const ts = [t0, t1];
  if (l1 !== l0) {
    const lo = p * (1 + FEATHER) - FEATHER;
    const hi = p * (1 + FEATHER);
    for (const l of [lo, hi]) {
      const t = t0 + ((l - l0) * (t1 - t0)) / (l1 - l0);
      if (t > Math.min(t0, t1) && t < Math.max(t0, t1)) ts.push(t);
    }
    if (bend > 0) {
      const [a, b] = [lo, hi].map((l) => Math.min(Math.max(t0 + ((l - l0) * (t1 - t0)) / (l1 - l0), Math.min(t0, t1)), Math.max(t0, t1)));
      for (let i = 1; i < bend; i++) ts.push(a! + ((b! - a!) * i) / bend);
    }
  }
  const lv = (t: number) => (t1 > t0 ? l0 + ((l1 - l0) * (t - t0)) / (t1 - t0) : l1);
  return [...new Set(ts)].sort((a, b) => (t1 >= t0 ? a - b : b - a)).map((t) => `${alpha(revealAt(lv(t), p, out))} ${at(t)}`);
}

/** A layer as a CSS gradient on an image of `W`×`H` px whose centre is the box's. */
function layerCss(l: Layer, p: number, out: boolean, w: number, h: number, W: number, H: number): string {
  const stops = (at: (t: number) => string, bend = 0) => l.pieces.flatMap((pc) => pieceStops(pc, p, out, at, bend)).join(", ");
  if (l.kind === "linear") {
    const a = (l.angle * Math.PI) / 180;
    const len = Math.abs(W * Math.sin(a)) + Math.abs(H * Math.cos(a));
    return `linear-gradient(${deg(l.angle)}, ${stops((t) => px(len / 2 + t))})`;
  }
  if (l.kind === "radial") return `radial-gradient(${px(l.rx)} ${px(l.ry)} at ${px(W / 2)} ${px(H / 2)}, ${stops((t) => px(t * l.rx))})`;
  // Squared-up degrees to the screen's.
  const screen = (t: number) => {
    const r = (t * Math.PI) / 180;
    let d = (Math.atan2(w * Math.sin(r), h * Math.cos(r)) * 180) / Math.PI;
    if (d < 0 || (d === 0 && t > 180)) d += 360;
    return deg(d);
  };
  return `conic-gradient(from 0deg at ${px(W / 2)} ${px(H / 2)}, ${stops(screen, 6)})`;
}

/** |t| over a half extent `r`: 0 at the centre, 1 at the edge (`inv`: reversed). */
const fromCentre = (r: number, inv: boolean): Piece[] => (inv ? [[-r, 0, 0, 1], [0, r, 1, 0]] : [[-r, 0, 1, 0], [0, r, 0, 1]]);

const BLINDS = 6;
/** Rows of checkerboard cells (PowerPoint's film: cells h/6 square). */
export const CHECKER_ROWS = 6;

/** A filter's reveal on a `w`×`h` box, else null (a filter it doesn't draw). */
function revealOf(filter: string, w: number, h: number): Reveal | null {
  const m = /^([a-z]+)(?:\(([A-Za-z0-9]*)\))?$/.exec(filter.trim());
  if (m === null) return null;
  const name = m[1]!;
  const arg = m[2] ?? "";
  const one = (l: Layer): Reveal => ({ layers: [l], op: "intersect" });
  // Directions are CSS angles: 90 runs left to right, 180 top to bottom.
  const across = (angle: number, pieces: Piece[]): Layer => ({ kind: "linear", angle, pieces });
  if (name === "wipe") {
    // A direction names the side that shows first: `wipe(down)` is
    // PowerPoint's "From Bottom" (subtype 4).
    const side = arg === "" ? "left" : arg;
    const by = new Map([["left", [90, w]], ["right", [270, w]], ["up", [180, h]], ["down", [0, h]]]).get(side);
    if (by === undefined) return null;
    const [angle, len] = by as [number, number];
    return one(across(angle, [[-len / 2, len / 2, 0, 1]]));
  }
  if (name === "barn") {
    // Split: `out` opens from the centre line, `in` closes onto it.
    const bm = /^(in|out)(Vertical|Horizontal)$/.exec(arg);
    if (bm === null) return null;
    const vertical = bm[2] === "Vertical";
    return one(across(vertical ? 90 : 180, fromCentre((vertical ? w : h) / 2, bm[1] === "in")));
  }
  if (name === "blinds") {
    // Six slats on the shape's own box, each opening from its top (left).
    if (arg !== "" && arg !== "horizontal" && arg !== "vertical") return null;
    const len = arg === "vertical" ? w : h;
    const s = len / BLINDS;
    const pieces: Piece[] = [];
    for (let i = 0; i < BLINDS; i++) pieces.push([-len / 2 + i * s, -len / 2 + (i + 1) * s, 0, 1]);
    return one(across(arg === "vertical" ? 90 : 180, pieces));
  }
  if (name === "wedge") {
    // Two edges sweeping down from 12 o'clock, one each way.
    if (arg !== "") return null;
    return one({ kind: "conic", pieces: [[0, 180, 0, 1], [180, 360, 1, 0]] });
  }
  if (name === "wheel") {
    // Spokes clockwise from 12 o'clock, each sweeping its sector.
    const n = /^[1-8]$/.test(arg) ? Number(arg) : arg === "" ? 1 : NaN;
    if (!Number.isInteger(n)) return null;
    const s = 360 / n;
    return one({ kind: "conic", pieces: Array.from({ length: n }, (_, k): Piece => [k * s, (k + 1) * s, 0, 1]) });
  }
  if (arg !== "in" && arg !== "out" && arg !== "") return null;
  // `out` opens from the centre; `in` closes from the edges onto it.
  const closes = arg === "in";
  if (name === "circle") {
    // An ellipse with the shape's aspect, through its corners at the end.
    const [rx, ry] = [(w / 2) * Math.SQRT2, (h / 2) * Math.SQRT2];
    return one({ kind: "radial", rx, ry, pieces: [[0, 1, closes ? 1 : 0, closes ? 0 : 1]] });
  }
  const bandX = across(90, fromCentre(w / 2, closes));
  const bandY = across(180, fromCentre(h / 2, closes));
  // A box is inside both bands; a plus (arms spanning the whole box,
  // PowerPoint's film) inside either. Closing, the complement of each.
  if (name === "box") return { layers: [bandX, bandY], op: closes ? "add" : "intersect" };
  if (name === "plus") return { layers: [bandX, bandY], op: closes ? "intersect" : "add" };
  if (name === "diamond") {
    // |x|/w + |y|/h = max(|x/w + y/h|, |x/w − y/h|): two diagonal bands,
    // reaching the box's corners at the end.
    const g = Math.hypot(1 / w, 1 / h);
    const a1 = (Math.atan2(1 / w, -1 / h) * 180) / Math.PI;
    const a2 = (Math.atan2(1 / w, 1 / h) * 180) / Math.PI;
    return { layers: [across(a1, fromCentre(1 / g, closes)), across(a2, fromCentre(1 / g, closes))], op: closes ? "add" : "intersect" };
  }
  return null;
}

/**
 * A transition filter's mask at each progress, for an effect coming in
 * (`out` false) or going out (the same edge moving the same way, hiding).
 * Null: a filter with no gradient here (`fade`, a pattern, or one it
 * doesn't draw: it fades). The mask covers the box pushed out by a margin
 * on each side, holding the edge's value there, so a shadow or an outline
 * drawn past the box comes with it.
 */
export function clipRegion(filter: string, out: boolean): Region | null {
  if (revealOf(filter, 100, 100) === null) return null;
  return (p, w, h, ox = 0, oy = 0) => {
    const whole = out ? p <= 0 : p >= 1;
    const e = clipMargin(w, h);
    const [W, H] = [w + 2 * e, h + 2 * e];
    const frame = { maskSize: `${px(W)} ${px(H)}`, maskPosition: `${px(ox - e)} ${px(oy - e)}`, maskRepeat: "no-repeat" };
    const r = whole ? null : revealOf(filter, w, h);
    if (r === null) return { maskImage: "none", maskComposite: "add", ...frame };
    // The image's px are the box's, shifted by the margin; a linear layer's
    // gradient line runs through the image's centre, which is the box's.
    const op = out ? (r.op === "add" ? "intersect" : "add") : r.op;
    return { maskImage: r.layers.map((l) => layerCss(l, p, out, w, h, W, H)).join(", "), maskComposite: op, ...frame };
  };
}

/** px a filter's mask reaches past the box on each side. */
export function clipMargin(w: number, h: number): number {
  return Math.round(Math.max(w, h) * 0.08 + 6);
}

/**
 * A filter drawn through pictures, one per level, that no gradient can
 * draw: `dissolve`'s cells and `randombar`'s bars (`horizontal`: bars
 * across) appear at random, `checkerboard`'s cells (`horizontal`: across,
 * rows half a cell apart) and `strips`' squares (from `corner`, in box
 * fractions) fill with the soft edge every filter has. `out`: it hides.
 */
export interface Pattern {
  kind: "dissolve" | "bars" | "checker" | "strips";
  horizontal: boolean;
  out: boolean;
  corner?: [number, number];
}

/** A pattern filter, else null. */
export function patternOf(filter: string, out: boolean): Pattern | null {
  const f = filter.trim();
  if (f === "dissolve") return { kind: "dissolve", horizontal: false, out };
  const bars = /^randombar(?:\((horizontal|vertical)\))?$/.exec(f);
  if (bars !== null) return { kind: "bars", horizontal: bars[1] !== "vertical", out };
  const checker = /^checkerboard(?:\((across|down)\))?$/.exec(f);
  if (checker !== null) return { kind: "checker", horizontal: checker[1] !== "down", out };
  const strips = /^strips(?:\(([A-Za-z]*)\))?$/.exec(f);
  if (strips !== null) {
    // The corner the squares start from (`downLeft` starts at the top right).
    const corner = new Map<string, [number, number]>([["downLeft", [1, 0]], ["upLeft", [1, 1]], ["downRight", [0, 0]], ["upRight", [0, 1]]]).get(strips[1] || "downRight");
    return corner === undefined ? null : { kind: "strips", horizontal: false, out, corner };
  }
  return null;
}

/** The share of a random pattern shown at progress `p` (coming in).
 *  PowerPoint's film of the `filters` deck, the shape's ink by frame:
 *  dissolve shows 9% a quarter of the way, 49% half way, 90% at three
 *  quarters; random bars 18%, 50%, 86%. Both fit p^a / (p^a + (1−p)^a). */
export function patternShare(kind: Pattern["kind"], p: number): number {
  const a = kind === "dissolve" ? 2.1 : kind === "bars" ? 1.5 : 1;
  const q = Math.min(1, Math.max(0, p));
  if (q <= 0 || q >= 1) return q;
  return q ** a / (q ** a + (1 - q) ** a);
}

type Pt = [number, number];

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

/** A name a deck wrote, as a note quotes it: a file can make it any
 *  length, and a tooltip only needs to say which. */
const NOTE_NAME_MAX = 40;
export function noteName(s: string): string {
  return s.length <= NOTE_NAME_MAX ? s : `${s.slice(0, NOTE_NAME_MAX - 1)}…`;
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
          // A filter it doesn't draw fades.
          note ??= `${e.name}: plays as a fade (${noteName(f.name)})`;
          segments.push({ key, ch: "op", value: (s) => { const q = local(s); return q === null ? null : goingOut ? 1 - q : q; } });
          break;
        }
        segments.push({ key, ch: "clip", value: local, region, filter: f.full });
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
          return fade(`${noteName(attr) || "an attribute"} isn't animated here`);
        }
        const read = HELD_CHANNELS.has(ch) ? decimalOf : (v: TimeValue | null) => valueOf(v, g);
        const keys = animKeys(b, read, curOf(ch, g));
        if (keys === null) return fade(`a value it can't read on ${noteName(attr)}`);
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
