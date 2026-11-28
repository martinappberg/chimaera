/**
 * A slide's animation timing and transition, read from its XML, and the
 * slide's state after each build. Pure: no DOM, no Svelte.
 *
 * The contract is `fixtures/pptx-timing/README.md`: what PowerPoint writes,
 * the effect vocabulary, and the golden JSON each corpus deck must parse to.
 * The PowerPoint plugin's Rust parser is held to the same goldens, so a
 * change in what this module returns for the corpus is a contract change.
 *
 * Reads the timing tree as PowerPoint plays it: an effect starts at the sum
 * of the delays on its path below its click group (PowerPoint writes an
 * "after previous" effect's group delay as the end of what came before it).
 * Everything here degrades instead of throwing: XML that doesn't scan, or a
 * tree past the caps, yields a plain slide with an `approximate` reason.
 */

import { attr, decodeEntities, PptxXmlError, scan, type XmlEl } from "./pptxXml";

const NS_P = new Set([
  "http://schemas.openxmlformats.org/presentationml/2006/main",
  "http://purl.oclc.org/ooxml/presentationml/main",
]);
const NS_MC = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const NS_P14 = "http://schemas.microsoft.com/office/powerpoint/2010/main";
const NS_P159 = "http://schemas.microsoft.com/office/powerpoint/2015/09/main";
/** Namespaces whose `mc:Choice` we take: what PowerPoint wraps transitions in. */
const UNDERSTOOD = new Set([NS_P14, NS_P159]);

/** Past any of these the slide plays as a plain slide (`approximate` says why). */
export const TIMING_CAPS = {
  /** Characters of `p:timing` source. */
  xmlChars: 512 * 1024,
  effects: 2000,
  clicks: 300,
  /** Time containers (`p:par`, `p:seq`, `p:excl`) nested in `p:timing`;
   *  PowerPoint's own main sequence nests five. */
  depth: 32,
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

export interface Target {
  /** Shape id (`cNvPr@id`) as written. */
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
  /** Its attributes, as written. */
  attrs: Record<string, string>;
}

interface BehaviourBase {
  target: Target | null;
  /** Offset from the effect's start, ms. */
  delay: number;
  /** One iteration, ms; null for `indefinite`. */
  dur: number | null;
  /** 1000ths of an iteration (1000 = once); Infinity for `indefinite`. */
  repeatCount: number;
  autoRev: boolean;
  /** 1000ths of the duration spent accelerating / decelerating. */
  accel: number;
  decel: number;
  additive: string | null;
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
  /** ms; null when nothing in it ends (every behaviour `indefinite`). */
  dur: number | null;
  presetClass: string | null;
  preset: number;
  subtype: number;
  name: EffectName;
  /** For fly and wipe: the side, from the subtype. */
  dir: Side | null;
  target: Target | null;
  /** What it leaves the target's visibility at (its last `style.visibility` set). */
  sets: Visibility | null;
  /** The motion path, on `path` effects. */
  path: string | null;
  /** `grpId`: ties the effect to its `p:bldLst` label. */
  group: number | null;
  /** The effect node's own timing modifiers. */
  accel: number;
  decel: number;
  autoRev: boolean;
  repeatCount: number;
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
  approximate: string | null;
}

export interface BuildState {
  /** Target keys not on screen, sorted. */
  hidden: string[];
  /** Target key → summed path offset [dx, dy] in slide fractions, keys sorted. */
  moved: Record<string, [number, number]>;
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
function text(xml: string, el: XmlEl): string {
  return decodeEntities(xml.slice(el.openEnd, el.closeStart)).trim();
}
function intOf(v: string | null): number | null {
  if (v === null || !/^\s*-?\d+\s*$/.test(v)) return null;
  return Number(v.trim());
}
function numOf(v: string | null): number | null {
  if (v === null || v.trim() === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}
function truthy(v: string | null): boolean {
  return v === "1" || v === "true";
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

const ENTR_EXIT: Record<number, string> = {
  1: "appear",
  2: "fly",
  3: "blinds",
  4: "box",
  6: "circle",
  9: "dissolve",
  10: "fade",
  16: "split",
  21: "wheel",
  22: "wipe",
  23: "zoom",
  42: "float",
};
const EMPH: Record<number, EffectName> = {
  1: "fill-color",
  6: "grow-shrink",
  8: "spin",
  9: "transparency",
  24: "darken",
  32: "teeter",
};
const SIDES: Record<number, Side> = { 1: "top", 2: "right", 4: "bottom", 8: "left" };

/** The vocabulary name for a preset (`other` outside it). */
export function effectName(presetClass: string | null, preset: number): EffectName {
  if (presetClass === "path") return "path";
  if (presetClass === "emph") return EMPH[preset] ?? "other";
  if (presetClass !== "entr" && presetClass !== "exit") return "other";
  const base = ENTR_EXIT[preset];
  if (base === undefined) return "other";
  if (base === "appear") return presetClass === "entr" ? "appear" : "disappear";
  return `${base}-${presetClass === "entr" ? "in" : "out"}` as EffectName;
}

function effectDir(presetClass: string | null, preset: number, subtype: number): Side | null {
  if (presetClass !== "entr" && presetClass !== "exit") return null;
  if (preset !== 2 && preset !== 22) return null;
  return SIDES[subtype] ?? null;
}

// --- targets ---------------------------------------------------------------------

/** A target as a golden-file key: `"4"`, `"4:bg"`, `"4:p1-1"`. */
export function targetKey(t: Target): string {
  if (t.bg) return `${t.spid}:bg`;
  if (t.para !== null) return `${t.spid}:p${t.para[0]}-${t.para[1]}`;
  return t.spid;
}

function readTarget(xml: string, tgtEl: XmlEl | undefined): Target | null {
  if (tgtEl === undefined) return null;
  const sp = childP(tgtEl, "spTgt") ?? childP(tgtEl, "inkTgt");
  if (sp === undefined) return null;
  const spid = attr(xml, sp, "spid");
  if (spid === null) return null;
  const tx = childP(sp, "txEl");
  const pRg = childP(tx, "pRg");
  let para: [number, number] | null = null;
  if (pRg !== undefined) {
    const st = intOf(attr(xml, pRg, "st"));
    const end = intOf(attr(xml, pRg, "end"));
    if (st !== null && end !== null) para = [st, end];
  }
  // A character range (`charRg`), sub-shape and chart element animate within
  // the shape; until playback models them they target the whole shape.
  return { spid, bg: childP(sp, "bg") !== undefined, para };
}

// --- timing ----------------------------------------------------------------------

/** A time node's start delay: its first plain `p:cond delay` (no event). */
function condDelay(xml: string, cTn: XmlEl): number {
  for (const c of childrenP(childP(cTn, "stCondLst"), "cond")) {
    if (attr(xml, c, "evt") !== null) continue;
    const d = intOf(attr(xml, c, "delay"));
    if (d !== null) return Math.max(0, d);
  }
  return 0;
}

interface NodeTiming {
  delay: number;
  dur: number | null;
  repeatCount: number;
  autoRev: boolean;
  accel: number;
  decel: number;
}

function nodeTiming(xml: string, cTn: XmlEl | undefined): NodeTiming {
  if (cTn === undefined) return { delay: 0, dur: 0, repeatCount: 1000, autoRev: false, accel: 0, decel: 0 };
  const durRaw = attr(xml, cTn, "dur");
  const repRaw = attr(xml, cTn, "repeatCount");
  return {
    delay: condDelay(xml, cTn),
    dur: durRaw === "indefinite" ? null : Math.max(0, intOf(durRaw) ?? 0),
    repeatCount: repRaw === "indefinite" ? Number.POSITIVE_INFINITY : Math.max(0, intOf(repRaw) ?? 1000),
    autoRev: truthy(attr(xml, cTn, "autoRev")),
    accel: intOf(attr(xml, cTn, "accel")) ?? 0,
    decel: intOf(attr(xml, cTn, "decel")) ?? 0,
  };
}

/** How long a node runs once its delay has passed; null when it never ends. */
function activeSpan(t: { dur: number | null; repeatCount: number; autoRev: boolean }): number | null {
  if (t.dur === null || !Number.isFinite(t.repeatCount)) return null;
  return Math.round(t.dur * (t.repeatCount / 1000) * (t.autoRev ? 2 : 1));
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

function color(xml: string, el: XmlEl | undefined): Color | null {
  const c = el?.children[0];
  if (c === undefined) return null;
  const attrs: Record<string, string> = {};
  for (const a of c.attrs) attrs[a.qname] = decodeEntities(xml.slice(a.valueStart, a.valueEnd));
  return { type: c.local, attrs };
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

/** When a behaviour ends, relative to its effect. A `set`'s 1 ms is
 *  PowerPoint's "instant" (an appear lasts nothing, and the effect after it
 *  starts at 0), so it ends where it starts. */
function behaviourEnd(b: Behaviour): number | null {
  if (b.kind === "set" && b.dur === 1) return b.delay;
  const span = activeSpan(b);
  return span === null ? null : b.delay + span;
}

const NODE_TRIGGERS: Record<string, Trigger> = {
  clickEffect: "click",
  withEffect: "with",
  afterEffect: "after",
};

function isEffectNode(xml: string, cTn: XmlEl): boolean {
  const nodeType = attr(xml, cTn, "nodeType");
  return (nodeType !== null && nodeType in NODE_TRIGGERS) || attr(xml, cTn, "presetClass") !== null;
}

function readEffect(xml: string, cTn: XmlEl, start: number, first: boolean): Effect {
  const presetClass = attr(xml, cTn, "presetClass");
  const preset = intOf(attr(xml, cTn, "presetID")) ?? 0;
  const subtype = intOf(attr(xml, cTn, "presetSubtype")) ?? 0;
  const nodeType = attr(xml, cTn, "nodeType");
  const behaviours: Behaviour[] = [];
  readBehaviours(xml, childP(cTn, "childTnLst"), 0, behaviours);

  let span: number | null = 0;
  let finite = behaviours.length === 0;
  for (const b of behaviours) {
    const end = behaviourEnd(b);
    if (end === null) continue;
    finite = true;
    span = Math.max(span ?? 0, end);
  }
  const t = nodeTiming(xml, cTn);
  const dur = finite ? activeSpan({ dur: span, repeatCount: t.repeatCount, autoRev: t.autoRev }) : null;

  // The visibility the effect leaves: its last set in time (ties: document order).
  let sets: Visibility | null = null;
  let setAt = Number.NEGATIVE_INFINITY;
  for (const b of behaviours) {
    if (!isVisibilitySet(b) || b.delay < setAt) continue;
    if (b.to === "visible" || b.to === "hidden") {
      sets = b.to;
      setAt = b.delay;
    }
  }
  const motion = behaviours.find((b) => b.kind === "motion");
  const name = effectName(presetClass, preset);
  return {
    trigger: (nodeType === null ? undefined : NODE_TRIGGERS[nodeType]) ?? (first ? "click" : "with"),
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

// --- transition ------------------------------------------------------------------

const SPEED: Record<string, number> = { fast: 500, med: 750, slow: 1000 };
const IGNORED_TRANSITION_CHILDREN = new Set(["sndAc", "extLst"]);

function readTransition(xml: string, el: XmlEl): Transition {
  const p14dur = intOf(attrNs(xml, el, NS_P14, "dur"));
  const spd = attr(xml, el, "spd") ?? "fast";
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
    dur: p14dur !== null && p14dur >= 0 ? p14dur : (SPEED[spd] ?? 500),
    advanceAfter: intOf(attr(xml, el, "advTm")),
    advanceOnClick: !(advClick === "0" || advClick === "false"),
    morph,
    element: effect?.local ?? null,
  };
}

// --- the slide ----------------------------------------------------------------------

function plain(transition: Transition | null, approximate: string | null): SlideTimeline {
  return { steps: [[]], transition, interactive: [], builds: [], approximate };
}

/** Time-node nesting (`p:par`/`p:seq`/`p:excl`) and effect count below
 *  `timing`, without recursion. */
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
    const spid = attr(xml, b, "spid");
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
  let root: XmlEl;
  try {
    root = scan(slideXml);
  } catch (e) {
    return plain(null, `unreadable XML${e instanceof PptxXmlError ? ` (${e.message})` : ""}`);
  }
  try {
    return readSlide(slideXml, root);
  } catch (e) {
    // XML of a shape nothing here expected degrades; it never takes the slide down.
    return plain(null, `unexpected XML (${e instanceof Error ? e.message : String(e)})`);
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

  if (timing.end - timing.start > TIMING_CAPS.xmlChars) return plain(transition, "timing XML over 512 KB");
  const m = measure(xml, timing);
  if (m.depth > TIMING_CAPS.depth) return plain(transition, `timing nested deeper than ${TIMING_CAPS.depth}`);
  if (m.effects > TIMING_CAPS.effects) return plain(transition, `more than ${TIMING_CAPS.effects} effects`);

  const rootCTn = childrenP(childP(timing, "tnLst"), "par")
    .map((p) => childP(p, "cTn"))
    .find((c) => c !== undefined && attr(xml, c, "nodeType") === "tmRoot");
  const seqs = childrenP(childP(rootCTn, "childTnLst"), "seq")
    .map((s) => childP(s, "cTn"))
    .filter((c): c is XmlEl => c !== undefined);
  const main = seqs.find((c) => attr(xml, c, "nodeType") === "mainSeq");

  const steps: Effect[][] = (main === undefined ? [] : groupsOf(main)).map((g) => readGroup(xml, g));
  // A first group that doesn't open with a click runs on entry: it is step 0.
  // Otherwise nothing runs on entry and every group is a click.
  const first = steps[0]?.[0];
  if (first === undefined || first.trigger === "click") steps.unshift([]);
  if (steps.length - 1 > TIMING_CAPS.clicks) return plain(transition, `more than ${TIMING_CAPS.clicks} clicks`);

  const interactive: InteractiveSequence[] = seqs
    .filter((c) => attr(xml, c, "nodeType") === "interactiveSeq")
    .map((c) => {
      let trigger: string | null = null;
      for (const cond of childrenP(childP(c, "stCondLst"), "cond")) {
        if (attr(xml, cond, "evt") !== "onClick") continue;
        trigger = readTarget(xml, childP(cond, "tgtEl"))?.spid ?? null;
        if (trigger !== null) break;
      }
      return { trigger, steps: groupsOf(c).map((g) => readGroup(xml, g)) };
    });

  return { steps, transition, interactive, builds: readBuilds(xml, childP(timing, "bldLst")), approximate: null };
}

// --- build states -------------------------------------------------------------------

/** Where a PowerPoint motion path ends, relative to where it starts. */
export function pathEnd(path: string): [number, number] {
  const tokens = path.match(/[A-Za-z]|-?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?/g) ?? [];
  let x = 0;
  let y = 0;
  let sx = 0;
  let sy = 0;
  let cmd = "";
  let i = 0;
  const nums = (n: number): number[] | null => {
    const out: number[] = [];
    for (let k = 0; k < n; k++) {
      const v = Number(tokens[i + k]);
      if (tokens[i + k] === undefined || !Number.isFinite(v)) return null;
      out.push(v);
    }
    i += n;
    return out;
  };
  while (i < tokens.length) {
    const tok = tokens[i]!;
    if (/[A-Za-z]/.test(tok)) {
      cmd = tok;
      i++;
      if (cmd === "Z" || cmd === "z") {
        x = sx;
        y = sy;
      }
      if (cmd === "E" || cmd === "e") break;
      continue;
    }
    const rel = cmd === cmd.toLowerCase();
    const arity = { M: 2, L: 2, C: 6 }[cmd.toUpperCase()];
    const v = arity === undefined ? null : nums(arity);
    if (v === null) break;
    const nx = v[v.length - 2]! + (rel ? x : 0);
    const ny = v[v.length - 1]! + (rel ? y : 0);
    x = nx;
    y = ny;
    if (cmd === "M" || cmd === "m") {
      sx = x;
      sy = y;
    }
  }
  return [x, y];
}

function round4(n: number): number {
  return Math.round(n * 1e4) / 1e4 + 0;
}

function visibilityEvents(effects: readonly Effect[]): { key: string; to: Visibility; at: number }[] {
  const out: { key: string; to: Visibility; at: number }[] = [];
  for (const e of effects) {
    for (const b of e.behaviours) {
      if (!isVisibilitySet(b) || (b.to !== "visible" && b.to !== "hidden")) continue;
      const t = b.target ?? e.target;
      if (t !== null) out.push({ key: targetKey(t), to: b.to, at: e.start + b.delay });
    }
  }
  return out;
}

/** Targets whose first visibility set (document order) is `visible`. */
function enteringTargets(steps: readonly Effect[][]): Set<string> {
  const first = new Map<string, Visibility>();
  for (const step of steps) for (const ev of visibilityEvents(step)) if (!first.has(ev.key)) first.set(ev.key, ev.to);
  return new Set([...first].filter(([, v]) => v === "visible").map(([k]) => k));
}

/** The slide after each step: `states[k]` is after step k. */
export function buildStates(timeline: SlideTimeline): BuildState[] {
  const always = new Set<string>();
  for (const seq of timeline.interactive) for (const k of enteringTargets(seq.steps)) always.add(k);
  const hidden = enteringTargets(timeline.steps);
  const moved = new Map<string, [number, number]>();
  const out: BuildState[] = [];
  for (const step of timeline.steps) {
    const events = visibilityEvents(step)
      .map((ev, i) => ({ ...ev, i }))
      .sort((a, b) => a.at - b.at || a.i - b.i);
    for (const ev of events) {
      if (ev.to === "hidden") hidden.add(ev.key);
      else hidden.delete(ev.key);
    }
    for (const e of step) {
      if (e.name !== "path") continue;
      for (const b of e.behaviours) {
        if (b.kind !== "motion" || b.path === null) continue;
        const t = b.target ?? e.target;
        if (t === null) continue;
        const key = targetKey(t);
        const [dx, dy] = pathEnd(b.path);
        const was = moved.get(key) ?? [0, 0];
        moved.set(key, [was[0] + dx, was[1] + dy]);
      }
    }
    const movedOut: Record<string, [number, number]> = {};
    for (const key of [...moved.keys()].sort()) {
      const [dx, dy] = moved.get(key)!;
      movedOut[key] = [round4(dx), round4(dy)];
    }
    out.push({ hidden: [...new Set([...hidden, ...always])].sort(), moved: movedOut });
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
  approximate?: string;
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
