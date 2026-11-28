import JSZip from "jszip";
import { describe, expect, it } from "vitest";
import { clearState, paraOffset, planState, playStep, transformScale, type ShapeInfo } from "./pptxPlay";
import type { TaggedSlide } from "./pptxTag";
import { parseTimeline, type BuildState, type GoldenSlide } from "./pptxTiming";

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

/** Enough of an element for `applyState` and `playStep`: inline style,
 *  attributes, a box, and `animate` that counts what runs. */
class FakeEl {
  style: Record<string, string> & { cssText: string };
  attrs = new Map<string, string>();
  parentElement: FakeEl | null = null;
  offsetWidth = 100;
  offsetHeight = 50;
  running = 0;
  started = 0;
  constructor(box: boolean) {
    const s: Record<string, string> = { opacity: "", translate: "", transform: "" };
    if (box) Object.assign(s, { left: "10px", top: "20px", width: "100px", height: "50px", position: "absolute" });
    const inline = new Set(["opacity", "translate", "transform", "left", "top", "width", "height", "position"]);
    this.style = new Proxy(s, {
      get: (t, k: string) => (k === "cssText" ? [...inline].filter((n) => t[n] !== "" && t[n] !== undefined).join(";") : (t[k] ?? "")),
      set: (t, k: string, v: string) => ((t[k] = v), true),
    }) as Record<string, string> & { cssText: string };
  }
  hasAttribute(n: string) {
    return this.attrs.has(n);
  }
  setAttribute(n: string, v: string) {
    this.attrs.set(n, v);
  }
  removeAttribute(n: string) {
    this.attrs.delete(n);
  }
  getBoundingClientRect() {
    return { left: 0, top: 0, right: 100, bottom: 50, width: 100, height: 50 };
  }
  animate() {
    this.running += 1;
    this.started += 1;
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

async function corpus(): Promise<{ name: string; part: string; t: ReturnType<typeof parseTimeline>; golden: GoldenSlide }[]> {
  const out = [];
  for (const file of fs.readdirSync(new URL("decks/", CORPUS)).filter((n) => n.endsWith(".pptx")).sort()) {
    const name = file.slice(0, -5);
    const zip = await JSZip.loadAsync(fs.readFileSync(new URL(`decks/${file}`, CORPUS)));
    const golden = JSON.parse(new TextDecoder().decode(fs.readFileSync(new URL(`goldens/${name}.json`, CORPUS)))) as { slides: GoldenSlide[] };
    for (const g of golden.slides) out.push({ name, part: g.part, t: parseTimeline(await zip.file(g.part)!.async("string")), golden: g });
  }
  return out;
}

describe("playStep lands every corpus click on its golden state", async () => {
  const slides = await corpus();
  for (const { name, part, t, golden } of slides) {
    it(`${name} ${part}`, async () => {
      const { tagged, all } = fakeSlide(t);
      const info = new Map<string, ShapeInfo>();
      for (const [id, s] of tagged.shapes) info.set(id, { paraCount: s.paraCount, paras: s.paras !== null });
      for (let k = 0; k < t.steps.length; k++) {
        const play = playStep(tagged, t, k, { w: 1280, h: 720 });
        play.finish();
        play.finish();
        await play.finished;
        const want = planState(info, golden.states[k]!);
        for (const [id, s] of tagged.shapes) {
          const el = s.el as unknown as FakeEl;
          const hidden = want.hideShapes.has(id);
          expect(el.style.opacity, `${id} after click ${k}`).toBe(hidden ? "0" : "");
          expect(el.hasAttribute("inert")).toBe(hidden);
          const off = want.moveShapes.get(id);
          expect(el.style.translate).toBe(off === undefined || (off[0] === 0 && off[1] === 0) ? "" : `${off[0] * 1280}px ${off[1] * 720}px`);
          (s.paras ?? []).forEach((p, i) => {
            expect((p as unknown as FakeEl).style.opacity, `${id}#${i} after click ${k}`).toBe(want.hideParas.get(id)?.has(i) === true ? "0" : "");
          });
        }
        // Nothing still runs, and nothing but the state is left inline.
        for (const el of all) expect(el.running).toBe(0);
      }
      // Off the slide, it is as drawn.
      clearState(tagged);
      for (const el of all) {
        expect(el.style.opacity).toBe("");
        expect(el.style.translate).toBe("");
        expect(el.hasAttribute("inert")).toBe(false);
      }
    });
  }

  it("animates the clicks that move something, and none under reduced motion", async () => {
    const s = slides.find((x) => x.name === "entrances" && x.part.endsWith("slide2.xml"))!;
    const { tagged, all } = fakeSlide(s.t);
    const play = playStep(tagged, s.t, 1, { w: 1280, h: 720 });
    expect(all.reduce((n, e) => n + e.running, 0)).toBe(1);
    play.finish();
    expect(all.reduce((n, e) => n + e.running, 0)).toBe(0);
    const before = all.reduce((n, e) => n + e.started, 0);
    playStep(tagged, s.t, 2, { w: 1280, h: 720, reduced: true });
    expect(all.reduce((n, e) => n + e.started, 0)).toBe(before);
  });
});
