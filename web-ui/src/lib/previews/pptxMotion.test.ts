import JSZip from "jszip";
import { describe, expect, it } from "vitest";
import { clipMargin, clipRegion, effectMotion, motionNotes, pacedSampler, pathPolyline, patternOf, patternShare, progress, revealPoint, warp, type Geom, type MaskFrame } from "./pptxMotion";
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

/** A mask frame's alpha at (`x`, `y`) in its image, read back from the CSS
 *  (linear, radial and conic gradients of `rgba(0,0,0,a) <pos>` stops). */
function maskAt(f: MaskFrame, x: number, y: number): number {
  const [W, H] = f.maskSize.split(" ").map((v) => Number.parseFloat(v)) as [number, number];
  const layers = f.maskImage.split(/,\s*(?=(?:linear|radial|conic)-gradient)/);
  const as = layers.map((g) => {
    const stops = [...g.matchAll(/rgba\(0,0,0,([0-9.]+)\) (-?[0-9.]+)(px|deg)/g)].map((m) => [Number(m[2]), Number(m[1])] as const);
    let pos: number;
    const lin = /^linear-gradient\((-?[0-9.]+)deg/.exec(g);
    const rad = /^radial-gradient\(([0-9.]+)px ([0-9.]+)px at ([0-9.]+)px ([0-9.]+)px/.exec(g);
    const con = /^conic-gradient\(from 0deg at ([0-9.]+)px ([0-9.]+)px/.exec(g);
    if (lin !== null) {
      const a = (Number(lin[1]) * Math.PI) / 180;
      const len = Math.abs(W * Math.sin(a)) + Math.abs(H * Math.cos(a));
      pos = len / 2 + (x - W / 2) * Math.sin(a) - (y - H / 2) * Math.cos(a);
    } else if (rad !== null) {
      const [rx, ry, cx, cy] = rad.slice(1).map(Number) as [number, number, number, number];
      pos = Math.hypot((x - cx) / rx, (y - cy) / ry) * rx;
    } else {
      const [cx, cy] = con!.slice(1).map(Number) as [number, number];
      pos = (Math.atan2(x - cx, cy - y) * 180) / Math.PI;
      if (pos < 0) pos += 360;
    }
    if (pos <= stops[0]![0]) return stops[0]![1];
    for (let i = 1; i < stops.length; i++) {
      const [p0, a0] = stops[i - 1]!;
      const [p1, a1] = stops[i]!;
      if (pos <= p1) return p1 > p0 ? a0 + ((a1 - a0) * (pos - p0)) / (p1 - p0) : a1;
    }
    return stops[stops.length - 1]![1];
  });
  return as.reduce((a, b) => (f.maskComposite === "intersect" ? a * b : a + b - a * b));
}

const SOFT = ["wipe(left)", "wipe(right)", "wipe(up)", "wipe(down)", "barn(inVertical)", "barn(outHorizontal)", "blinds(horizontal)", "blinds(vertical)", "box(in)", "box(out)", "circle(in)", "circle(out)", "diamond(in)", "diamond(out)", "plus(in)", "plus(out)", "wheel(1)", "wheel(4)", "wedge"];

describe("transition filters: a soft edge 15% of the sweep, as PowerPoint's film draws it", () => {
  it("the mask's CSS draws the model, in and out, on a wide and a tall box", () => {
    for (const f of SOFT) {
      for (const out of [false, true]) {
        const r = clipRegion(f, out)!;
        expect(r, f).not.toBe(null);
        for (const [w, h] of [[388, 320], [150, 400]] as const) {
          const e = clipMargin(w, h);
          for (const p of [0.1, 0.5, 0.85]) {
            const frame = r(p, w, h);
            expect(frame.maskPosition).toBe(`${-e}px ${-e}px`);
            for (let gx = 0.05; gx < 1; gx += 0.1) {
              for (let gy = 0.05; gy < 1; gy += 0.1) {
                const want = revealPoint(f, out, p, gx * w, gy * h, w, h)!;
                // A conic's squared-up angle is drawn through a few stops.
                expect(maskAt(frame, gx * w + e, gy * h + e), `${f} ${out} ${p} ${gx} ${gy}`).toBeCloseTo(want, f.startsWith("w") && !f.startsWith("wipe") ? 1 : 2);
              }
            }
          }
        }
      }
    }
  });

  it("wipe: PowerPoint's 388 px wipe from the left has a 47 px edge, its middle half way across at half time", () => {
    const at = (x: number, p: number) => revealPoint("wipe(left)", false, p, x, 160, 388, 320)!;
    expect(at(194, 0.5)).toBeCloseTo(0.5, 2);
    // 0.9 to 0.1: 80% of the ramp, 15% of 388 px.
    const x90 = 194 - 0.4 * 0.15 * 388;
    expect(at(x90, 0.5)).toBeCloseTo(0.9, 2);
    expect(at(x90 + 46.56, 0.5)).toBeCloseTo(0.1, 2);
    expect(at(0, 0)).toBe(0);
    expect(at(387, 1)).toBe(1);
  });

  it("wedge: half way, the edge's middle lies level with the centre; circle: half way out to the corners", () => {
    expect(revealPoint("wedge", false, 0.5, 90, 50, 100, 100)).toBeCloseTo(0.5, 6);
    expect(revealPoint("wedge", false, 0.5, 50, 10, 100, 100)).toBe(1);
    expect(revealPoint("circle(out)", false, 0.5, 200 + 0.5 * 200 * Math.SQRT2, 100, 400, 200)).toBeCloseTo(0.5, 6);
    // The plus's arms span the whole box from the start.
    expect(revealPoint("plus(out)", false, 0.3, 200, 1, 400, 200)).toBe(1);
  });

  it("blinds: six slats on the shape's own box, each opening from its top", () => {
    const h = 300;
    for (let i = 0; i < 6; i++) {
      expect(revealPoint("blinds(horizontal)", false, 0.5, 10, i * 50 + 1, 200, h)).toBe(1);
      expect(revealPoint("blinds(horizontal)", false, 0.5, 10, i * 50 + 49, 200, h)).toBe(0);
    }
  });

  it("the whole box shows at the end (no mask), and nothing is left going out", () => {
    for (const f of SOFT) {
      expect(clipRegion(f, false)!(1, 100, 80).maskImage).toBe("none");
      expect(clipRegion(f, true)!(0, 100, 80).maskImage).toBe("none");
      expect(revealPoint(f, true, 1, 30, 30, 100, 80)).toBe(0);
    }
  });

  it("from bottom (subtype 4) is wipe(down): the bottom shows first, and the click lands unmasked", async () => {
    const e = (await slide("entrances", 3)).steps.flat().find((x) => x.subtype === 4)!;
    const f = e.behaviours.find((b) => b.kind === "effect");
    expect(f?.kind === "effect" && f.filter).toBe("wipe(down)");
    const kf = planOne(e).tracks[0]!.keyframes;
    const mid = kf.find((k) => k.offset >= 0.5)!;
    expect(mid.maskImage).toMatch(/^linear-gradient\(0deg, /);
    expect(kf[kf.length - 1]!.maskImage).toBe("none");
  });

  it("refuses what it doesn't draw as a moving edge (patterns, or a fade)", () => {
    for (const f of ["dissolve", "randombar(horizontal)", "checkerboard(across)", "strips(downLeft)", "slide(fromTop)", "wipe(sideways)", "box(weird)", "wheel(99)", "x(y)(z)"]) {
      expect(clipRegion(f, false), f).toBe(null);
    }
  });
});

describe("pattern filters, laid out as PowerPoint's film of the filters deck shows them", () => {
  it("dissolve, random bars, checkerboard and strips are patterns, in and out; slide still fades", () => {
    expect(patternOf("dissolve", true)).toEqual({ kind: "dissolve", horizontal: false, out: true });
    expect(patternOf("randombar(vertical)", false)).toEqual({ kind: "bars", horizontal: false, out: false });
    expect(patternOf("checkerboard(down)", false)).toEqual({ kind: "checker", horizontal: false, out: false });
    expect(patternOf("strips(downLeft)", false)).toEqual({ kind: "strips", horizontal: false, out: false, corner: [1, 0] });
    expect(patternOf("checkerboard(diagonal)", false)).toBeNull();
    expect(patternOf("slide(fromTop)", false)).toBeNull();
  });

  it("dissolve fills slow, then fast, then slow; random bars less so (the film's ink by frame)", () => {
    expect(patternShare("dissolve", 0.25)).toBeCloseTo(0.09, 2);
    expect(patternShare("dissolve", 0.5)).toBe(0.5);
    expect(patternShare("dissolve", 0.75)).toBeCloseTo(0.91, 2);
    expect(patternShare("bars", 0.25)).toBeCloseTo(0.16, 2);
    expect(patternShare("bars", 0.75)).toBeCloseTo(0.84, 2);
    expect([0, 1].map((p) => patternShare("dissolve", p))).toEqual([0, 1]);
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

describe("a chained path (order, slide 3: PowerPoint's film)", () => {
  it("a second path written from where the first ended plays on from there, without a jump", async () => {
    const t3 = await slide("order", 3);
    const states = buildStates(t3);
    const shapes = new Map<string, ShapeInfo>([["2", { paraCount: 0, paras: false }]]);
    const b = { ...base(), tx: 0.25 * W, ty: 0 };
    const kf = planStep(t3.steps[2]!, states[1]!, shapes, ["2"], () => b, noLanding, W, H).tracks[0]!.keyframes;
    const xs = kf.map((k) => px(k.translate));
    expect(xs[0]![0]).toBeCloseTo(0.25 * W, 3);
    expect(xs.every(([x]) => Math.abs(x - 0.25 * W) < 1e-6)).toBe(true);
    expect(xs.at(-1)![1]).toBeCloseTo(0.25 * H, 0);
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

describe("a second path replaces the first (R18, PowerPoint's film of paths slide 2)", () => {
  it("snaps back to the layout place as it begins and ends at its own end point", async () => {
    const t = await slide("paths", 2);
    const states = buildStates(t);
    const e = t.steps[3]![0]!;
    const shapes = new Map<string, ShapeInfo>([[e.target!.spid, { paraCount: 0, paras: false }]]);
    const moved = states[2]!.moved[e.target!.spid]!;
    expect(moved).toEqual([0.25, 0]);
    const b = { ...base(), tx: moved[0] * W, ty: moved[1] * H };
    const kf = planStep(t.steps[3]!, states[2]!, shapes, [e.target!.spid], () => b, noLanding, W, H).tracks[0]!.keyframes;
    expect(px(kf[0]!.translate)).toEqual([0, 0]);
    expect(px(kf[kf.length - 1]!.translate)[0]).toBeCloseTo(0, 1);
    expect(px(kf[kf.length - 1]!.translate)[1]).toBeCloseTo(0.25 * H, 0);
    expect(states[3]!.moved[e.target!.spid]).toEqual([0, 0.25]);
  });
});

describe("zoom, spin, teeter, boomerang: from the behaviours", () => {
  it("zoom in grows from nothing about its centre", async () => {
    const zoom = (await slide("entrances", 1)).steps[3]![0]!;
    expect(zoom.name).toBe("zoom-in");
    const kf = planOne(zoom).tracks[0]!.keyframes;
    // From nothing: 0.001, the smallest frame (a frame at exactly 0 breaks WebKit's layer).
    expect(kf[0]!.scale).toBe("0.001 0.001");
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
  it("dissolve plays through random cells; transparency (never ends) plays nothing", async () => {
    const dissolve = (await slide("entrances", 1)).steps[5]![0]!;
    const m = effectMotion(dissolve, () => G);
    expect(m.note).toBeNull();
    expect(m.segments.map((s) => [s.ch, s.pattern])).toEqual([["mask", { kind: "dissolve", horizontal: false, out: false }]]);
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

  it("the slide's notes list what plays other than as written: nothing on the corpus's entrances and emphasis", async () => {
    expect(motionNotes((await slide("entrances", 1)).steps)).toEqual([]);
    for (const n of [1, 2, 3]) expect(motionNotes((await slide("filters", n)).steps), `filters ${n}`).toEqual([]);
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

describe("the contract's played-as-a-fade list (fixtures/pptx-timing/fallbacks.json)", () => {
  // Every filter ECMA-376 names for p:animEffect, with each subtype.
  const UNIVERSE = [
    ...["inVertical", "inHorizontal", "outVertical", "outHorizontal"].map((s) => `barn(${s})`),
    "blinds(horizontal)", "blinds(vertical)", "box(in)", "box(out)",
    "checkerboard(across)", "checkerboard(down)", "circle(in)", "circle(out)",
    "diamond(in)", "diamond(out)", "dissolve", "fade", "plus(in)", "plus(out)",
    "randombar(horizontal)", "randombar(vertical)",
    ...["fromTop", "fromBottom", "fromLeft", "fromRight"].map((s) => `slide(${s})`),
    ...["downLeft", "upLeft", "downRight", "upRight"].map((s) => `strips(${s})`),
    "wedge", ...[1, 2, 3, 4, 8].map((n) => `wheel(${n})`),
    "wipe(up)", "wipe(down)", "wipe(left)", "wipe(right)",
  ];
  const listed = (JSON.parse(new TextDecoder().decode(fs.readFileSync(new URL("fallbacks.json", CORPUS)))) as { filters: { filter: string }[] }).filters.map((f) => f.filter);
  const entrance = (filter: string): Effect => ({
    trigger: "click", start: 0, dur: 500, presetClass: "entr", preset: 0, subtype: 0, name: "other", dir: null,
    target: { spid: "2", bg: false, para: null }, sets: "visible", path: null, group: null, accel: 0, decel: 0, autoRev: false, repeatCount: 1000, fill: null,
    behaviours: [{
      kind: "effect", filter, transition: "in", prLst: null, target: { spid: "2", bg: false, para: null }, delay: 0, dur: 500,
      repeatCount: 1000, autoRev: false, accel: 0, decel: 0, additive: null, fill: null, attrs: [],
    }],
  });

  it("lists exactly the filters an entrance falls back to a fade on", () => {
    const fallsBack = new Set<string>();
    for (const f of UNIVERSE) {
      const note = effectMotion(entrance(f), () => G).note;
      if (note !== null && note.includes("plays as a fade")) fallsBack.add(/^[a-z]+/.exec(f)![0]);
    }
    expect([...fallsBack].sort()).toEqual([...new Set(listed)].sort());
  });
});
