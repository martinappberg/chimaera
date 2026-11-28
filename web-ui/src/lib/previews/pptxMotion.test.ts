import JSZip from "jszip";
import { describe, expect, it } from "vitest";
import { clipMargin, clipRegion, effectMotion, motionNotes, pacedSampler, pathPolyline, progress, warp, type Geom } from "./pptxMotion";
import { entryState, MAX_FRAMES, PLAY_BUDGET, planStep, type ElementBase, type ShapeInfo } from "./pptxPlay";
import { buildStates, parseTimeline, pathEnd, type Effect, type SlideTimeline } from "./pptxTiming";

/**
 * What a click plays, built from the behaviours PowerPoint wrote in the
 * timing corpus (`fixtures/pptx-timing/`): the keyframes each effect
 * yields on a shape whose place the test chooses.
 */

type NodeFs = { readFileSync(path: URL): Uint8Array };
const fs = (globalThis as unknown as { process: { getBuiltinModule(id: "node:fs"): NodeFs } }).process.getBuiltinModule("node:fs");
const CORPUS = new URL("./fixtures/pptx-timing/", import.meta.url);

async function slide(deck: string, n: number): Promise<SlideTimeline> {
  const zip = await JSZip.loadAsync(fs.readFileSync(new URL(`decks/${deck}.pptx`, CORPUS)));
  return parseTimeline(await zip.file(`ppt/slides/slide${n}.xml`)!.async("string"));
}
function caseTimeline(name: string): SlideTimeline {
  return parseTimeline(new TextDecoder().decode(fs.readFileSync(new URL(`cases/${name}.xml`, CORPUS))));
}

const W = 1280;
const H = 720;
/** A shape centred at (0.5, 0.5), a tenth of the slide wide, a fifth tall. */
const G: Geom = { ppt_x: 0.5, ppt_y: 0.5, ppt_w: 0.1, ppt_h: 0.2 };
const base = (g = G): ElementBase => ({ geom: g, k: 1, tx: 0, ty: 0, drawn: 1, held: 1, sx: 1, sy: 1, rot: 0, bw: g.ppt_w * W, bh: g.ppt_h * H });
const noLanding = () => null;

/** Plan one effect alone on a shape with `G`'s place, from the slide's entry state. */
function planOne(e: Effect, before = { hidden: e.sets === "visible" && e.target !== null ? [e.target.spid] : [], moved: {} }) {
  const shapes = new Map<string, ShapeInfo>([[e.target!.spid, { paraCount: 0, paras: false }]]);
  return planStep([{ ...e, start: 0 }], before, shapes, [e.target!.spid], () => base(), noLanding, W, H);
}
/** A polygon's points. */
const pts = (s: string) => [...s.matchAll(/(-?[0-9.]+)px (-?[0-9.]+)px/g)].map((m) => [Number(m[1]), Number(m[2])] as const);
const px = (s: string | undefined): [number, number] => {
  const m = /^(-?[0-9.]+)px (-?[0-9.]+)px$/.exec(s ?? "")!;
  return [Number(m[1]), Number(m[2])];
};

describe("time", () => {
  it("accel and decel ease without moving the ends", () => {
    expect(warp(0, 50000, 50000)).toBe(0);
    expect(warp(1, 50000, 50000)).toBeCloseTo(1);
    expect(warp(0.5, 50000, 50000)).toBeCloseTo(0.5);
    expect(warp(0.1, 50000, 50000)).toBeLessThan(0.1);
    expect(warp(0.9, 50000, 50000)).toBeGreaterThan(0.9);
  });

  it("autoRev plays forward then back to the start; repeats count in 1000ths", () => {
    expect(progress(-1, 500, 1000, false, 0, 0)).toBe(null);
    expect(progress(250, 500, 1000, true, 0, 0)).toBeCloseTo(0.5);
    expect(progress(500, 500, 1000, true, 0, 0)).toBeCloseTo(1);
    expect(progress(750, 500, 1000, true, 0, 0)).toBeCloseTo(0.5);
    expect(progress(1000, 500, 1000, true, 0, 0)).toBe(0);
    expect(progress(5000, 500, 1000, true, 0, 0)).toBe(0);
    expect(progress(5000, 500, 1000, false, 0, 0)).toBe(1);
    expect(progress(5000, 500, 1500, false, 0, 0)).toBeCloseTo(0.5);
  });

  it("an autoRev effect (repeat-autorev case, grow 3× there and back) ends where it began", () => {
    const grow = caseTimeline("repeat-autorev").steps[3]![0]!;
    const p = planOne(grow);
    const kf = p.tracks[0]!.keyframes;
    expect(p.duration).toBe(3000);
    expect(kf[kf.length - 1]!.scale).toBe("1 1");
    expect(Math.max(...kf.map((k) => Number(k.scale!.split(" ")[0])))).toBeCloseTo(1.5, 1);
  });
});

describe("fly (entrances slide 2): off the slide on its side, then home", async () => {
  const t = await slide("entrances", 2);
  const sides = new Map([[1, "top"], [2, "right"], [4, "bottom"], [8, "left"]]);
  for (const e of t.steps.flat()) {
    it(`from the ${sides.get(e.subtype)}`, () => {
      const kf = planOne(e).tracks[0]!.keyframes;
      const [dx, dy] = px(kf[0]!.translate);
      const left = G.ppt_x - G.ppt_w / 2 + dx / W;
      const top = G.ppt_y - G.ppt_h / 2 + dy / H;
      const side = sides.get(e.subtype);
      if (side === "left") expect(left + G.ppt_w).toBeLessThanOrEqual(0.0001);
      if (side === "right") expect(left).toBeGreaterThanOrEqual(0.9999);
      if (side === "top") expect(top + G.ppt_h).toBeLessThanOrEqual(0.0001);
      if (side === "bottom") expect(top).toBeGreaterThanOrEqual(0.9999);
      expect(kf[kf.length - 1]!.translate).toBe("0px 0px");
      // Shown from its first frame, as PowerPoint's fly is (it comes from off the slide).
      expect(kf[1]!.opacity).toBe(1);
    });
  }
});

describe("wipe", () => {
  it("names the side that shows first: right reveals from the right edge", () => {
    const r = clipRegion("wipe(right)", false)!;
    // The region reaches a margin past the box (a shadow comes with the shape).
    const e = clipMargin(100, 50);
    const xs = pts(r(0.25, 100, 50)).map(([x]) => x);
    expect(Math.max(...xs)).toBe(100 + e);
    expect(Math.min(...xs)).toBeCloseTo(0.75 * (100 + 2 * e) - e, 1);
    expect(r(0, 100, 50)).toMatch(/^polygon\(/);
    expect(Math.min(...pts(r(0, 100, 50)).map(([x]) => x))).toBe(100 + e);
    expect(r(1, 100, 50)).toBe("none");
  });

  it("from bottom (subtype 4) is wipe(down): the bottom shows first", async () => {
    const e = (await slide("entrances", 3)).steps.flat().find((x) => x.subtype === 4)!;
    const f = e.behaviours.find((b) => b.kind === "effect");
    expect(f?.kind === "effect" && f.filter).toBe("wipe(down)");
    const kf = planOne(e).tracks[0]!.keyframes;
    const mid = kf.find((k) => k.offset >= 0.25)!;
    // The visible band's top edge rises from the bottom (h = 144 px) towards 0.
    const ys = pts(mid.clipPath!).map(([, y]) => y);
    expect(Math.max(...ys)).toBe(144 + clipMargin(128, 144));
    expect(Math.min(...ys)).toBeGreaterThan(72);
    expect(kf[kf.length - 1]!.clipPath).toBe("none");
  });

  it("going out hides from the same side (exits-emphasis: wipe(up) out)", () => {
    const r = clipRegion("wipe(up)", true)!;
    const e = clipMargin(100, 40);
    const ys = pts(r(0.25, 100, 40)).map(([, y]) => y);
    expect(Math.min(...ys)).toBeCloseTo(0.25 * (40 + 2 * e) - e, 1);
    expect(Math.max(...ys)).toBe(40 + e);
  });

  it("box, circle, split and the rest keep one shape all the way, and close or open", () => {
    for (const f of ["box(in)", "box(out)", "circle(in)", "circle(out)", "diamond(in)", "plus(out)", "barn(inVertical)", "barn(outHorizontal)", "blinds(horizontal)", "blinds(vertical)", "wheel(1)", "wheel(4)"]) {
      for (const out of [false, true]) {
        const r = clipRegion(f, out)!;
        expect(r, f).not.toBe(null);
        const n = (p: number) => r(p, 200, 100).split(",").length;
        expect(n(0.2), f).toBe(n(0.7));
      }
    }
    // In: the box less a shape shrinking to the centre (even-odd).
    expect(clipRegion("box(in)", false)!(0.5, 200, 100)).toMatch(/^polygon\(evenodd, /);
    // Out: a box growing from the centre (half the margin-wide box at 0.5).
    expect(clipMargin(200, 100)).toBe(22);
    expect(clipRegion("box(out)", false)!(0.5, 200, 100)).toBe("polygon(39px 50px, 39px 14px, 161px 14px, 161px 86px, 39px 86px)");
  });

  it("refuses what it doesn't draw (the effect fades instead)", () => {
    for (const f of ["dissolve", "checkerboard(across)", "randombar(horizontal)", "wipe(sideways)", "box(weird)", "wheel(99)", "x(y)(z)"]) {
      expect(clipRegion(f, false), f).toBe(null);
    }
  });
});

describe("motion paths (paths slide 1)", async () => {
  const t = await slide("paths", 1);
  it("each path's polyline ends where the contract's walker does (R19)", () => {
    const paths = [
      ...t.steps.flat().map((e) => e.path!),
      "M 0 0 0.1 0.1 Z",
      "M 0 0 L 0.1 0.2 X 5 5 L 1 1 E",
      "M 0 0 L 0.00025 -0.00025",
      "m 0 0 l 0.1 0 l 0 0.1 e",
      "M 0.1 0.1 L 0.2 0.2 M 0.5 0.5 L 0.6 0.5 Z",
      "c 0.1 0 0.1 0.1 0.2 0.1 l 0.1 0",
      "",
      "E",
    ];
    for (const p of paths) {
      const line = pathPolyline(p)!;
      const end = line.pts[line.pts.length - 1]!;
      const want = pathEnd(p);
      expect(end[0], p).toBeCloseTo(want[0], 12);
      expect(end[1], p).toBeCloseTo(want[1], 12);
      expect(pacedSampler(line)(1)).toEqual(end);
    }
  });

  it("the shape follows its path and stops at R19's end point", () => {
    for (const e of t.steps.flat()) {
      const kf = planOne(e).tracks[0]!.keyframes;
      const [ex, ey] = pathEnd(e.path!);
      const [x, y] = px(kf[kf.length - 1]!.translate);
      expect(x).toBeCloseTo(ex * W, 1);
      expect(y).toBeCloseTo(ey * H, 1);
      // About 60 frames a second over its 2 s, each a real step along the way.
      expect(kf.length).toBeGreaterThan(100);
      expect(new Set(kf.map((k) => k.translate)).size).toBeGreaterThan(60);
    }
  });

  it("a curve is sampled, not cut straight: the arc rises above the line", () => {
    const arc = t.steps.flat().find((e) => e.path!.includes("C 0.081"))!;
    const ys = planOne(arc).tracks[0]!.keyframes.map((k) => px(k.translate)[1]);
    expect(Math.min(...ys)).toBeLessThan(-0.05 * H);
  });

  it("paths that add up: the second starts where the state before left it", async () => {
    const t2 = await slide("paths", 2);
    const states = buildStates(t2);
    const shapes = new Map<string, ShapeInfo>([[t2.steps[2]![0]!.target!.spid, { paraCount: 0, paras: false }]]);
    const before = states[1]!;
    const moved = Object.values(before.moved)[0] ?? [0, 0];
    const b = { ...base(), tx: moved[0] * W, ty: moved[1] * H };
    const kf = planStep(t2.steps[2]!, before, shapes, [...shapes.keys()], () => b, noLanding, W, H).tracks[0]!.keyframes;
    expect(px(kf[0]!.translate)).toEqual([moved[0] * W, moved[1] * H]);
    const end = states[2]!.moved[t2.steps[2]![0]!.target!.spid]!;
    expect(px(kf[kf.length - 1]!.translate)[0]).toBeCloseTo(end[0] * W, 0);
    expect(px(kf[kf.length - 1]!.translate)[1]).toBeCloseTo(end[1] * H, 0);
  });
});

describe("held emphasis plays from what the state before holds", () => {
  it("a second grow starts at 150% and ends at 225%; shrink to 50% is of the shape's own size", async () => {
    const t = await slide("emphasis", 1);
    const shapes = new Map<string, ShapeInfo>([["2", { paraCount: 0, paras: false }], ["3", { paraCount: 0, paras: false }]]);
    const states = buildStates(t);
    const grown = { ...base(), sx: 1.5, sy: 1.5 };
    const kf = planStep(t.steps[2]!, states[1]!, shapes, ["2"], () => grown, noLanding, W, H).tracks[0]!.keyframes;
    expect(kf[0]!.scale).toBe("1.5 1.5");
    expect(kf[kf.length - 1]!.scale).toBe("2.25 2.25");
    expect(states[2]!.scale!["2"]).toEqual([2.25, 2.25]);
    const shrink = planStep(t.steps[4]!, states[3]!, shapes, ["3"], () => grown, noLanding, W, H).tracks[0]!.keyframes;
    expect(shrink[0]!.scale).toBe("1.5 1.5");
    expect(shrink[shrink.length - 1]!.scale).toBe("0.5 0.5");
  });

  it("the last frame is the landed value exactly, and a whole turn ends without spinning back", async () => {
    const spin = (await slide("exits-emphasis", 2)).steps[2]![0]!;
    const shapes = new Map<string, ShapeInfo>([[spin.target!.spid, { paraCount: 0, paras: false }]]);
    const landed = { opacity: 1, translate: "none", scale: "none", rotate: "none" };
    const kf = planStep([spin], { hidden: [], moved: {} }, shapes, [spin.target!.spid], () => base(), () => landed, W, H).tracks[0]!.keyframes;
    // 0 → 360 becomes −360 → 0, so the frame before the last is close to it.
    expect(kf[0]!.rotate).toBe("-360deg");
    expect(kf[kf.length - 1]!.rotate).toBe("none");
    expect(parseFloat(kf[kf.length - 2]!.rotate!)).toBeGreaterThan(-10);
    const fly = (await slide("entrances", 2)).steps[1]![0]!;
    const f = planOne(fly).tracks[0]!.keyframes;
    const g = planStep([fly], { hidden: [fly.target!.spid], moved: {} }, shapes.set(fly.target!.spid, { paraCount: 0, paras: false }), [fly.target!.spid], () => base(), () => landed, W, H).tracks[0]!.keyframes;
    expect(f[f.length - 1]!.translate).toBe("0px 0px");
    expect(g[g.length - 1]!.translate).toBe("none");
    expect(g[g.length - 1]!.opacity).toBe(1);
  });
});

describe("zoom, spin, teeter, boomerang: from the behaviours", () => {
  it("zoom in grows from nothing about its centre", async () => {
    const zoom = (await slide("entrances", 1)).steps[3]![0]!;
    expect(zoom.name).toBe("zoom-in");
    const kf = planOne(zoom).tracks[0]!.keyframes;
    expect(kf[0]!.scale).toBe("0 0");
    expect(kf[kf.length - 1]!.scale).toBe("1 1");
    expect(kf[0]!.translate).toBeUndefined();
  });

  it("spin turns a full circle; teeter rocks and comes back", async () => {
    const s = (await slide("exits-emphasis", 2)).steps;
    const spin = planOne(s[2]![0]!).tracks[0]!.keyframes;
    expect(spin[spin.length - 1]!.rotate).toBe("360deg");
    const teeter = planOne(s[4]![0]!).tracks[0]!.keyframes;
    const deg = teeter.map((k) => parseFloat(k.rotate!));
    expect(Math.max(...deg)).toBeCloseTo(2, 0);
    expect(Math.min(...deg)).toBeCloseTo(-2, 0);
    expect(deg[deg.length - 1]).toBeCloseTo(0);
  });

  it("boomerang (outside the vocabulary) still plays from its behaviours", async () => {
    const boom = (await slide("entrances", 4)).steps[6]![0]!;
    expect(boom.name).toBe("other");
    const m = effectMotion(boom, () => G);
    expect(m.note).toBe(null);
    const kf = planOne(boom).tracks[0]!.keyframes;
    expect(kf[0]!.rotate).toBe("-90deg");
    expect(kf[kf.length - 1]!.rotate).toBe("0deg");
    expect(Math.min(...kf.map((k) => Number(k.scale!.split(" ")[0])))).toBeLessThan(0.1);
    expect(kf[kf.length - 1]!.translate).toBe("0px 0px");
  });
});

describe("fallbacks", () => {
  it("dissolve fades and says so; transparency (never ends) plays nothing", async () => {
    const dissolve = (await slide("entrances", 1)).steps[5]![0]!;
    const m = effectMotion(dissolve, () => G);
    expect(m.note).toMatch(/dissolve-in: plays as a fade/);
    expect(m.segments.map((s) => s.ch)).toEqual(["op"]);
    const transparency = (await slide("exits-emphasis", 2)).steps[3]![0]!;
    expect(effectMotion(transparency, () => G).segments).toEqual([]);
  });

  it("an entrance it can't read becomes a fade; an emphasis plays nothing", () => {
    const e: Effect = {
      trigger: "click", start: 0, dur: 500, presetClass: "entr", preset: 99, subtype: 0, name: "other", dir: null,
      target: { spid: "2", bg: false, para: null }, sets: "visible", path: null, group: null, accel: 0, decel: 0, autoRev: false, repeatCount: 1000, fill: null,
      behaviours: [
        { kind: "set", to: "visible", target: { spid: "2", bg: false, para: null }, delay: 0, dur: 1, repeatCount: 1000, autoRev: false, accel: 0, decel: 0, additive: null, fill: null, attrs: ["style.visibility"] },
        { kind: "anim", calcmode: "lin", valueType: "num", by: null, from: null, to: null, tav: [{ tm: 0, fmla: null, val: "sin(#ppt_x)" }, { tm: 100000, fmla: null, val: "#ppt_x" }],
          target: { spid: "2", bg: false, para: null }, delay: 0, dur: 500, repeatCount: 1000, autoRev: false, accel: 0, decel: 0, additive: null, fill: null, attrs: ["ppt_x"] },
      ],
    };
    const m = effectMotion(e, () => G);
    expect(m.note).toMatch(/plays as a fade/);
    expect(m.segments.map((s) => s.ch)).toEqual(["op"]);
    const emph = effectMotion({ ...e, presetClass: "emph", sets: null, behaviours: [e.behaviours[1]!] }, () => G);
    expect(emph.segments).toEqual([]);
    expect(emph.note).toMatch(/not animated/);
  });

  it("the slide's notes list what plays other than as written: only the fades standing in for rare filters", async () => {
    expect(motionNotes((await slide("entrances", 1)).steps)).toEqual(["dissolve-in: plays as a fade (dissolve)"]);
    expect(motionNotes((await slide("entrances", 2)).steps)).toEqual([]);
    // Emphasis holds now (R24-R31): grow, spin, transparency, colour play and stay.
    for (const n of [1, 2, 3]) expect(motionNotes((await slide("exits-emphasis", n)).steps), `exits-emphasis ${n}`).toEqual([]);
    for (const n of [1, 2, 3, 4]) expect(motionNotes((await slide("emphasis", n)).steps), `emphasis ${n}`).toEqual([]);
  });
});

describe("the play planner: what starts when", () => {
  it("triggers slide 1, click 1: on click and with at 0, after previous at 500 ms", async () => {
    const t = await slide("triggers", 1);
    const step = t.steps[1]!;
    const shapes = new Map<string, ShapeInfo>(step.map((e) => [e.target!.spid, { paraCount: 0, paras: false }]));
    const p = planStep(step, entryState(t), shapes, [...shapes.keys()], () => base(), noLanding, W, H);
    expect(p.duration).toBe(1000);
    const byId = new Map(p.tracks.map((tr) => [tr.id, tr.keyframes]));
    const at = (id: string, ms: number) => {
      const kf = byId.get(id)!;
      return kf.filter((k) => k.offset <= ms / 1000).at(-1)!.opacity!;
    };
    const [a, b, c] = step.map((e) => e.target!.spid);
    expect(at(a!, 0)).toBe(0);
    expect(at(a!, 250)).toBeCloseTo(0.5, 1);
    expect(at(b!, 250)).toBeCloseTo(0.5, 1);
    expect(at(c!, 400)).toBe(0);
    expect(at(c!, 750)).toBeCloseTo(0.5, 1);
    expect(at(c!, 1000)).toBe(1);
  });

  it("caps a step's play time", async () => {
    const t = await slide("triggers", 1);
    const step = t.steps[2]!;
    const shapes = new Map<string, ShapeInfo>(step.map((e) => [e.target!.spid, { paraCount: 0, paras: false }]));
    expect(planStep(step, buildStates(t)[1]!, shapes, [...shapes.keys()], () => base(), noLanding, W, H, 300).duration).toBe(300);
  });

  it("an appear alone moves nothing: the step lands at once", async () => {
    const t = await slide("entrances", 1);
    const shapes = new Map<string, ShapeInfo>([[t.steps[1]![0]!.target!.spid, { paraCount: 0, paras: false }]]);
    expect(planStep(t.steps[1]!, entryState(t), shapes, [...shapes.keys()], () => base(), noLanding, W, H).duration).toBe(0);
  });
});

describe("what a hostile deck can cost", () => {
  /** A fade on shape `spid`, `start` ms into the step: the parser's own shape. */
  const fadeOn = (spid: string, start: number): Effect => {
    const target = { spid, bg: false, para: null };
    const common = { target, delay: 0, repeatCount: 1000, autoRev: false, accel: 0, decel: 0, additive: null, fill: null };
    return {
      trigger: "with", start, dur: 500, presetClass: "entr", preset: 10, subtype: 0, name: "fade-in", dir: null, target, sets: "visible",
      path: null, group: null, accel: 0, decel: 0, autoRev: false, repeatCount: 1000, fill: "hold",
      behaviours: [
        { ...common, kind: "set", dur: 1, to: "visible", attrs: ["style.visibility"] },
        { ...common, kind: "effect", dur: 500, filter: "fade", transition: "in", prLst: null, attrs: [] },
      ],
    };
  };
  const shapes = new Map<string, ShapeInfo>(Array.from({ length: 200 }, (_, i) => [String(i + 2), { paraCount: 0, paras: false }]));
  const ids = [...shapes.keys()];

  it("past the budget (2,000 effects on 200 shapes, the parser's cap) the click lands without planning", () => {
    const effects = Array.from({ length: 2000 }, (_, i) => fadeOn(String((i % 200) + 2), i * 7));
    const t0 = performance.now();
    const p = planStep(effects, { hidden: ids, moved: {} }, shapes, ids, () => base(), noLanding, W, H);
    expect(p.overBudget).toBe(true);
    expect(performance.now() - t0).toBeLessThan(50);
  });

  it("at the budget it plans in well under a frame budget and keeps every track within MAX_FRAMES", () => {
    const effects = Array.from({ length: PLAY_BUDGET.events }, (_, i) => fadeOn(String((i % 200) + 2), i * 37));
    const t0 = performance.now();
    const p = planStep(effects, { hidden: ids, moved: {} }, shapes, ids, () => base(), noLanding, W, H);
    const ms = performance.now() - t0;
    expect(p.overBudget).toBe(false);
    expect(p.tracks.length).toBe(200);
    for (const tr of p.tracks) expect(tr.keyframes.length).toBeLessThanOrEqual(MAX_FRAMES);
    expect(ms).toBeLessThan(1500);
  });

  it("clamps what a file can push: huge paths and scales stay finite and bounded", () => {
    const huge = pathPolyline("M 0 0 L 1e308 0 L -1e308 0 E")!;
    expect(huge.pts.every(([x, y]) => Number.isFinite(x) || x === Infinity || x === -Infinity || Number.isFinite(y))).toBe(true);
    const target = { spid: "2", bg: false, para: null };
    const common = { target, delay: 0, repeatCount: 1000, autoRev: false, accel: 0, decel: 0, additive: null, fill: "hold" };
    const path: Effect = {
      ...fadeOn("2", 0), presetClass: "path", name: "path", sets: null, path: "M 0 0 L 1e308 0 L -1e308 0 E",
      behaviours: [{ ...common, kind: "motion", dur: 1000, path: "M 0 0 L 1e308 0 L -1e308 0 E", origin: "layout", pathEditMode: null, attrs: ["ppt_x", "ppt_y"] }],
    };
    const grow: Effect = {
      ...fadeOn("2", 0), presetClass: "emph", name: "grow-shrink", sets: null,
      behaviours: [{ ...common, kind: "scale", dur: 1000, by: { x: 1e300, y: 1e300 }, from: null, to: null, attrs: [] }],
    };
    const one = new Map<string, ShapeInfo>([["2", { paraCount: 0, paras: false }]]);
    for (const e of [path, grow]) {
      const kf = planStep([e], { hidden: [], moved: {} }, one, ["2"], () => base(), noLanding, W, H).tracks[0]?.keyframes ?? [];
      for (const f of kf) {
        for (const v of [f.translate, f.scale].filter((x): x is string => x !== undefined)) {
          for (const n of v.split(/px|\s+/).filter((x) => x !== "").map(Number)) {
            expect(Number.isFinite(n), `${e.name} ${v}`).toBe(true);
            expect(Math.abs(n)).toBeLessThanOrEqual(1e5);
          }
        }
      }
    }
  });
});
