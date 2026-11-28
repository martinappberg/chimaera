import JSZip from "jszip";
import { describe, expect, it } from "vitest";
import { clearState, paraOffset, planState, playStep, transformScale, type ShapeInfo } from "./pptxPlay";
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
  style: Record<string, string> & { cssText: string; getPropertyValue(p: string): string; setProperty(p: string, v: string): void; removeProperty(p: string): void };
  attrs = new Map<string, string>();
  parentElement: FakeEl | null = null;
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
      setProperty: (p: string, v: string) => void (v === "" ? this.props.delete(p) : this.props.set(p, v)),
      removeProperty: (p: string) => void this.props.delete(p),
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
  querySelectorAll() {
    return [];
  }
  closest() {
    return null;
  }
  getBoundingClientRect() {
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
function fakeSlide(t: ReturnType<typeof parseTimeline>): { tagged: TaggedSlide; all: FakeEl[] } {
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
});
