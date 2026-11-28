/**
 * A slide's animation timing and transition, read from its XML, and the
 * slide's state after each build. Pure: no DOM, no Svelte.
 *
 * The contract is `fixtures/pptx-timing/README.md`: what PowerPoint writes,
 * the effect vocabulary, the rules below, and the golden JSON each corpus
 * deck and case must parse to. The PowerPoint plugin's Rust parser is held
 * to the same goldens, so a change in what this module returns for the
 * corpus is a contract change.
 *
 * Reads the timing tree as PowerPoint plays it: an effect starts at the sum
 * of the delays on its path below its click group (PowerPoint writes an
 * "after previous" effect's group delay as the end of what came before it).
 *
 * Caps (past any, the slide plays as a plain slide with an `approximate`
 * code; the slide-part bound comes first because `scan` runs before the
 * timing-level caps can be measured):
 * - the slide part is at most 4 MiB of UTF-8;
 * - the `p:timing` source is at most 512 KiB, measured in UTF-8 bytes;
 * - at most 2,000 effects, counting every `p:cTn` in `p:timing` that has
 *   `presetClass` or one of the three effect `nodeType`s, interactive
 *   sequences included;
 * - at most 300 click groups in the main sequence;
 * - nesting counts `p:par`/`p:seq`/`p:excl` containers, starting at 1 for
 *   the root `p:par` under `p:tnLst`; more than 32 is over.
 * Nothing here throws: XML `scan` refuses is a plain slide too.
 *
 * Every string the file supplies is treated as hostile: vocabulary lookups
 * go through `Map`s, objects keyed by file ids have no prototype, and a
 * shape id is accepted only as 1–10 ASCII digits.
 */

import { decodeEntities, scan, type XmlEl } from "./pptxXml";

/** An attribute's value as XML gives it (README R23a): each literal tab,
 *  newline or carriage return in the source becomes a space (a CRLF pair
 *  one space) before references are decoded, so `&#10;` stays a newline. */
function attr(xml: string, el: XmlEl, name: string): string | null {
  const a = el.attrs.find((x) => x.qname === name);
  if (a === undefined) return null;
  return decodeEntities(xml.slice(a.valueStart, a.valueEnd).replace(/\r\n|[\t\n\r]/g, " "));
}

/** Most child transforms or attributes a colour may carry, and most `by`
 *  changes one target's part may hold (R21): a real colour has a handful. */
export const COLOR_CAPS = { parts: 8, chain: 16 } as const;

const NS_P = new Set([
  "http://schemas.openxmlformats.org/presentationml/2006/main",
  "http://purl.oclc.org/ooxml/presentationml/main",
]);
const NS_MC = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const NS_P14 = "http://schemas.microsoft.com/office/powerpoint/2010/main";
const NS_P159 = "http://schemas.microsoft.com/office/powerpoint/2015/09/main";
/** Namespaces whose `mc:Choice` we take: what PowerPoint wraps transitions in. */
const UNDERSTOOD = new Set([NS_P14, NS_P159]);

/** Past any of these the slide plays as a plain slide (`approximate` says which). */
export const TIMING_CAPS = {
  /** UTF-8 bytes of the slide part. */
  partBytes: 4 * 1024 * 1024,
  /** UTF-8 bytes of the `p:timing` source. */
  timingBytes: 512 * 1024,
  effects: 2000,
  /** Click groups in the main sequence. */
  clicks: 300,
  /** `p:par`/`p:seq`/`p:excl` nesting, the root `p:par` being 1. */
  depth: 32,
  /** What the states may hold in all: each main-sequence behaviour's
   *  target keys (a paragraph range one per paragraph, up to
   *  `HELD_PARAGRAPHS`) times the states from its step on (R21). */
  values: 250_000,
} as const;

// --- types -------------------------------------------------------------------------

export type Trigger = "click" | "with" | "after";
export type Visibility = "visible" | "hidden";

export type EffectName =
  | "appear" | "disappear"
  | "fade-in" | "fade-out"
  | "fly-in" | "fly-out"
  | "wipe-in" | "wipe-out"
  | "zoom-in" | "zoom-out"
  | "float-in" | "float-out"
  | "split-in" | "split-out"
  | "wheel-in" | "wheel-out"
  | "circle-in" | "circle-out"
  | "box-in" | "box-out"
  | "blinds-in" | "blinds-out"
  | "dissolve-in" | "dissolve-out"
  | "grow-shrink" | "spin" | "transparency" | "teeter" | "fill-color" | "darken"
  | "path"
  | "other";

export type Side = "top" | "right" | "bottom" | "left";

/** Why a slide's timing was dropped; the codes are part of the contract. */
export type Approximate =
  | "unreadable"
  | "unexpected"
  | "part-too-large"
  | "timing-too-large"
  | "too-many-effects"
  | "too-many-clicks"
  | "too-deep"
  | "colour-too-large"
  | "too-many-values";

export interface Target {
  /** Shape id (`cNvPr@id`): 1–10 ASCII digits. */
  spid: string;
  /** The text box's background (`p:bg`). */
  bg: boolean;
  /** Paragraph range (`p:txEl/p:pRg`), from 0 as PowerPoint numbers them. */
  para: [number, number] | null;
}

export type TimeValue = string | number | boolean;

export interface Color {
  /** The colour element's local name (`srgbClr`, `schemeClr`, `hsl`, `rgb`, …). */
  type: string;
  /** Its attributes, as written (no prototype: keys are the file's). */
  attrs: Record<string, string>;
  /** Its child transforms in order (`lumMod`, `tint`, `alpha`, …): local name and `val`. */
  mods: { name: string; val: string | null }[];
}

interface BehaviourBase {
  target: Target | null;
  /** Offset from the effect's start, ms. */
  delay: number;
  /** One iteration, ms; null for `indefinite`, absent or malformed. */
  dur: number | null;
  /** 1000ths of an iteration (1000 = once); null for `indefinite`. */
  repeatCount: number | null;
  autoRev: boolean;
  /** 1000ths of the duration spent accelerating / decelerating. */
  accel: number;
  decel: number;
  additive: string | null;
  /** Its `p:cTn`'s `fill` (`hold`, `freeze`, `remove`, `transition`), as written. */
  fill: string | null;
  /** `p:attrNameLst` entries. */
  attrs: string[];
}

export type Behaviour = BehaviourBase &
  (
    | { kind: "set"; to: TimeValue | null }
    | { kind: "effect"; filter: string | null; transition: string | null; prLst: string | null }
    | {
        kind: "anim";
        calcmode: string | null;
        valueType: string | null;
        by: string | null;
        from: string | null;
        to: string | null;
        tav: { tm: number | null; fmla: string | null; val: TimeValue | null }[];
      }
    | { kind: "scale"; by: Point | null; from: Point | null; to: Point | null }
    | { kind: "rot"; by: number | null; from: number | null; to: number | null }
    | { kind: "motion"; path: string | null; origin: string | null; pathEditMode: string | null }
    | { kind: "color"; clrSpc: string | null; dir: string | null; by: Color | null; from: Color | null; to: Color | null }
    | { kind: "other"; element: string }
  );

export interface Point {
  x: number;
  y: number;
}

export interface Effect {
  trigger: Trigger;
  /** From the start of the step, ms. */
  start: number;
  /** ms; null when nothing in it ends (see the README's duration rule). */
  dur: number | null;
  presetClass: string | null;
  preset: number;
  subtype: number;
  name: EffectName;
  /** For fly and wipe: the side, from the subtype. */
  dir: Side | null;
  target: Target | null;
  /** What it leaves the target's visibility at (its visibility set that ends last). */
  sets: Visibility | null;
  /** The motion path, on `path` effects. */
  path: string | null;
  /** `grpId`: ties the effect to its `p:bldLst` label. */
  group: number | null;
  /** The effect node's own timing modifiers. */
  accel: number;
  decel: number;
  autoRev: boolean;
  repeatCount: number | null;
  /** The effect node's own `fill`, as written. */
  fill: string | null;
  behaviours: Behaviour[];
}

export interface Transition {
  kind: "none" | "fade" | "fade-black" | "push" | "wipe" | "cut" | "morph" | "other";
  /** The transition element's `dir`, as written. */
  dir: string | null;
  /** ms */
  dur: number;
  /** `advTm`, ms. */
  advanceAfter: number | null;
  advanceOnClick: boolean;
  /** Morph's option (`byObject` when unstated). */
  morph: string | null;
  /** The transition element's local name (`checker` for an `other`). */
  element: string | null;
}

/** A `p:bldLst` entry: labels only, never timing. */
export interface BuildLabel {
  spid: string;
  group: number | null;
  /** `bldP`'s `build` (`p` = by paragraph), `bldGraphic`, … */
  kind: string;
  build: string | null;
  level: number | null;
  animBg: boolean;
}

export interface InteractiveSequence {
  /** The shape whose click starts it. */
  trigger: string | null;
  /** One list per click on the trigger. */
  steps: Effect[][];
}

export interface SlideTimeline {
  /** `steps[0]` runs on entry; `steps[k]` is click k. Always at least one. */
  steps: Effect[][];
  transition: Transition | null;
  interactive: InteractiveSequence[];
  builds: BuildLabel[];
  /** Why the timing was dropped (the slide then plays as a plain slide). */
  approximate: Approximate | null;
}

export interface BuildState {
  /** Target keys not on screen, sorted. */
  hidden: string[];
  /** Target key → summed path offset [dx, dy] in slide fractions, keys sorted.
   *  No prototype: keys come from the file. */
  moved: Record<string, [number, number]>;
  /** Held size factors [x, y] (R26), only targets not at [1, 1]; absent when none. */
  scale?: Record<string, [number, number]>;
  /** Held turn in degrees, in [0, 360) (R27), only targets not at 0; absent when none. */
  turn?: Record<string, number>;
  /** Held opacity (R28), only targets not at 1; absent when none. */
  opacity?: Record<string, number>;
  /** Held colour changes per part (R29), absent when none. */
  color?: Record<string, HeldColors>;
}

/** A target's held colour changes, by what they colour. */
export interface HeldColors {
  fill?: HeldColor;
  line?: HeldColor;
  text?: HeldColor;
}

/** One part's held colour: the last colour set (`to`), the changes made on
 *  top of it in order (`by`), and the fill/line switches set with them. */
export interface HeldColor {
  to?: Color;
  by?: Color[];
  /** `fill.type` set to `solid`: the part is filled with one colour. */
  solid?: true;
  /** `fill.on` / `stroke.on`, as set. */
  on?: boolean;
}

// --- numbers and strings -----------------------------------------------------------

const INT = /^-?[0-9]{1,9}$/;
const SPID = /^[0-9]{1,10}$/;

/** An integer attribute: an optional `-` and 1–9 ASCII digits, else null. */
function intOf(v: string | null): number | null {
  return v !== null && INT.test(v) ? Number(v) : null;
}
/** An integer that counts as 0 below 0. */
function natOf(v: string | null): number | null {
  const n = intOf(v);
  return n === null ? null : Math.max(0, n);
}
/** A decimal number (README R2a): an optional sign, digits with an
 *  optional fraction (or a fraction alone), an optional exponent; nothing
 *  else (no hex, no blank, no whitespace, no `Infinity`). */
const DECIMAL = /^[-+]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][-+]?[0-9]+)?$/;
function numOf(v: string | null): number | null {
  if (v === null || !DECIMAL.test(v)) return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

/** A time value as a number (R28): a `fltVal`/`intVal`, or a `strVal`
 *  whose text is a decimal number (PowerPoint writes transparency as
 *  `strVal "0.5"`); a `boolVal` never is. */
export function decimalOf(v: TimeValue | null): number | null {
  if (typeof v === "number") return Number.isFinite(v) ? v : null;
  return typeof v === "string" ? numOf(v) : null;
}
function truthy(v: string | null): boolean {
  return v === "1" || v === "true";
}

/** UTF-8 length of `s`, stopping once it passes `limit` (a lone surrogate
 *  counts 3, as a UTF-8 encoder writes U+FFFD for it). */
function utf8Over(s: string, start: number, end: number, limit: number): boolean {
  if (end - start > limit) return true;
  if ((end - start) * 3 <= limit) return false;
  let n = 0;
  for (let i = start; i < end; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x80) n += 1;
    else if (c < 0x800) n += 2;
    else if (c >= 0xd800 && c <= 0xdbff && i + 1 < end) {
      const d = s.charCodeAt(i + 1);
      if (d >= 0xdc00 && d <= 0xdfff) {
        n += 4;
        i++;
      } else n += 3;
    } else n += 3;
    if (n > limit) return true;
  }
  return false;
}

// --- tree helpers ----------------------------------------------------------------

function isP(el: XmlEl, local: string): boolean {
  return el.local === local && NS_P.has(el.ns);
}
function childP(el: XmlEl | undefined, local: string): XmlEl | undefined {
  return el?.children.find((c) => isP(c, local));
}
function childrenP(el: XmlEl | undefined, local: string): XmlEl[] {
  return el === undefined ? [] : el.children.filter((c) => isP(c, local));
}
/** An element's text as XML gives it (R23a): comments and processing
 *  instructions dropped, CDATA sections taken as written (not decoded),
 *  the rest's references decoded. */
function text(xml: string, el: XmlEl): string {
  const raw = xml.slice(el.openEnd, el.closeStart);
  let out = "";
  const re = /<!--[\s\S]*?-->|<\?[\s\S]*?\?>|<!\[CDATA\[([\s\S]*?)\]\]>/g;
  let last = 0;
  for (let m = re.exec(raw); m !== null; m = re.exec(raw)) {
    out += decodeEntities(raw.slice(last, m.index)) + (m[1] ?? "");
    last = m.index + m[0].length;
  }
  // XML's whitespace only (R23a): a no-break space or U+FEFF is text.
  return (out + decodeEntities(raw.slice(last))).replace(/^[ \t\n\r]+|[ \t\n\r]+$/g, "");
}
/** An attribute by namespace + local name, whatever prefix the part binds. */
function attrNs(xml: string, el: XmlEl, ns: string, local: string): string | null {
  for (const a of el.attrs) {
    const colon = a.qname.indexOf(":");
    if (colon < 0 || a.qname.slice(colon + 1) !== local) continue;
    if (el.scope.get(a.qname.slice(0, colon)) === ns) return decodeEntities(xml.slice(a.valueStart, a.valueEnd));
  }
  return null;
}

/**
 * Children of `el`, with each `mc:AlternateContent` replaced by the children
 * of the branch we take: the first `mc:Choice` whose every `Requires` prefix
 * resolves to a namespace we understand, else `mc:Fallback`.
 */
function resolvedChildren(xml: string, el: XmlEl): XmlEl[] {
  const out: XmlEl[] = [];
  for (const c of el.children) {
    if (c.local !== "AlternateContent" || c.ns !== NS_MC) {
      out.push(c);
      continue;
    }
    const branch =
      c.children.find((b) => {
        if (b.local !== "Choice" || b.ns !== NS_MC) return false;
        const req = (attr(xml, b, "Requires") ?? "").split(/\s+/).filter((s) => s !== "");
        return req.length > 0 && req.every((p) => UNDERSTOOD.has(b.scope.get(p) ?? ""));
      }) ?? c.children.find((b) => b.local === "Fallback" && b.ns === NS_MC);
    if (branch !== undefined) out.push(...branch.children);
  }
  return out;
}

// --- vocabulary ------------------------------------------------------------------

const ENTR_EXIT = new Map<number, string>([
  [1, "appear"],
  [2, "fly"],
  [3, "blinds"],
  [4, "box"],
  [6, "circle"],
  [9, "dissolve"],
  [10, "fade"],
  [16, "split"],
  [21, "wheel"],
  [22, "wipe"],
  [23, "zoom"],
  [42, "float"],
]);
const EMPH = new Map<number, EffectName>([
  [1, "fill-color"],
  [6, "grow-shrink"],
  [8, "spin"],
  [9, "transparency"],
  [24, "darken"],
  [32, "teeter"],
]);
const SIDES = new Map<number, Side>([
  [1, "top"],
  [2, "right"],
  [4, "bottom"],
  [8, "left"],
]);

/** The vocabulary name for a preset (`other` outside it). */
export function effectName(presetClass: string | null, preset: number): EffectName {
  if (presetClass === "path") return "path";
  if (presetClass === "emph") return EMPH.get(preset) ?? "other";
  if (presetClass !== "entr" && presetClass !== "exit") return "other";
  const base = ENTR_EXIT.get(preset);
  if (base === undefined) return "other";
  if (base === "appear") return presetClass === "entr" ? "appear" : "disappear";
  return `${base}-${presetClass === "entr" ? "in" : "out"}` as EffectName;
}

function effectDir(presetClass: string | null, preset: number, subtype: number): Side | null {
  if (presetClass !== "entr" && presetClass !== "exit") return null;
  if (preset !== 2 && preset !== 22) return null;
  return SIDES.get(subtype) ?? null;
}

// --- targets ---------------------------------------------------------------------

/** A target as a golden-file key: `"4"`, `"4:bg"`, `"4:p1-1"`. */
export function targetKey(t: Target): string {
  if (t.bg) return `${t.spid}:bg`;
  if (t.para !== null) return `${t.spid}:p${t.para[0]}-${t.para[1]}`;
  return t.spid;
}

/** A shape id as the contract accepts it, else null. */
function spidOf(v: string | null): string | null {
  return v !== null && SPID.test(v) ? v : null;
}

function readTarget(xml: string, tgtEl: XmlEl | undefined): Target | null {
  if (tgtEl === undefined) return null;
  const sp = childP(tgtEl, "spTgt") ?? childP(tgtEl, "inkTgt");
  if (sp === undefined) return null;
  const spid = spidOf(attr(xml, sp, "spid"));
  if (spid === null) return null;
  const pRg = childP(childP(sp, "txEl"), "pRg");
  let para: [number, number] | null = null;
  if (pRg !== undefined) {
    const st = intOf(attr(xml, pRg, "st"));
    const end = intOf(attr(xml, pRg, "end"));
    if (st === null || end === null || st < 0 || end < 0) return null;
    para = [st, end];
  }
  // A character range (`charRg`), sub-shape and chart element animate within
  // the shape; until playback models them they target the whole shape.
  return { spid, bg: childP(sp, "bg") !== undefined, para };
}

// --- timing ----------------------------------------------------------------------

/** A time node's start delay: its first `p:cond` without `evt` whose
 *  `delay` is an integer (negative counts as 0); 0 when none is. */
function condDelay(xml: string, cTn: XmlEl): number {
  for (const c of childrenP(childP(cTn, "stCondLst"), "cond")) {
    if (attr(xml, c, "evt") !== null) continue;
    const d = natOf(attr(xml, c, "delay"));
    if (d !== null) return d;
  }
  return 0;
}

interface NodeTiming {
  delay: number;
  dur: number | null;
  repeatCount: number | null;
  autoRev: boolean;
  accel: number;
  decel: number;
}

function nodeTiming(xml: string, cTn: XmlEl | undefined): NodeTiming {
  if (cTn === undefined) return { delay: 0, dur: null, repeatCount: 1000, autoRev: false, accel: 0, decel: 0 };
  const rep = attr(xml, cTn, "repeatCount");
  return {
    delay: condDelay(xml, cTn),
    dur: natOf(attr(xml, cTn, "dur")),
    repeatCount: rep === "indefinite" ? null : (natOf(rep) ?? 1000),
    autoRev: truthy(attr(xml, cTn, "autoRev")),
    accel: intOf(attr(xml, cTn, "accel")) ?? 0,
    decel: intOf(attr(xml, cTn, "decel")) ?? 0,
  };
}

function timeValue(xml: string, holder: XmlEl | undefined): TimeValue | null {
  const v = holder?.children[0];
  if (v === undefined) return null;
  const val = attr(xml, v, "val");
  switch (v.local) {
    case "strVal":
      return val ?? "";
    case "fltVal":
    case "intVal":
      return numOf(val);
    case "boolVal":
      return truthy(val);
    default:
      return val;
  }
}

function point(xml: string, el: XmlEl | undefined): Point | null {
  if (el === undefined) return null;
  const x = numOf(attr(xml, el, "x"));
  const y = numOf(attr(xml, el, "y"));
  return x === null || y === null ? null : { x, y };
}

/** A colour past `COLOR_CAPS`: the slide plays as a plain slide (R21). */
class ColourTooLarge extends Error {}

function color(xml: string, el: XmlEl | undefined): Color | null {
  const c = el?.children[0];
  if (c === undefined) return null;
  const attrs = Object.create(null) as Record<string, string>;
  // A prefixed attribute is never part of a colour (PowerPoint writes
  // none; R29): dropped, so no unbound prefix reaches anything.
  // Namespace declarations are not attributes (R21).
  const own = c.attrs.filter((a) => !a.qname.includes(":") && a.qname !== "xmlns");
  if (own.length > COLOR_CAPS.parts || c.children.length > COLOR_CAPS.parts) throw new ColourTooLarge();
  for (const a of [...own].sort((x, y) => (x.qname < y.qname ? -1 : x.qname > y.qname ? 1 : 0))) attrs[a.qname] = attr(xml, c, a.qname)!;
  return { type: c.local, attrs, mods: c.children.map((m) => ({ name: m.local, val: attr(xml, m, "val") })) };
}

const BEHAVIOURS = new Set(["set", "anim", "animEffect", "animScale", "animRot", "animMotion", "animClr", "cmd", "audio", "video"]);
const CONTAINERS = new Set(["par", "seq", "excl"]);

/** The behaviours under an effect, flattened, each delay relative to the effect. */
function readBehaviours(xml: string, list: XmlEl | undefined, offset: number, out: Behaviour[]): void {
  for (const b of list?.children ?? []) {
    if (!NS_P.has(b.ns)) continue;
    if (CONTAINERS.has(b.local)) {
      const cTn = childP(b, "cTn");
      if (cTn === undefined) continue;
      readBehaviours(xml, childP(cTn, "childTnLst"), offset + condDelay(xml, cTn), out);
      continue;
    }
    if (!BEHAVIOURS.has(b.local)) continue;
    const cBhvr = childP(b, "cBhvr") ?? childP(b, "cMediaNode");
    const cTn = childP(cBhvr, "cTn");
    const t = nodeTiming(xml, cTn);
    const base: BehaviourBase = {
      target: readTarget(xml, childP(cBhvr, "tgtEl")),
      delay: offset + t.delay,
      dur: t.dur,
      repeatCount: t.repeatCount,
      autoRev: t.autoRev,
      accel: t.accel,
      decel: t.decel,
      additive: cBhvr === undefined ? null : attr(xml, cBhvr, "additive"),
      fill: cTn === undefined ? null : attr(xml, cTn, "fill"),
      attrs: childrenP(childP(cBhvr, "attrNameLst"), "attrName").map((a) => text(xml, a)),
    };
    switch (b.local) {
      case "set":
        out.push({ ...base, kind: "set", to: timeValue(xml, childP(b, "to")) });
        break;
      case "animEffect":
        out.push({
          ...base,
          kind: "effect",
          filter: attr(xml, b, "filter"),
          transition: attr(xml, b, "transition"),
          prLst: attr(xml, b, "prLst"),
        });
        break;
      case "anim":
        out.push({
          ...base,
          kind: "anim",
          calcmode: attr(xml, b, "calcmode"),
          valueType: attr(xml, b, "valueType"),
          by: attr(xml, b, "by"),
          from: attr(xml, b, "from"),
          to: attr(xml, b, "to"),
          tav: childrenP(childP(b, "tavLst"), "tav").map((tav) => ({
            tm: intOf(attr(xml, tav, "tm")),
            fmla: attr(xml, tav, "fmla"),
            val: timeValue(xml, childP(tav, "val")),
          })),
        });
        break;
      case "animScale":
        out.push({
          ...base,
          kind: "scale",
          by: point(xml, childP(b, "by")),
          from: point(xml, childP(b, "from")),
          to: point(xml, childP(b, "to")),
        });
        break;
      case "animRot":
        out.push({
          ...base,
          kind: "rot",
          by: intOf(attr(xml, b, "by")),
          from: intOf(attr(xml, b, "from")),
          to: intOf(attr(xml, b, "to")),
        });
        break;
      case "animMotion":
        out.push({
          ...base,
          kind: "motion",
          path: attr(xml, b, "path"),
          origin: attr(xml, b, "origin"),
          pathEditMode: attr(xml, b, "pathEditMode"),
        });
        break;
      case "animClr":
        out.push({
          ...base,
          kind: "color",
          clrSpc: attr(xml, b, "clrSpc"),
          dir: attr(xml, b, "dir"),
          by: color(xml, childP(b, "by")),
          from: color(xml, childP(b, "from")),
          to: color(xml, childP(b, "to")),
        });
        break;
      default:
        out.push({ ...base, kind: "other", element: b.local });
    }
  }
}

function isVisibilitySet(b: Behaviour): b is Behaviour & { kind: "set" } {
  return b.kind === "set" && b.attrs.includes("style.visibility");
}

/**
 * When a behaviour ends, relative to its effect, in thousandths of a ms
 * (exact integers): delay + dur × repeatCount/1000 × (2 with autoRev).
 * `indefinite` (dur absent, malformed or `indefinite`, or repeatCount
 * `indefinite`) is null. A `set` whose dur is exactly 1 is PowerPoint's
 * "instant" and counts as dur 0: an appear lasts nothing, and PowerPoint
 * starts the "after previous" effect that follows it at 0.
 */
function behaviourEnd(b: BehaviourBase & { kind: string }): bigint | null {
  if (b.dur === null || b.repeatCount === null) return null;
  const dur = b.kind === "set" && b.dur === 1 ? 0 : b.dur;
  return BigInt(b.delay) * 1000n + BigInt(dur) * BigInt(b.repeatCount) * (b.autoRev ? 2n : 1n);
}

/** `n / d` rounded half away from zero (n, d ≥ 0). */
function roundDiv(n: bigint, d: bigint): number {
  return Number((2n * n + d) / (2n * d));
}

const NODE_TRIGGERS = new Map<string, Trigger>([
  ["clickEffect", "click"],
  ["withEffect", "with"],
  ["afterEffect", "after"],
]);

function isEffectNode(xml: string, cTn: XmlEl): boolean {
  const nodeType = attr(xml, cTn, "nodeType");
  return (nodeType !== null && NODE_TRIGGERS.has(nodeType)) || attr(xml, cTn, "presetClass") !== null;
}

function readEffect(xml: string, cTn: XmlEl, start: number, first: boolean): Effect {
  const presetClass = attr(xml, cTn, "presetClass");
  const preset = intOf(attr(xml, cTn, "presetID")) ?? 0;
  const subtype = intOf(attr(xml, cTn, "presetSubtype")) ?? 0;
  const nodeType = attr(xml, cTn, "nodeType");
  const behaviours: Behaviour[] = [];
  readBehaviours(xml, childP(cTn, "childTnLst"), 0, behaviours);
  const t = nodeTiming(xml, cTn);

  // Span: the latest finite end; null when none is finite or any repeats forever.
  let span: bigint | null = null;
  let forever = t.repeatCount === null;
  for (const b of behaviours) {
    if (b.repeatCount === null) forever = true;
    const end = behaviourEnd(b);
    if (end !== null && (span === null || end > span)) span = end;
  }
  const dur =
    forever || span === null
      ? null
      : roundDiv(span * BigInt(t.repeatCount ?? 1000) * (t.autoRev ? 2n : 1n), 1_000_000n);

  // The visibility the effect leaves: its set that ends last (a set that never
  // ends counts as last; ties: the later in document order).
  let sets: Visibility | null = null;
  let setEnd: bigint | null | undefined;
  for (const b of behaviours) {
    if (!isVisibilitySet(b) || (b.to !== "visible" && b.to !== "hidden")) continue;
    const end = behaviourEnd(b);
    const wins = setEnd === undefined || end === null || (setEnd !== null && end >= setEnd);
    if (wins) {
      sets = b.to;
      setEnd = end;
    }
  }
  const motion = behaviours.find((b) => b.kind === "motion");
  const name = effectName(presetClass, preset);
  return {
    trigger: (nodeType === null ? undefined : NODE_TRIGGERS.get(nodeType)) ?? (first ? "click" : "with"),
    start,
    dur,
    presetClass,
    preset,
    subtype,
    name,
    dir: effectDir(presetClass, preset, subtype),
    target: behaviours.find((b) => b.target !== null)?.target ?? null,
    sets,
    path: name === "path" && motion?.kind === "motion" ? motion.path : null,
    group: intOf(attr(xml, cTn, "grpId")),
    accel: t.accel,
    decel: t.decel,
    autoRev: t.autoRev,
    repeatCount: t.repeatCount,
    fill: attr(xml, cTn, "fill"),
    behaviours,
  };
}

/** The effects under one click group, in document order, each started at
 *  the summed delays on its path below the group. */
function readGroup(xml: string, groupCTn: XmlEl): Effect[] {
  const out: Effect[] = [];
  const visit = (list: XmlEl | undefined, offset: number): void => {
    for (const node of list?.children ?? []) {
      if (!NS_P.has(node.ns) || !CONTAINERS.has(node.local)) continue;
      const cTn = childP(node, "cTn");
      if (cTn === undefined) continue;
      const at = offset + condDelay(xml, cTn);
      if (isEffectNode(xml, cTn)) out.push(readEffect(xml, cTn, at, out.length === 0));
      else visit(childP(cTn, "childTnLst"), at);
    }
  };
  visit(childP(groupCTn, "childTnLst"), 0);
  return out;
}

/** The click groups of a sequence (its `cTn`'s child time nodes). */
function groupsOf(seqCTn: XmlEl): XmlEl[] {
  return (childP(seqCTn, "childTnLst")?.children ?? [])
    .filter((c) => NS_P.has(c.ns) && CONTAINERS.has(c.local))
    .map((c) => childP(c, "cTn"))
    .filter((c): c is XmlEl => c !== undefined);
}

/** A sequence's clicks: one effect list per group that yields any effect. */
function readSteps(xml: string, groups: readonly XmlEl[]): Effect[][] {
  return groups.map((g) => readGroup(xml, g)).filter((s) => s.length > 0);
}

// --- transition ------------------------------------------------------------------

const SPEED = new Map<string, number>([
  ["fast", 500],
  ["med", 750],
  ["slow", 1000],
]);
const IGNORED_TRANSITION_CHILDREN = new Set(["sndAc", "extLst"]);

function readTransition(xml: string, el: XmlEl): Transition {
  const p14dur = natOf(attrNs(xml, el, NS_P14, "dur"));
  const spd = attr(xml, el, "spd");
  const effect = el.children.find((c) => !(NS_P.has(c.ns) && IGNORED_TRANSITION_CHILDREN.has(c.local)));
  let kind: Transition["kind"] = "other";
  let morph: string | null = null;
  if (effect === undefined) kind = "none";
  else if (effect.local === "morph" && effect.ns === NS_P159) {
    kind = "morph";
    morph = attr(xml, effect, "option") ?? "byObject";
  } else if (NS_P.has(effect.ns)) {
    if (effect.local === "fade") kind = truthy(attr(xml, effect, "thruBlk")) ? "fade-black" : "fade";
    else if (effect.local === "push" || effect.local === "wipe" || effect.local === "cut") kind = effect.local;
  }
  const advClick = attr(xml, el, "advClick");
  return {
    kind,
    dir: effect === undefined ? null : attr(xml, effect, "dir"),
    dur: p14dur ?? (spd === null ? undefined : SPEED.get(spd)) ?? 500,
    advanceAfter: natOf(attr(xml, el, "advTm")),
    advanceOnClick: !(advClick === "0" || advClick === "false"),
    morph,
    element: effect?.local ?? null,
  };
}

// --- the slide ----------------------------------------------------------------------

function plain(transition: Transition | null, approximate: Approximate | null): SlideTimeline {
  return { steps: [[]], transition, interactive: [], builds: [], approximate };
}

/** Container nesting (the root `p:par` is 1) and effect count in `timing`,
 *  without recursion. */
function measure(xml: string, timing: XmlEl): { depth: number; effects: number } {
  let depth = 0;
  let effects = 0;
  const stack: [XmlEl, number][] = [[timing, 0]];
  while (stack.length > 0) {
    const [el, d] = stack.pop()!;
    const p = NS_P.has(el.ns);
    const here = p && CONTAINERS.has(el.local) ? d + 1 : d;
    if (here > depth) depth = here;
    if (p && el.local === "cTn" && isEffectNode(xml, el)) effects++;
    for (const c of el.children) stack.push([c, here]);
  }
  return { depth, effects };
}

function readBuilds(xml: string, bldLst: XmlEl | undefined): BuildLabel[] {
  return (bldLst?.children ?? []).flatMap((b) => {
    const spid = spidOf(attr(xml, b, "spid"));
    if (spid === null) return [];
    return [
      {
        spid,
        group: intOf(attr(xml, b, "grpId")),
        kind: b.local,
        build: attr(xml, b, "build"),
        level: intOf(attr(xml, b, "bldLvl")),
        animBg: truthy(attr(xml, b, "animBg")),
      },
    ];
  });
}

/** Read a slide part's timing and transition. Never throws. */
export function parseTimeline(slideXml: string): SlideTimeline {
  if (utf8Over(slideXml, 0, slideXml.length, TIMING_CAPS.partBytes)) return plain(null, "part-too-large");
  let root: XmlEl;
  try {
    root = scan(slideXml);
  } catch {
    return plain(null, "unreadable");
  }
  try {
    return readSlide(slideXml, root);
  } catch {
    // XML of a shape nothing here expected degrades; it never takes the slide down.
    return plain(null, "unexpected");
  }
}

function readSlide(xml: string, root: XmlEl): SlideTimeline {
  let transitionEl: XmlEl | undefined;
  let timing: XmlEl | undefined;
  for (const c of resolvedChildren(xml, root)) {
    if (transitionEl === undefined && isP(c, "transition")) transitionEl = c;
    if (timing === undefined && isP(c, "timing")) timing = c;
  }
  const transition = transitionEl === undefined ? null : readTransition(xml, transitionEl);
  if (timing === undefined) return plain(transition, null);

  if (utf8Over(xml, timing.start, timing.end, TIMING_CAPS.timingBytes)) return plain(transition, "timing-too-large");
  const m = measure(xml, timing);
  if (m.depth > TIMING_CAPS.depth) return plain(transition, "too-deep");
  if (m.effects > TIMING_CAPS.effects) return plain(transition, "too-many-effects");

  const rootCTn = childrenP(childP(timing, "tnLst"), "par")
    .map((p) => childP(p, "cTn"))
    .find((c) => c !== undefined && attr(xml, c, "nodeType") === "tmRoot");
  const seqs = childrenP(childP(rootCTn, "childTnLst"), "seq")
    .map((s) => childP(s, "cTn"))
    .filter((c): c is XmlEl => c !== undefined);
  const main = seqs.find((c) => attr(xml, c, "nodeType") === "mainSeq");

  const groups = main === undefined ? [] : groupsOf(main);
  if (groups.length > TIMING_CAPS.clicks) return plain(transition, "too-many-clicks");
  let steps: Effect[][];
  try {
    steps = readSteps(xml, groups);
  } catch (e) {
    if (e instanceof ColourTooLarge) return plain(transition, "colour-too-large");
    throw e;
  }
  // A first group whose first effect isn't a click runs on entry: it is step 0.
  // Otherwise nothing runs on entry and every group is a click.
  if (steps[0]?.[0]?.trigger !== "with" && steps[0]?.[0]?.trigger !== "after") steps.unshift([]);

  let tooLarge = false;
  const interactive: InteractiveSequence[] = seqs
    .filter((c) => attr(xml, c, "nodeType") === "interactiveSeq")
    .map((c) => {
      let trigger: string | null = null;
      for (const cond of childrenP(childP(c, "stCondLst"), "cond")) {
        if (attr(xml, cond, "evt") !== "onClick") continue;
        trigger = readTarget(xml, childP(cond, "tgtEl"))?.spid ?? null;
        if (trigger !== null) break;
      }
      let isteps: Effect[][] = [];
      try {
        isteps = readSteps(xml, groupsOf(c));
      } catch (e) {
        if (!(e instanceof ColourTooLarge)) throw e;
        tooLarge = true;
      }
      return { trigger, steps: isteps };
    });
  if (tooLarge) return plain(transition, "colour-too-large");
  // A held `by` chain is bounded too (R21), in every sequence alike: count
  // the colour changes by target and attribute, each sequence on its own.
  for (const seq of [steps, ...interactive.map((i) => i.steps)]) {
    const chains = new Map<string, number>();
    for (const step of seq) {
      for (const e of step) {
        for (const b of e.behaviours) {
          if (b.kind !== "color" || b.by === null || b.target === null) continue;
          const k = `${targetKey(b.target)} ${(b.attrs[0] ?? "").toLowerCase()}`;
          const n = (chains.get(k) ?? 0) + 1;
          if (n > COLOR_CAPS.chain) return plain(transition, "colour-too-large");
          chains.set(k, n);
        }
      }
    }
  }
  // What the states would hold (R21): bounded before any is built.
  let values = 0;
  for (let k = 0; k < steps.length; k++) {
    for (const e of steps[k]!) {
      for (const b of e.behaviours) {
        const t = b.target ?? e.target;
        if (t === null) continue;
        values += (t.para === null ? 1 : Math.max(0, Math.min(t.para[1] - t.para[0] + 1, HELD_PARAGRAPHS))) * (steps.length - k);
        if (values > TIMING_CAPS.values) return plain(transition, "too-many-values");
      }
    }
  }

  return { steps, transition, interactive, builds: readBuilds(xml, childP(timing, "bldLst")), approximate: null };
}

/** What a viewer keeps of a slide part for its timeline (see `extractTiming`). */
export interface TimingExtract {
  /** A slide part that parses to the same timeline as the whole part; null
   *  when `timeline` was read at once instead. */
  source: string | null;
  /** The timeline, when keeping a source would cost more than reading it
   *  now (past a cap, or XML the scanner refuses). */
  timeline: SlideTimeline | null;
  /** The slide is hidden in the show (`show="0"` on the root). */
  hidden: boolean;
}

/** Most UTF-16 units of source kept per slide before it is read at once
 *  instead (the `p:timing` cap, with room for the transition). */
const EXTRACT_MAX = TIMING_CAPS.timingBytes + 64 * 1024;

/**
 * The part of a slide `parseTimeline` reads, cut from the whole part with
 * the scanner's offsets: the root's start tag (its namespace declarations),
 * the root's `p:transition`, `p:timing` and `mc:AlternateContent` children
 * as written, and the root's end tag. `parseTimeline` gives the same result
 * for it as for the whole part, so a viewer can let the shapes' XML go and
 * parse later. A part past the caps is read at once (its timeline is small).
 */
export function extractTiming(slideXml: string): TimingExtract {
  if (utf8Over(slideXml, 0, slideXml.length, TIMING_CAPS.partBytes)) {
    return { source: null, timeline: parseTimeline(slideXml), hidden: false };
  }
  let root: XmlEl;
  try {
    root = scan(slideXml);
  } catch {
    return { source: null, timeline: plain(null, "unreadable"), hidden: false };
  }
  const show = attr(slideXml, root, "show");
  const hidden = show === "0" || show === "false";
  const kept = root.children.filter((c) => isP(c, "transition") || isP(c, "timing") || (c.local === "AlternateContent" && c.ns === NS_MC));
  const size = kept.reduce((n, c) => n + (c.end - c.start), 0);
  if (size > EXTRACT_MAX || root.selfClosing) return { source: null, timeline: parseTimeline(slideXml), hidden };
  const source =
    slideXml.slice(root.start, root.openEnd) +
    kept.map((c) => slideXml.slice(c.start, c.end)).join("") +
    slideXml.slice(root.closeStart, root.end);
  return { source, timeline: null, hidden };
}

// --- build states -------------------------------------------------------------------

/**
 * SMIL acceleration: `accel` and `decel` (100000ths of the duration) turn
 * linear progress `q` (0–1) into eased progress with the same end points.
 */
export function warp(q: number, accel: number, decel: number): number {
  let a = Math.max(0, accel) / 100000;
  let d = Math.max(0, decel) / 100000;
  if (a + d > 1) {
    const s = a + d;
    a /= s;
    d /= s;
  }
  if (a === 0 && d === 0) return q;
  const r = 1 / (1 - a / 2 - d / 2);
  if (q < a) return (r * q * q) / (2 * a);
  if (q <= 1 - d) return r * (q - a / 2);
  const e = 1 - q;
  return 1 - (r * e * e) / (2 * d);
}

/**
 * Progress (0–1) of a timed node `tau` ms after it begins: null before it
 * begins; `repeat` in 1000ths (null: forever), `autoRev` plays each
 * iteration forward then back, acceleration applies to each simple
 * duration. Holds the last value after the end.
 */
export function progress(
  tau: number,
  dur: number,
  repeat: number | null,
  autoRev: boolean,
  accel: number,
  decel: number,
): number | null {
  if (tau < 0) return null;
  if (dur <= 0) return autoRev ? 0 : 1;
  const iter = autoRev ? 2 : 1;
  const total = repeat === null ? Infinity : (dur * iter * repeat) / 1000;
  const s = Math.min(tau, total) / dur;
  if (autoRev) {
    let pos = s % 2;
    if (pos === 0 && s > 0) return total === 0 ? 0 : warp(0, accel, decel);
    if (pos > 1) pos = 2 - pos;
    return warp(pos, accel, decel);
  }
  let pos = s % 1;
  if (pos === 0 && s > 0) pos = 1;
  return warp(pos, accel, decel);
}



// --- time and path math shared with playback (`pptxMotion.ts`) --------------------
// One copy of each: the state a click lands on and the frames that play it
// must agree, or the landing jumps.

/** A behaviour's dur, a set of dur 1 counting 0 (PowerPoint's instant, R10);
 *  null when it never ends. */
export function instantDur(b: Behaviour): number | null {
  if (b.dur === null) return null;
  return b.kind === "set" && b.dur === 1 ? 0 : b.dur;
}

/** The effect's span before its own repeat: its latest finite behaviour
 *  end (R11, in ms as doubles: R25's `S`). */
export function effectSpan(e: Effect): number {
  let span = 0;
  for (const b of e.behaviours) {
    const d = instantDur(b);
    if (d === null || b.repeatCount === null) continue;
    span = Math.max(span, b.delay + (d * b.repeatCount * (b.autoRev ? 2 : 1)) / 1000);
  }
  return span;
}

/** The effect's own clock `tau` ms after it starts (its accel, decel,
 *  repeat and autoRev; R25): null before it starts. */
export function effectTime(e: Effect, tau: number): number | null {
  if (tau < 0) return null;
  const span = effectSpan(e);
  if (span <= 0) return 0;
  if (e.accel === 0 && e.decel === 0 && !e.autoRev && e.repeatCount === 1000) return Math.min(tau, span);
  const q = progress(tau, span, e.repeatCount, e.autoRev, e.accel, e.decel);
  return q === null ? null : q * span;
}

/** A behaviour's own progress (0–1) at its effect's clock `c`: null before
 *  it begins or when it never ends. */
export function behaviourProgress(b: Behaviour, c: number): number | null {
  const d = instantDur(b);
  if (d === null || b.repeatCount === null) return null;
  return progress(c - b.delay, d, b.repeatCount, b.autoRev, b.accel, b.decel);
}

/** Keyframes: progress (0–1) → value, in document order. */
export type Keys = { at: number; v: number }[];

/**
 * `p:anim`'s values as keyframes (R28), or null when it contributes
 * nothing: a `valueType` other than `num`, `calcmode` `fmla`, a `tav`
 * with a formula, a value `read` refuses, a time outside 0–1 or before
 * the one before it. Without `tav`s: `from` (else `cur`, the value now)
 * to `to`, or to `from + by`, or staying at `cur`.
 */
export function animKeys(b: Behaviour & { kind: "anim" }, read: (v: TimeValue | null) => number | null, cur: number): Keys | null {
  if (b.valueType !== null && b.valueType !== "num") return null;
  if (b.calcmode === "fmla") return null;
  if (b.tav.length > 0) {
    const n = b.tav.length;
    const keys: Keys = [];
    for (const [i, tav] of b.tav.entries()) {
      if (tav.fmla !== null && tav.fmla !== "") return null;
      const v = read(tav.val);
      if (v === null) return null;
      const at = tav.tm === null ? (n === 1 ? 0 : i / (n - 1)) : tav.tm / 100000;
      if (!(at >= 0 && at <= 1)) return null;
      if (keys.length > 0 && at < keys[keys.length - 1]!.at) return null;
      keys.push({ at, v });
    }
    return keys;
  }
  const from = b.from === null ? cur : read(b.from);
  if (from === null) return null;
  let to: number | null = cur;
  if (b.to !== null) to = read(b.to);
  else if (b.by !== null) {
    const by = read(b.by);
    to = by === null ? null : from + by;
  }
  return to === null ? null : [
    { at: 0, v: from },
    { at: 1, v: to },
  ];
}

/** The keyframes' value at progress `q` (R28): the first before the
 *  first, the last after the last; between two times, linear (`discrete`:
 *  the earlier); at a time several keys share, the last of them. */
export function keyValue(keys: Keys, q: number, discrete: boolean): number {
  if (q < keys[0]!.at) return keys[0]!.v;
  let i = 0;
  while (i + 1 < keys.length && keys[i + 1]!.at <= q) i++;
  const a = keys[i]!;
  const z = keys[i + 1];
  if (z === undefined || q === a.at || discrete) return a.v;
  return a.v + ((z.v - a.v) * (q - a.at)) / (z.at - a.at);
}

const PATH_ARITY = new Map<string, number>([
  ["M", 2],
  ["L", 2],
  ["C", 6],
]);

/** One piece of a motion path, in absolute coordinates: a move or line to
 *  `to`, a cubic through `c1`, `c2` to `to`, or a close back to `to`. */
export type PathPiece =
  | { kind: "move" | "line" | "close"; from: [number, number]; to: [number, number] }
  | { kind: "curve"; from: [number, number]; c1: [number, number]; c2: [number, number]; to: [number, number] };

/**
 * Walks a PowerPoint motion path from (0, 0) by R19, handing each piece to
 * `visit` (return `false` to stop). Pairs after an `M` beyond the first are
 * line-tos (they don't move the subpath start); `Z` returns to the subpath
 * start; `E` or the end of the string ends the path; lowercase `m`/`l`/`c`
 * are relative; any other command, or a number where none fits, stops the
 * walk. Returns the point reached.
 */
export function walkPath(path: string, visit?: (p: PathPiece) => boolean | void): [number, number] {
  const tokens = path.match(/[A-Za-z]|[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?/g) ?? [];
  let x = 0;
  let y = 0;
  let sx = 0;
  let sy = 0;
  let cmd = "";
  let i = 0;
  while (i < tokens.length) {
    const tok = tokens[i]!;
    if (/^[A-Za-z]$/.test(tok)) {
      i++;
      if (tok === "E" || tok === "e") break;
      if (tok === "Z" || tok === "z") {
        const from: [number, number] = [x, y];
        x = sx;
        y = sy;
        cmd = "";
        if (visit?.({ kind: "close", from, to: [x, y] }) === false) break;
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
    const from: [number, number] = [x, y];
    x = v[arity - 2]! + ox;
    y = v[arity - 1]! + oy;
    let piece: PathPiece;
    if (cmd === "C" || cmd === "c") piece = { kind: "curve", from, c1: [v[0]! + ox, v[1]! + oy], c2: [v[2]! + ox, v[3]! + oy], to: [x, y] };
    else if (cmd === "M" || cmd === "m") {
      piece = { kind: "move", from, to: [x, y] };
      sx = x;
      sy = y;
      cmd = cmd === "M" ? "L" : "l";
    } else piece = { kind: "line", from, to: [x, y] };
    if (visit?.(piece) === false) break;
  }
  return [x, y];
}

/** Where a PowerPoint motion path ends, relative to where it starts (R19). */
export function pathEnd(path: string): [number, number] {
  return walkPath(path);
}

/** To 4 decimals, half away from zero, never −0. */
function round4(n: number): number {
  const r = (Math.sign(n) * Math.round(Math.abs(n) * 1e4)) / 1e4;
  return r === 0 ? 0 : r;
}

interface VisEvent {
  key: string;
  to: Visibility;
  /** Thousandths of a ms from the step's start; null when it never ends. */
  at: bigint | null;
}

function visibilityEvents(effects: readonly Effect[]): VisEvent[] {
  const out: VisEvent[] = [];
  for (const e of effects) {
    for (const b of e.behaviours) {
      // A behaviour without a target of its own changes nothing (README R3).
      const t = b.target;
      if (!isVisibilitySet(b) || (b.to !== "visible" && b.to !== "hidden") || t === null) continue;
      const end = behaviourEnd(b);
      out.push({ key: targetKey(t), to: b.to, at: end === null ? null : BigInt(e.start) * 1000n + end });
    }
  }
  return out;
}

// --- held values (R24-R31) ------------------------------------------------------------

/** The behaviour's own progress (0-1) when its effect has ended, or null
 *  when it never began (R25): the effect's repeat, autoRev and
 *  acceleration first, then the behaviour's own. */
function endProgress(e: Effect, b: Behaviour): number | null {
  if (b.dur === null || b.repeatCount === null || e.repeatCount === null) return null;
  const c = effectTime(e, (effectSpan(e) * e.repeatCount * (e.autoRev ? 2 : 1)) / 1000);
  return behaviourProgress(b, c ?? 0);
}

/** Whether a behaviour's value stays once its effect is over (R24). */
function holds(e: Effect, b: Behaviour): boolean {
  if (e.fill === "remove" || b.repeatCount === null || e.repeatCount === null) return false;
  if (b.dur === null) return b.kind === "set";
  return b.fill === "hold" || b.fill === "freeze";
}

/** A held `p:anim`'s value at progress `q`, from `cur` the value held so
 *  far: the same keyframes and interpolation playback uses (R28), values
 *  read as R2a decimals; null when it contributes nothing. */
function animHeld(b: Behaviour & { kind: "anim" }, q: number, cur: number): number | null {
  const keys = animKeys(b, decimalOf, cur);
  return keys === null ? null : keyValue(keys, q, b.calcmode === "discrete");
}

const ROTATION_ATTRS = new Set(["r", "ppt_r", "style.rotation"]);
const COLOR_PART = new Map<string, keyof HeldColors>([
  ["fillcolor", "fill"],
  ["fill.color", "fill"],
  ["stroke.color", "line"],
  ["style.color", "text"],
]);

/** A change whose result isn't finite is dropped: the value stays (R31). */
function setFinite(m: Map<string, number>, key: string, v: number): void {
  if (Number.isFinite(v)) m.set(key, v);
}

interface Held {
  scale: Map<string, [number, number]>;
  turn: Map<string, number>;
  opacity: Map<string, number>;
  color: Map<string, HeldColors>;
}

/** Most paragraphs one held change on a paragraph range reaches (R31). */
export const HELD_PARAGRAPHS = 256;

/**
 * The shapes whose text the main sequence animates by paragraph: on these
 * PowerPoint draws the text apart from the shape's body, and an effect on
 * the whole shape acts on the body alone (README R32; the `order` deck's
 * film).
 */
export function splitShapes(timeline: SlideTimeline): Set<string> {
  const out = new Set<string>();
  for (const step of timeline.steps) for (const e of step) for (const b of e.behaviours) if (b.target?.para) out.add(b.target.spid);
  return out;
}

/** The keys a held change on `t` is kept under (R31, R32): a paragraph
 *  range, each of its paragraphs; the whole of a split shape, its body. */
function heldKeys(t: Target, split: ReadonlySet<string>): string[] {
  if (t.para !== null) {
    const out: string[] = [];
    for (let n = t.para[0]; n <= t.para[1] && n < t.para[0] + HELD_PARAGRAPHS; n++) out.push(`${t.spid}:p${n}-${n}`);
    return out;
  }
  if (!t.bg && split.has(t.spid)) return [`${t.spid}:bg`];
  return [targetKey(t)];
}

/** One step's held changes, applied in the order they end (as R17). */
function applyHeld(step: readonly Effect[], held: Held, split: ReadonlySet<string>): void {
  const changes: { at: bigint | null; i: number; run: () => void }[] = [];
  let i = 0;
  for (const e of step) {
    for (const b of e.behaviours) {
      const t = b.target;
      if (t === null || !holds(e, b)) continue;
      const q = b.dur === null ? 1 : endProgress(e, b);
      if (q === null) continue;
      const end = behaviourEnd(b);
      const at = end === null ? null : BigInt(e.start) * 1000n + end;
      // A split shape's text never takes a whole-shape change (R32).
      const body = t.para === null && !t.bg && split.has(t.spid);
      const order = i++;
      for (const key of heldKeys(t, split)) {
      let run: (() => void) | null = null;
      if (b.kind === "scale") {
        const f = (p: Point | null, axis: "x" | "y") => (p === null ? null : p[axis] / 100000);
        run = () => {
          const cur = held.scale.get(key) ?? [1, 1];
          const next: [number, number] = [0, 1].map((k) => {
            const axis = k === 0 ? "x" : "y";
            const from = f(b.from, axis) ?? cur[k]!;
            const to = f(b.to, axis);
            if (to !== null) return from + (to - from) * q;
            const by = f(b.by, axis);
            return by === null ? from : from * (1 + (by - 1) * q);
          }) as [number, number];
          if (next.every(Number.isFinite)) held.scale.set(key, next);
        };
      } else if (b.kind === "rot") {
        run = () => {
          const cur = held.turn.get(key) ?? 0;
          if (b.to !== null) {
            const from = b.from === null ? cur : b.from / 60000;
            setFinite(held.turn, key, from + (b.to / 60000 - from) * q);
          } else if (b.by !== null) setFinite(held.turn, key, (b.from === null ? cur : b.from / 60000) + (b.by / 60000) * q);
        };
      } else if (b.kind === "anim" && b.attrs.some((a) => ROTATION_ATTRS.has(a))) {
        run = () => {
          const v = animHeld(b, q, held.turn.get(key) ?? 0);
          if (v !== null) setFinite(held.turn, key, v);
        };
      } else if (b.kind === "anim" && b.attrs.includes("style.opacity")) {
        run = () => {
          const v = animHeld(b, q, held.opacity.get(key) ?? 1);
          if (v !== null) setFinite(held.opacity, key, v);
        };
      } else if (b.kind === "set" && b.attrs.includes("style.opacity")) {
        const v = decimalOf(b.to);
        if (v !== null) run = () => setFinite(held.opacity, key, v);
      } else if (b.kind === "color" && q === 1) {
        const part = b.attrs.map((a) => COLOR_PART.get(a.toLowerCase())).find((x) => x !== undefined);
        if (part !== undefined && !(body && part === "text") && (b.to !== null || b.by !== null)) {
          run = () => {
            const all = held.color.get(key) ?? {};
            const c: HeldColor = { ...(all[part] ?? {}) };
            if (b.to !== null) {
              c.to = b.to;
              delete c.by;
            } else c.by = [...(c.by ?? []), b.by!];
            all[part] = c;
            held.color.set(key, all);
          };
        }
      } else if (b.kind === "set" && q === 1) {
        const a = b.attrs[0] ?? "";
        const v = typeof b.to === "string" ? b.to : b.to === true ? "true" : b.to === false ? "false" : "";
        const part: keyof HeldColors | null = a === "fill.type" || a === "fill.on" ? "fill" : a === "stroke.on" ? "line" : null;
        if (part !== null && (a !== "fill.type" || v === "solid") && (a === "fill.type" || v === "true" || v === "false")) {
          run = () => {
            const all = held.color.get(key) ?? {};
            const c: HeldColor = { ...(all[part] ?? {}) };
            if (a === "fill.type") c.solid = true;
            else c.on = v === "true";
            all[part] = c;
            held.color.set(key, all);
          };
        }
      }
      if (run !== null) changes.push({ at, i: order, run });
      }
    }
  }
  changes.sort((a, b) => {
    if (a.at !== b.at) {
      if (a.at === null) return 1;
      if (b.at === null) return -1;
      return a.at < b.at ? -1 : 1;
    }
    return a.i - b.i;
  });
  for (const c of changes) c.run();
}

/** A colour part in the golden key order. */
function heldColorOut(c: HeldColor): HeldColor {
  const out: HeldColor = {};
  if (c.to !== undefined) out.to = c.to;
  if (c.by !== undefined && c.by.length > 0) out.by = c.by;
  if (c.solid) out.solid = true;
  if (c.on !== undefined) out.on = c.on;
  return out;
}

/** The held values as a state carries them: rounded, identities left out. */
function heldOut(held: Held, state: BuildState): void {
  const sorted = <V>(m: Map<string, V>) => [...m.keys()].sort().map((k) => [k, m.get(k)!] as const);
  // A held value that isn't finite is not held (R31).
  const scale = Object.create(null) as Record<string, [number, number]>;
  for (const [k, [x, y]] of sorted(held.scale)) {
    if (!Number.isFinite(x) || !Number.isFinite(y)) continue;
    const v: [number, number] = [round4(x), round4(y)];
    if (v[0] !== 1 || v[1] !== 1) scale[k] = v;
  }
  const turn = Object.create(null) as Record<string, number>;
  for (const [k, d] of sorted(held.turn)) {
    if (!Number.isFinite(d)) continue;
    const v = round4(((d % 360) + 360) % 360);
    if (v !== 0 && v !== 360) turn[k] = v;
  }
  const opacity = Object.create(null) as Record<string, number>;
  for (const [k, o] of sorted(held.opacity)) {
    if (!Number.isFinite(o)) continue;
    const v = round4(Math.min(1, Math.max(0, o)));
    if (v !== 1) opacity[k] = v;
  }
  const color = Object.create(null) as Record<string, HeldColors>;
  for (const [k, c] of sorted(held.color)) {
    const out: HeldColors = {};
    for (const part of ["fill", "line", "text"] as const) {
      const v = c[part] === undefined ? null : heldColorOut(c[part]);
      if (v !== null && Object.keys(v).length > 0) out[part] = v;
    }
    if (Object.keys(out).length > 0) color[k] = out;
  }
  if (Object.keys(scale).length > 0) state.scale = scale;
  if (Object.keys(turn).length > 0) state.turn = turn;
  if (Object.keys(opacity).length > 0) state.opacity = opacity;
  if (Object.keys(color).length > 0) state.color = color;
}

/** The slide after each step: `states[k]` is after step k. */
export function buildStates(timeline: SlideTimeline): BuildState[] {
  // Initially hidden: targets whose first visibility set, reading the main
  // sequence and then the interactive sequences in document order, is `visible`.
  const first = new Map<string, Visibility>();
  const all = [...timeline.steps, ...timeline.interactive.flatMap((s) => s.steps)];
  for (const step of all) for (const ev of visibilityEvents(step)) if (!first.has(ev.key)) first.set(ev.key, ev.to);
  const hidden = new Set([...first].filter(([, v]) => v === "visible").map(([k]) => k));

  const moved = new Map<string, [number, number]>();
  const held: Held = { scale: new Map(), turn: new Map(), opacity: new Map(), color: new Map() };
  const split = splitShapes(timeline);
  const out: BuildState[] = [];
  for (const step of timeline.steps) {
    // In the order the sets end; one that never ends goes last; ties keep document order.
    const events = visibilityEvents(step)
      .map((ev, i) => ({ ...ev, i }))
      .sort((a, b) => {
        if (a.at !== b.at) {
          if (a.at === null) return 1;
          if (b.at === null) return -1;
          return a.at < b.at ? -1 : 1;
        }
        return a.i - b.i;
      });
    for (const ev of events) {
      if (ev.to === "hidden") hidden.add(ev.key);
      else hidden.delete(ev.key);
    }
    // A path is an offset from the target's layout place, and the path
    // that ends last replaces any before it (R18): PowerPoint's film shows
    // a second path starting from the layout place again.
    const paths: { key: string; at: bigint | null; i: number; end: [number, number] }[] = [];
    for (const e of step) {
      if (e.name !== "path") continue;
      for (const b of e.behaviours) {
        if (b.kind !== "motion" || b.path === null || b.target === null) continue;
        const end = behaviourEnd(b);
        paths.push({ key: targetKey(b.target), at: end === null ? null : BigInt(e.start) * 1000n + end, i: paths.length, end: pathEnd(b.path) });
      }
    }
    paths.sort((a, b) => {
      if (a.at !== b.at) {
        if (a.at === null) return 1;
        if (b.at === null) return -1;
        return a.at < b.at ? -1 : 1;
      }
      return a.i - b.i;
    });
    for (const p of paths) moved.set(p.key, p.end);
    const movedOut = Object.create(null) as Record<string, [number, number]>;
    for (const key of [...moved.keys()].sort()) {
      const [dx, dy] = moved.get(key)!;
      movedOut[key] = [round4(dx), round4(dy)];
    }
    applyHeld(step, held, split);
    const state: BuildState = { hidden: [...hidden].sort(), moved: movedOut };
    heldOut(held, state);
    out.push(state);
  }
  return out;
}

// --- golden form ----------------------------------------------------------------------

export interface GoldenEffect {
  trigger: Trigger;
  start: number;
  dur: number | null;
  class: string | null;
  preset: number;
  subtype: number;
  effect: EffectName;
  dir: Side | null;
  target: string | null;
  sets: Visibility | null;
  path?: string | null;
}

export interface GoldenSlide {
  part: string;
  id: number;
  transition: Omit<Transition, "element"> | null;
  steps: GoldenEffect[][];
  states: BuildState[];
  interactive: number;
  approximate?: Approximate;
}

/** A parsed slide in exactly the golden-file shape (key order included). */
export function goldenSlide(part: string, id: number, timeline: SlideTimeline): GoldenSlide {
  const tr = timeline.transition;
  const out: GoldenSlide = {
    part,
    id,
    transition:
      tr === null
        ? null
        : {
            kind: tr.kind,
            dir: tr.dir,
            dur: tr.dur,
            advanceAfter: tr.advanceAfter,
            advanceOnClick: tr.advanceOnClick,
            morph: tr.morph,
          },
    steps: timeline.steps.map((step) =>
      step.map((e) => {
        const g: GoldenEffect = {
          trigger: e.trigger,
          start: e.start,
          dur: e.dur,
          class: e.presetClass,
          preset: e.preset,
          subtype: e.subtype,
          effect: e.name,
          dir: e.dir,
          target: e.target === null ? null : targetKey(e.target),
          sets: e.sets,
        };
        if (e.name === "path") g.path = e.path;
        return g;
      }),
    ),
    states: buildStates(timeline),
    interactive: timeline.interactive.length,
  };
  if (timeline.approximate !== null) out.approximate = timeline.approximate;
  return out;
}
