import JSZip from "jszip";
import { describe, expect, it } from "vitest";
import { applyState, boxGeom, clearMasks, clearState, colorEasing, colorWindows, maskCacheStats, maskImages, paintedKeyframes, paraOffset, planState, playStep, releaseMasks, retainMasks, transformScale, type ShapeInfo } from "./pptxPlay";
import type { Pattern } from "./pptxMotion";
import type { TaggedSlide } from "./pptxTag";
import { buildStates, parseTimeline, type BuildState, type GoldenSlide } from "./pptxTiming";

/**
 * How a build state's keys land on drawn shapes (the timing contract's R5,
 * plus what the renderer can't draw apart), and that `playStep` lands every
 * corpus click on its golden state (on stand-in elements: the motion
 * itself is checked live against the corpus).
 */

const st = (hidden: string[], moved: Record<string, [number, number]> = {}): BuildState => ({ hidden, moved });
const shapes = (entries: [string, ShapeInfo][]) => new Map(entries);

describe("planState", () => {
  it("hides a shape by its key", () => {
    const p = planState(shapes([["2", { paraCount: 0, paras: false }]]), st(["2"]));
    expect([...p.hideShapes]).toEqual(["2"]);
    expect(p.approximate).toBe(false);
  });

  it("hides paragraphs by range when they were matched", () => {
    const p = planState(shapes([["3", { paraCount: 4, paras: true }]]), st(["3:p1-2", "3:p3-3"]));
    expect([...p.hideShapes]).toEqual([]);
    expect([...p.hideParas.get("3")!].sort()).toEqual([1, 2, 3]);
    expect(p.approximate).toBe(false);
  });

  it("a hidden shape hides its paragraphs with it, whatever their keys say", () => {
    const p = planState(shapes([["3", { paraCount: 2, paras: true }]]), st(["3", "3:p0-0"]));
    expect([...p.hideShapes]).toEqual(["3"]);
    expect(p.hideParas.size).toBe(0);
  });

  it("a hidden :bg with every paragraph hidden hides the shape (the bg-paragraphs case, state 0)", () => {
    const p = planState(shapes([["2", { paraCount: 2, paras: true }]]), st(["2:bg", "2:p0-0", "2:p1-1"]));
    expect([...p.hideShapes]).toEqual(["2"]);
    expect(p.approximate).toBe(false);
  });

  it("a hidden :bg under shown text keeps the box drawn, approximately", () => {
    const p = planState(shapes([["2", { paraCount: 2, paras: true }]]), st(["2:bg", "2:p1-1"]));
    expect([...p.hideShapes]).toEqual([]);
    expect([...p.hideParas.get("2")!]).toEqual([1]);
    expect(p.approximate).toBe(true);
  });

  it("a value held on :bg lands on the box's own drawing, and on the shape only approximately when it has none (R32)", () => {
    const state: BuildState = { hidden: [], moved: {}, scale: { "4:bg": [2.25, 2.25], "4:p0-0": [1.5, 1.5] } };
    const drawn = planState(shapes([["4", { paraCount: 2, paras: true, bg: true }]]), state);
    expect(drawn.held.get("4!bg")).toEqual({ scale: [2.25, 2.25] });
    expect(drawn.held.get("4#0")).toEqual({ scale: [1.5, 1.5] });
    expect(drawn.held.has("4")).toBe(false);
    expect(drawn.approximate).toBe(false);
    const flat = planState(shapes([["4", { paraCount: 2, paras: true }]]), state);
    expect(flat.held.get("4")).toEqual({ scale: [2.25, 2.25] });
    expect(flat.approximate).toBe(true);
  });

  it("unmatched paragraphs: the shape comes with its first shown paragraph", () => {
    const info = shapes([["2", { paraCount: 3, paras: false }]]);
    const all = planState(info, st(["2:p0-0", "2:p1-1", "2:p2-2"]));
    expect([...all.hideShapes]).toEqual(["2"]);
    expect(all.approximate).toBe(true);
    const some = planState(info, st(["2:p1-1", "2:p2-2"]));
    expect([...some.hideShapes]).toEqual([]);
    expect(some.approximate).toBe(true);
  });

  it("moves a shape, and a paragraph on its own when it was matched", () => {
    const p = planState(
      shapes([
        ["2", { paraCount: 0, paras: false }],
        ["3", { paraCount: 2, paras: true }],
      ]),
      st([], { "2": [0.25, 0], "3:p1-1": [0, 0.5] }),
    );
    expect(p.moveShapes.get("2")).toEqual([0.25, 0]);
    expect(p.moveParas.get("3")?.get(1)).toEqual([0, 0.5]);
    expect(p.approximate).toBe(false);
  });

  it("a key for a shape that wasn't tagged is approximate, never an error", () => {
    const p = planState(shapes([]), st(["9", "9:p0-0"], { "9": [0.1, 0.1] }));
    expect(p.hideShapes.size).toBe(0);
    expect(p.approximate).toBe(true);
  });

  it("ignores keys outside the contract's shape", () => {
    const p = planState(shapes([["2", { paraCount: 0, paras: false }]]), st(["__proto__", "2:x", "constructor"]));
    expect(p.hideShapes.size).toBe(0);
    expect(p.approximate).toBe(false);
  });
});

describe("a paragraph's offset inside a scaled text body", () => {
  it("multiplies the scales between the paragraph and the slide", () => {
    expect(transformScale(["", "none"])).toBe(1);
    expect(transformScale(["scale(0.5)"])).toBe(0.5);
    expect(transformScale(["scale(0.5, 0.5)", "scale(0.8)"])).toBeCloseTo(0.4);
  });

  it("refuses what turns or flips an offset", () => {
    expect(transformScale(["rotate(30deg)"])).toBe(null);
    expect(transformScale(["scaleX(-1)"])).toBe(null);
    expect(transformScale(["scale(0.5, 0.8)"])).toBe(null);
    expect(transformScale(["rotate(180deg) scale(0.5)"])).toBe(null);
    expect(transformScale(["scale(0)"])).toBe(null);
  });

  it("divides by the body's scale so the paragraph lands where PowerPoint puts it", () => {
    // A quarter of a 1280 px slide is 320 px on the slide; inside a body
    // drawn at 0.8, that is 400 of the body's own px.
    expect(paraOffset([0.25, 0], 1280, 720, ["scale(0.8)", ""])).toEqual([400, 0]);
    expect(paraOffset([0, 0.5], 1280, 720, [""])).toEqual([0, 360]);
    expect(paraOffset([0.25, 0], 1280, 720, ["rotate(90deg)"])).toBe(null);
  });
});

// --- playback: every corpus click lands on its golden state -------------------------


type NodeFs = { readFileSync(path: URL): Uint8Array; readdirSync(path: URL): string[] };
const fs = (globalThis as unknown as { process: { getBuiltinModule(id: "node:fs"): NodeFs } }).process.getBuiltinModule("node:fs");
const CORPUS = new URL("./fixtures/pptx-timing/", import.meta.url);

const kebab = (k: string) => k.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`);

/** Enough of an element for `applyState` and `playStep`: inline style,
 *  attributes, a box, and `animate` that counts what runs (with the
 *  keyframes it was given). */
class FakeEl {
  props = new Map<string, string>();
  /** Properties set `!important`. */
  important = new Set<string>();
  style: Record<string, string> & { cssText: string; getPropertyValue(p: string): string; getPropertyPriority(p: string): string; setProperty(p: string, v: string, priority?: string): void; removeProperty(p: string): void };
  attrs = new Map<string, string>();
  parentElement: FakeEl | null = null;
  tagName = "DIV";
  /** Its first child: a probe whose box follows this element's margin
   *  and border, as `inflate` measures it. */
  firstElementChild: FakeEl | null = null;
  probeFor: FakeEl | null = null;
  offsetWidth = 100;
  offsetHeight = 50;
  running = 0;
  started = 0;
  frames: Record<string, unknown>[][] = [];
  throwOnLeft = false;
  constructor(box: boolean) {
    if (box) for (const [k, v] of Object.entries({ left: "10px", top: "20px", width: "100px", height: "50px", position: "absolute" })) this.props.set(k, v);
    const methods = {
      getPropertyValue: (p: string) => this.props.get(p) ?? "",
      getPropertyPriority: (p: string) => (this.important.has(p) ? "important" : ""),
      setProperty: (p: string, v: string, priority = "") => {
        if (priority === "important") this.important.add(p);
        else this.important.delete(p);
        if (v === "") this.props.delete(p);
        else this.props.set(p, v);
      },
      removeProperty: (p: string) => void (this.props.delete(p), this.important.delete(p)),
    };
    this.style = new Proxy(methods, {
      get: (t, k: string) => {
        if (k in t) return t[k as keyof typeof t];
        if (k === "cssText") return [...this.props.keys()].join(";");
        if (k === "left" && this.throwOnLeft) throw new Error("forced");
        return this.props.get(kebab(k)) ?? "";
      },
      set: (_t, k: string, v: string) => (v === "" ? this.props.delete(kebab(k)) : this.props.set(kebab(k), v), true),
    }) as unknown as FakeEl["style"];
  }
  hasAttribute(n: string) {
    return n === "style" ? this.props.size > 0 : this.attrs.has(n);
  }
  getAttribute(n: string) {
    return this.attrs.get(n) ?? null;
  }
  setAttribute(n: string, v: string) {
    this.attrs.set(n, v);
  }
  removeAttribute(n: string) {
    if (n === "style") this.props.clear();
    this.attrs.delete(n);
  }
  querySelector() {
    return null;
  }
  /** SVG nodes carrying a filter inside this element. */
  filtered: FakeEl[] = [];
  querySelectorAll(sel: string) {
    return sel === "[filter]" ? this.filtered : [];
  }
  closest() {
    return null;
  }
  getBoundingClientRect() {
    const p = this.probeFor;
    if (p !== null) {
      const n = (v: string | undefined) => Number.parseFloat(v ?? "") || 0;
      const left = n(p.props.get("margin-left")) + n(p.props.get("border-left-width"));
      const top = n(p.props.get("margin-top")) + n(p.props.get("border-top-width"));
      return { left, top, right: left + 100, bottom: top + 50, width: 100, height: 50 };
    }
    return { left: 0, top: 0, right: 100, bottom: 50, width: 100, height: 50 };
  }
  animate(frames: Record<string, unknown>[]) {
    this.running += 1;
    this.started += 1;
    this.frames.push(frames);
    return { cancel: () => (this.running -= 1) };
  }
}

/** A tagged slide with one element per shape the timeline names (and its
 *  paragraphs, as many as the keys reach). */
function fakeSlide(t: ReturnType<typeof parseTimeline>, probes = false): { tagged: TaggedSlide; all: FakeEl[] } {
  const paras = new Map<string, number>();
  const note = (spid: string, n: number) => paras.set(spid, Math.max(paras.get(spid) ?? 0, n));
  for (const step of [...t.steps, ...t.interactive.flatMap((s) => s.steps)]) {
    for (const e of step) {
      for (const b of e.behaviours) {
        if (b.target === null) continue;
        note(b.target.spid, b.target.para === null ? 0 : b.target.para[1] + 1);
      }
    }
  }
  const slideEl = new FakeEl(false);
  const shapes = new Map<string, TaggedSlide["shapes"] extends Map<string, infer S> ? S : never>();
  const all: FakeEl[] = [];
  for (const [spid, n] of paras) {
    const el = new FakeEl(true);
    el.parentElement = slideEl;
    if (probes) {
      el.firstElementChild = new FakeEl(false);
      el.firstElementChild.probeFor = el;
    }
    const ps = Array.from({ length: n }, () => {
      const p = new FakeEl(false);
      p.parentElement = el;
      return p;
    });
    all.push(el, ...ps);
    shapes.set(spid, { el: el as unknown as HTMLElement, paras: n > 0 ? (ps as unknown as HTMLElement[]) : null, paraCount: n });
  }
  return { tagged: { shapes, fellBack: false, paraMismatches: 0 }, all };
}

type Slide = { name: string; part: string; t: ReturnType<typeof parseTimeline>; golden: GoldenSlide };

async function corpus(): Promise<Slide[]> {
  const out: Slide[] = [];
  for (const file of fs.readdirSync(new URL("decks/", CORPUS)).filter((n) => n.endsWith(".pptx")).sort()) {
    const name = file.slice(0, -5);
    const zip = await JSZip.loadAsync(fs.readFileSync(new URL(`decks/${file}`, CORPUS)));
    const golden = JSON.parse(new TextDecoder().decode(fs.readFileSync(new URL(`goldens/${name}.json`, CORPUS)))) as { slides: GoldenSlide[] };
    for (const g of golden.slides) out.push({ name, part: g.part, t: parseTimeline(await zip.file(g.part)!.async("string")), golden: g });
  }
  for (const file of fs.readdirSync(new URL("cases/", CORPUS)).filter((n) => n.endsWith(".xml")).sort()) {
    const name = file.slice(0, -4);
    const golden = JSON.parse(new TextDecoder().decode(fs.readFileSync(new URL(`goldens/cases/${name}.json`, CORPUS)))) as GoldenSlide;
    const xml = new TextDecoder().decode(fs.readFileSync(new URL(`cases/${file}`, CORPUS)));
    out.push({ name: `cases/${name}`, part: file, t: parseTimeline(xml), golden });
  }
  return out;
}

const W = 1280;
const H = 720;

/** What the element should carry inline for `id` in state `st`. */
function expected(info: Map<string, ShapeInfo>, st: BuildState, id: string): Record<string, string> {
  const plan = planState(info, st);
  const [spid, para] = id.split("#");
  const out: Record<string, string> = {};
  const hidden = para === undefined ? plan.hideShapes.has(spid!) : plan.hideParas.get(spid!)?.has(Number(para)) === true;
  const held = plan.held.get(id);
  if (hidden) out.opacity = "0";
  else if (held?.opacity !== undefined) out.opacity = String(Math.round(held.opacity * 1000) / 1000);
  const off = para === undefined ? plan.moveShapes.get(spid!) : plan.moveParas.get(spid!)?.get(Number(para));
  const r3 = (n: number) => Math.round(n * 1000) / 1000;
  if (off !== undefined && (off[0] !== 0 || off[1] !== 0)) out.translate = `${r3(off[0] * W)}px ${r3(off[1] * H)}px`;
  if (held?.scale !== undefined) out.scale = `${held.scale[0]} ${held.scale[1]}`;
  if (held?.turn !== undefined) out.rotate = `${held.turn}deg`;
  return out;
}

const inline = (el: FakeEl) => Object.fromEntries([...el.props].filter(([k]) => ["opacity", "translate", "scale", "rotate"].includes(k)));

describe("playStep lands every corpus click on its golden state", async () => {
  const slides = await corpus();
  for (const { name, part, t, golden } of slides) {
    it(`${name} ${part}`, async () => {
      const { tagged, all } = fakeSlide(t);
      const info = new Map<string, ShapeInfo>();
      for (const [id, s] of tagged.shapes) info.set(id, { paraCount: s.paraCount, paras: s.paras !== null });
      for (let k = 0; k < t.steps.length; k++) {
        const play = playStep(tagged, t, k, { w: W, h: H });
        play.finish();
        play.finish();
        await play.finished;
        for (const [id, s] of tagged.shapes) {
          const el = s.el as unknown as FakeEl;
          expect(inline(el), `${id} after click ${k}`).toEqual(expected(info, golden.states[k]!, id));
          expect(el.hasAttribute("inert")).toBe(planState(info, golden.states[k]!).hideShapes.has(id));
          (s.paras ?? []).forEach((p, i) => {
            expect(inline(p as unknown as FakeEl), `${id}#${i} after click ${k}`).toEqual(expected(info, golden.states[k]!, `${id}#${i}`));
          });
        }
        // Nothing still runs, and nothing but the state is left inline.
        for (const el of all) expect(el.running).toBe(0);
      }
      // Off the slide, it is as drawn.
      clearState(tagged);
      for (const el of all) {
        expect(inline(el)).toEqual({});
        expect(el.hasAttribute("inert")).toBe(false);
      }
    });
  }

  it("animates the clicks that move something, and none under reduced motion", async () => {
    const s = slides.find((x) => x.name === "entrances" && x.part.endsWith("slide2.xml"))!;
    const { tagged, all } = fakeSlide(s.t);
    const play = playStep(tagged, s.t, 1, { w: W, h: H });
    expect(all.reduce((n, e) => n + e.running, 0)).toBe(1);
    play.finish();
    expect(all.reduce((n, e) => n + e.running, 0)).toBe(0);
    const before = all.reduce((n, e) => n + e.started, 0);
    playStep(tagged, s.t, 2, { w: W, h: H, reduced: true });
    expect(all.reduce((n, e) => n + e.started, 0)).toBe(before);
  });

  it("a filled box's background coming in keeps its hidden paragraphs hidden on every frame (bg-paragraphs, click 1)", () => {
    const s = slides.find((x) => x.name === "cases/bg-paragraphs")!;
    const { tagged } = fakeSlide(s.t);
    playStep(tagged, s.t, 0, { w: W, h: H }).finish();
    const play = playStep(tagged, s.t, 1, { w: W, h: H });
    const shape = tagged.shapes.get("2")!;
    const info = new Map([["2", { paraCount: shape.paraCount, paras: true }]]);
    const states = buildStates(s.t);
    shape.paras!.forEach((p, i) => {
      const id = `2#${i}`;
      const hiddenBefore = planState(info, states[0]!).hideParas.get("2")?.has(i) || planState(info, states[0]!).hideShapes.has("2");
      const hiddenAfter = planState(info, states[1]!).hideParas.get("2")?.has(i) === true;
      if (!hiddenAfter) return;
      const fe = p as unknown as FakeEl;
      const frames = fe.frames.at(-1);
      if (frames !== undefined && fe.running > 0) for (const f of frames) expect(f.opacity, `${id} in flight`).toBe(0);
      else expect(fe.props.get("opacity") === "0" || hiddenBefore).toBe(true);
    });
    play.finish();
  });

  it("repaints a growing shape's filtered drawing every frame, ending on its own opacity (WebKit's stale filter picture)", () => {
    const s = slides.find((x) => x.name === "entrances" && x.part.endsWith("slide1.xml"))!;
    const { tagged } = fakeSlide(s.t);
    const shadow = new FakeEl(false);
    shadow.attrs.set("opacity", "0.5");
    const zoomed = tagged.shapes.get("4")!.el as unknown as FakeEl;
    zoomed.filtered.push(shadow);
    const faded = new FakeEl(false);
    (tagged.shapes.get("3")!.el as unknown as FakeEl).filtered.push(faded);
    for (let k = 0; k < 3; k++) playStep(tagged, s.t, k, { w: W, h: H }).finish();
    const play = playStep(tagged, s.t, 3, { w: W, h: H });
    expect(zoomed.running).toBe(1);
    expect(shadow.running).toBe(1);
    const frames = shadow.frames.at(-1)!;
    expect(frames[0]!.opacity).toBe(0.5);
    expect(frames.at(-1)!.opacity).toBe(0.5);
    expect(frames.some((f) => f.opacity !== 0.5)).toBe(true);
    play.finish();
    expect(shadow.running).toBe(0);
    // A fade scales nothing: its shadow is left alone.
    playStep(tagged, s.t, 2, { w: W, h: H }).finish();
    expect(faded.started).toBe(0);
  });

  it("a slide whose drawing throws while planning lands on its state", async () => {
    const s = slides.find((x) => x.name === "entrances" && x.part.endsWith("slide2.xml"))!;
    const { tagged, all } = fakeSlide(s.t);
    for (const el of all) el.throwOnLeft = true;
    const play = playStep(tagged, s.t, 1, { w: W, h: H });
    await play.finished;
    for (const el of all) expect(el.running).toBe(0);
    const info = new Map([...tagged.shapes].map(([id, x]) => [id, { paraCount: x.paraCount, paras: x.paras !== null }]));
    for (const [id, x] of tagged.shapes) expect(inline(x.el as unknown as FakeEl)).toEqual(expected(info, buildStates(s.t)[1]!, id));
  });

  it("a landing whose state throws still ends its animations and resolves", async () => {
    const s = slides.find((x) => x.name === "entrances" && x.part.endsWith("slide2.xml"))!;
    const { tagged, all } = fakeSlide(s.t);
    const play = playStep(tagged, s.t, 1, { w: W, h: H });
    expect(all.reduce((n, e) => n + e.running, 0)).toBe(1);
    Object.defineProperty(tagged, "shapes", {
      get() {
        throw new Error("forced");
      },
    });
    expect(() => play.finish()).toThrow("forced");
    expect(all.reduce((n, e) => n + e.running, 0)).toBe(0);
    await play.finished;
  });
});

/** A canvas stand-in for `maskImages`: records each picture's size. */
function withCanvas<T>(run: (sizes: [number, number][]) => T): T {
  const g = globalThis as unknown as { document?: unknown };
  const had = g.document;
  const sizes: [number, number][] = [];
  const urls = { create: URL.createObjectURL, revoke: URL.revokeObjectURL };
  let n = 0;
  const revoked: string[] = [];
  g.document = {
    createElement: () => {
      const c = {
        width: 0,
        height: 0,
        getContext: () => ({ createImageData: (w: number, h: number) => (sizes.push([w, h]), { data: new Uint8ClampedArray(w * h * 4) }), putImageData() {} }),
        toDataURL: () => "data:image/png;base64,AAAA",
      };
      return c;
    },
  };
  URL.createObjectURL = () => `blob:test/${n++}`;
  URL.revokeObjectURL = (u: string) => void revoked.push(u);
  try {
    return run(Object.assign(sizes, { revoked }) as [number, number][]);
  } finally {
    g.document = had;
    URL.createObjectURL = urls.create;
    URL.revokeObjectURL = urls.revoke;
    clearMasks();
  }
}

describe("pattern pictures are bounded and outlive no click", () => {
  const kinds: Pattern[] = [
    { kind: "dissolve", horizontal: false, out: false },
    { kind: "bars", horizontal: true, out: false },
    { kind: "bars", horizontal: false, out: true },
    { kind: "checker", horizontal: true, out: false },
    { kind: "checker", horizontal: false, out: false },
    { kind: "strips", horizontal: false, out: false, corner: [1, 1] },
  ];
  for (const pattern of kinds) {
    it(`${pattern.kind}${pattern.horizontal ? " across" : ""} on a 1e8 px box draws a bounded picture, quickly`, () => {
      withCanvas((sizes) => {
        const t0 = performance.now();
        const set = maskImages(pattern, 1e8, 1e8, 10, "4", 50);
        expect(set).not.toBeNull();
        expect(set!.urls.length).toBe(51);
        for (const [w, h] of sizes) expect(w * h).toBeLessThanOrEqual(768 * 768);
        expect(performance.now() - t0).toBeLessThan(3000);
      });
    });
  }

  it("refuses a box that is not finite, and caps the levels", () => {
    withCanvas(() => {
      expect(maskImages(kinds[0]!, Number.POSITIVE_INFINITY, 10, 0, "4", 12)).toBeNull();
      expect(maskImages(kinds[0]!, 100, 100, 0, "4", 1e9)!.urls.length).toBe(51);
    });
  });

  it("a click that dissolves 40 shapes draws a few sets, and keeps every set it holds", () => {
    withCanvas((sizes) => {
      const ids = Array.from({ length: 40 }, (_, i) => `${i + 2}`);
      const sets = ids.map((id) => maskImages(kinds[0]!, 100, 100, 10, id, 12)!);
      expect(new Set(sets).size).toBeLessThanOrEqual(8);
      expect(sizes.length).toBeLessThanOrEqual(8 * 13);
      // Held sets survive any number of later sets; released, they go.
      retainMasks(sets);
      const revoked = (sizes as unknown as { revoked: string[] }).revoked;
      for (let i = 0; i < 60; i++) maskImages(kinds[3]!, 100 + i, 100, 10, "x", 12);
      for (const s of sets) for (const u of s.urls) expect(revoked).not.toContain(/^url\("(.*)"\)$/.exec(u)![1]);
      expect(maskCacheStats().size).toBeLessThanOrEqual(32 + 8);
      releaseMasks(sets);
      expect(maskCacheStats().held).toBe(0);
      expect(maskCacheStats().size).toBeLessThanOrEqual(32);
      clearMasks();
      expect(maskCacheStats().size).toBe(0);
    });
  });
});

describe("measured, never assumed: geometry under any fit", () => {
  // A 1280×720 slide shown at 0.625 (a small window) and 1.5 (full screen),
  // offset in the page; a paragraph a quarter in from the left, a fifth down.
  for (const k of [0.625, 1, 1.5, 2.67]) {
    it(`a paragraph's place in slide fractions is the same at a fit of ${k}`, () => {
      const slide = { left: 40, top: 30, width: 1280 * k, height: 720 * k };
      const para = { left: 40 + 320 * k, top: 30 + 144 * k, width: 400 * k, height: 60 * k };
      const g = boxGeom(para, slide, 1280, 720);
      expect(g.ppt_x).toBeCloseTo((320 + 200) / 1280, 10);
      expect(g.ppt_y).toBeCloseTo((144 + 30) / 720, 10);
      expect(g.ppt_w).toBeCloseTo(400 / 1280, 10);
      expect(g.ppt_h).toBeCloseTo(60 / 720, 10);
    });
  }
});

describe("colour blends run on the effect's clock (B5)", () => {
  const xml = (accel: number, decel: number) =>
    `<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree/></p:cSld><p:timing><p:tnLst><p:par><p:cTn id="1" dur="indefinite" nodeType="tmRoot"><p:childTnLst><p:seq><p:cTn id="2" dur="indefinite" nodeType="mainSeq"><p:childTnLst>` +
    `<p:par><p:cTn id="3" fill="hold"><p:stCondLst><p:cond delay="indefinite"/></p:stCondLst><p:childTnLst><p:par><p:cTn id="4" fill="hold"><p:stCondLst><p:cond delay="0"/></p:stCondLst><p:childTnLst>` +
    `<p:par><p:cTn id="5" presetID="1" presetClass="emph" fill="hold" nodeType="clickEffect" accel="${accel}" decel="${decel}"><p:stCondLst><p:cond delay="0"/></p:stCondLst><p:childTnLst>` +
    `<p:animClr clrSpc="rgb" dir="cw"><p:cBhvr><p:cTn id="6" dur="1000" fill="hold"/><p:tgtEl><p:spTgt spid="2"/></p:tgtEl><p:attrNameLst><p:attrName>fillcolor</p:attrName></p:attrNameLst></p:cBhvr><p:to><a:srgbClr val="FF0000"/></p:to></p:animClr>` +
    `</p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:seq></p:childTnLst></p:cTn></p:par></p:tnLst></p:timing></p:sld>`;

  it("an even blend needs no easing; decel front-loads it as every other channel", () => {
    const even = colorWindows(parseTimeline(xml(0, 0)).steps[1]!).get("2")!;
    expect([even.from, even.to]).toEqual([0, 1000]);
    expect(colorEasing(even)).toBeNull();
    const eased = colorWindows(parseTimeline(xml(0, 100000)).steps[1]!).get("2")!;
    expect(eased.progress(500)).toBeCloseTo(0.75, 6);
    const ease = colorEasing(eased, 4)!;
    expect(ease).toBe("linear(0, 0.4375, 0.75, 0.9375, 1)");
  });
});

describe("a grow or turn plays through painted custom properties", () => {
  it("moves scale and rotate onto the properties, none as 1 and 0deg, and keeps every other value", () => {
    const out = paintedKeyframes([
      { offset: 0, scale: "1 1", rotate: "0deg", opacity: 1, transformOrigin: "10px 5px" },
      { offset: 0.5, scale: "1.25 0.75", rotate: "45deg" },
      { offset: 1, scale: "none", rotate: "none" },
      { offset: 1, scale: "2" },
    ]);
    expect(out).toEqual([
      { offset: 0, opacity: 1, transformOrigin: "10px 5px", "--pptx-sx": "1", "--pptx-sy": "1", "--pptx-turn": "0deg" },
      { offset: 0.5, "--pptx-sx": "1.25", "--pptx-sy": "0.75", "--pptx-turn": "45deg" },
      { offset: 1, "--pptx-sx": "1", "--pptx-sy": "1", "--pptx-turn": "0deg" },
      { offset: 1, "--pptx-sx": "2", "--pptx-sy": "2" },
    ]);
  });
});

describe("clearState puts back what the renderer drew", () => {
  it("keeps an inline declaration's priority", () => {
    const t = parseTimeline(
      `<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree/></p:cSld><p:timing><p:tnLst><p:par><p:cTn id="1" dur="indefinite" nodeType="tmRoot"><p:childTnLst><p:seq><p:cTn id="2" dur="indefinite" nodeType="mainSeq"><p:childTnLst>` +
        `<p:par><p:cTn id="3" fill="hold"><p:stCondLst><p:cond delay="indefinite"/></p:stCondLst><p:childTnLst><p:par><p:cTn id="4" fill="hold"><p:stCondLst><p:cond delay="0"/></p:stCondLst><p:childTnLst>` +
        `<p:par><p:cTn id="5" presetID="10" presetClass="exit" nodeType="clickEffect"><p:stCondLst><p:cond delay="0"/></p:stCondLst><p:childTnLst>` +
        `<p:set><p:cBhvr><p:cTn id="6" dur="1" fill="hold"><p:stCondLst><p:cond delay="499"/></p:stCondLst></p:cTn><p:tgtEl><p:spTgt spid="2"/></p:tgtEl><p:attrNameLst><p:attrName>style.visibility</p:attrName></p:attrNameLst></p:cBhvr><p:to><p:strVal val="hidden"/></p:to></p:set>` +
        `</p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:seq></p:childTnLst></p:cTn></p:par></p:tnLst></p:timing></p:sld>`,
    );
    const { tagged, all } = fakeSlide(t);
    const el = all[0]!;
    el.style.setProperty("opacity", "0.5", "important");
    applyState(tagged, buildStates(t)[1]!, W, H);
    expect(el.style.getPropertyValue("opacity")).toBe("0");
    clearState(tagged);
    expect(el.style.getPropertyValue("opacity")).toBe("0.5");
    expect(el.style.getPropertyPriority("opacity")).toBe("important");
  });
});

describe("a click that masks a shape and moves, grows or turns it lands on exactly its state", () => {
  // PowerPoint-made (`maskmove`): click 1 wipes A in along a path while C
  // turns, click 2 wipes B in as it grows, click 3 wipes all three out and
  // click 4 wipes them back in, each still holding its move, size or turn.
  const t = parseTimeline(new TextDecoder().decode(fs.readFileSync(new URL("./pptxPlay.maskmove.fixture.xml", import.meta.url))));
  const states = buildStates(t);

  it("grows each masked shape's border box while it plays and leaves applyState's values when it lands", async () => {
    expect(t.steps.length).toBe(5);
    const { tagged, all } = fakeSlide(t, true);
    const twin = fakeSlide(t, true);
    for (let k = 1; k < t.steps.length; k++) {
      const play = playStep(tagged, t, k, { w: W, h: H, states });
      // The masked shapes play inflated (the case the landing has to undo).
      expect(all.some((e) => e.props.has("border-left-width")), `click ${k} inflates`).toBe(true);
      play.finish();
      await play.finished;
      applyState(twin.tagged, states[k]!, W, H);
      all.forEach((el, i) => {
        expect(Object.fromEntries(el.props), `element ${i} after click ${k}`).toEqual(Object.fromEntries(twin.all[i]!.props));
        expect([...el.important], `element ${i} after click ${k}`).toEqual([...twin.all[i]!.important]);
        expect(el.running).toBe(0);
      });
    }
    // Something was held through the wipes: a move, a size and a turn.
    const held = all.map((e) => [e.props.get("translate"), e.props.get("scale"), e.props.get("rotate")]);
    expect(held.some(([tr]) => tr !== undefined)).toBe(true);
    expect(held.some(([, sc]) => sc !== undefined)).toBe(true);
    expect(held.some(([, , ro]) => ro !== undefined)).toBe(true);
  });
});
