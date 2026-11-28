import { afterEach, describe, expect, it, vi } from "vitest";
import type { Playing } from "./pptxPlay";
import type { TaggedSlide } from "./pptxTag";
import { FLICK_MS, slidePalette, SlideTimings, StageMotion } from "./pptxStage.svelte";

/**
 * The click's motion apart from the view: which forward click plays, which
 * pass plays it, the click playing, the next click planned ahead, palettes
 * and timings. The stage itself (tagging, animating) is verified live.
 */

const fs = (globalThis as unknown as { process: { getBuiltinModule(id: "node:fs"): { readFileSync(p: URL, e: "utf8"): string } } }).process.getBuiltinModule("node:fs");
const CASES = new URL("./fixtures/pptx-timing/cases/", import.meta.url);

const tagged = (): TaggedSlide => ({ shapes: new Map(), fellBack: false, paraMismatches: 0 });
function playing(): { p: Playing; land: () => void; finished: number } {
  let resolve!: () => void;
  const out = { finished: 0, land: () => resolve(), p: undefined as unknown as Playing };
  out.p = { finished: new Promise<void>((r) => (resolve = r)), finish: () => (out.finished++, resolve()), approximate: false };
  return out;
}

afterEach(() => vi.useRealTimers());

describe("which forward click plays", () => {
  it("a click in the show plays; one under 400 ms after the last lands; one after a pause plays again", () => {
    const m = new StageMotion();
    m.moved({ slide: 0, build: 1 }, { click: true }, true, false, 1000);
    expect(m.toPlay).toEqual({ slide: 0, build: 1 });
    m.moved({ slide: 0, build: 2 }, { click: true }, true, false, 1000 + FLICK_MS - 1);
    expect(m.toPlay).toBeNull();
    m.moved({ slide: 0, build: 3 }, { click: true }, true, false, 1000 + 2 * FLICK_MS + 10);
    expect(m.toPlay).toEqual({ slide: 0, build: 3 });
  });

  it("an advance by itself plays without counting as a click; reduced motion, a jump and the presenter's view land", () => {
    const m = new StageMotion();
    m.moved({ slide: 0, build: 1 }, { click: true }, true, false, 1000);
    m.moved({ slide: 0, build: 2 }, { click: true, auto: true }, true, false, 1100);
    expect(m.toPlay).toEqual({ slide: 0, build: 2 });
    expect(m.lastClickAt).toBe(1000);
    m.moved({ slide: 0, build: 3 }, { click: true }, true, true, 5000);
    expect(m.toPlay).toBeNull();
    m.moved({ slide: 1, build: 0 }, {}, true, false, 9000);
    expect(m.toPlay).toBeNull();
    m.moved({ slide: 1, build: 1 }, { click: true }, false, false, 20000);
    expect(m.toPlay).toBeNull();
  });

  it("a move lands the click that is playing", () => {
    const m = new StageMotion();
    const p = playing();
    m.started({ p: p.p, slide: 0, build: 1, tagged: tagged(), gen: "1:0" }, () => {});
    expect(m.animating).toBe(true);
    m.moved({ slide: 0, build: 2 }, { click: true }, true, false, 0);
    expect(p.finished).toBe(1);
    expect(m.playing).toBeNull();
    expect(m.animating).toBe(false);
  });
});

describe("which pass plays it", () => {
  it("only the pass that brought the place, and only in the show", () => {
    const m = new StageMotion();
    m.moved({ slide: 0, build: 1 }, { click: true }, true, false, 0);
    expect(m.pass(0, 1, true)).toEqual({ slide: 0, build: 1 });
    m.toPlay = { slide: 0, build: 1 };
    expect(m.pass(0, 1, true)).toBeNull();
    m.toPlay = { slide: 0, build: 2 };
    expect(m.pass(0, 2, false)).toBeNull();
    expect(m.toPlay).toBeNull();
  });

  it("a click keeps playing only on its own pass: same place, version and drawing", () => {
    const m = new StageMotion();
    const t = tagged();
    m.started({ p: playing().p, slide: 2, build: 1, tagged: t, gen: "3:0" }, () => {});
    expect(m.owns(2, 1, true, false, "3:0", t)).toBe(true);
    expect(m.owns(2, 1, true, false, "3:1", t)).toBe(false);
    expect(m.owns(2, 1, true, true, "3:0", t)).toBe(false);
    expect(m.owns(2, 1, true, false, "3:0", tagged())).toBe(false);
    expect(m.owns(2, 2, true, false, "3:0", t)).toBe(false);
    expect(m.owns(2, 1, false, false, "3:0", t)).toBe(false);
  });

  it("a landed click's follow-up runs only while it is still the one playing", async () => {
    const m = new StageMotion();
    const a = playing();
    let landed = 0;
    m.started({ p: a.p, slide: 0, build: 1, tagged: tagged(), gen: "g" }, () => landed++);
    m.stop();
    await a.p.finished;
    expect(landed).toBe(0);
    const b = playing();
    m.started({ p: b.p, slide: 0, build: 2, tagged: tagged(), gen: "g" }, () => landed++);
    b.land();
    await b.p.finished;
    await Promise.resolve();
    expect(landed).toBe(1);
    expect(m.playing).toBeNull();
  });
});

describe("the next click, planned ahead", () => {
  it("is used only for its own place and version", () => {
    const m = new StageMotion();
    const p = {} as never;
    m.warmed = { slide: 1, build: 2, gen: "4:0", p };
    expect(m.prepared(1, 2, "4:0")).toBe(p);
    expect(m.prepared(1, 3, "4:0")).toBeNull();
    expect(m.prepared(1, 2, "4:1")).toBeNull();
  });

  it("is dropped on every pass, planned only after the show stands still, never while a click plays", () => {
    vi.useFakeTimers();
    const m = new StageMotion();
    m.warmed = { slide: 1, build: 2, gen: "g", p: {} as never };
    let asked = 0;
    const src = { live: () => true, next: () => (asked++, { slide: 1, build: 2 }), tagged: () => undefined, timeline: () => null, options: () => ({ w: 1280, h: 720 }) };
    m.warm("g", src, true);
    expect(m.warmed).toBeNull();
    vi.advanceTimersByTime(150);
    expect(asked).toBe(0);
    vi.advanceTimersByTime(100);
    expect(asked).toBe(1);
    m.started({ p: playing().p, slide: 0, build: 1, tagged: tagged(), gen: "g" }, () => {});
    m.warm("g", src, true);
    vi.advanceTimersByTime(500);
    expect(asked).toBe(1);
    m.stop();
    m.warm("g", src, false);
    vi.advanceTimersByTime(500);
    expect(asked).toBe(1);
  });
});

describe("palettes and timings", () => {
  it("a slide's palette is its master's theme and colour map, its override on top, kept per part", () => {
    const pres = {
      slides: [{ slidePath: "ppt/slides/slide1.xml", layoutIndex: "L1", colorMapOverride: new Map([["bg1", "dk1"]]), colorMapOverrideMode: "override" }],
      layoutToMaster: new Map([["L1", "M1"]]),
      masters: new Map([["M1", { colorMap: new Map([["bg1", "lt1"], ["tx1", "dk1"]]) }]]),
      masterToTheme: new Map([["M1", "T1"]]),
      themes: new Map([["T1", { colorScheme: new Map([["accent2", "C0504D"]]) }]]),
    } as never;
    const cache = new Map();
    const p = slidePalette(pres, 0, cache)!;
    expect(p.scheme.get("accent2")).toBe("C0504D");
    expect(p.map.get("bg1")).toBe("dk1");
    expect(p.map.get("tx1")).toBe("dk1");
    expect(slidePalette(pres, 0, cache)).toBe(p);
    expect(slidePalette(pres, 1, cache)).toBeNull();
  });

  it("a slide's timing is parsed once, on first ask, and counted for paging", () => {
    const xml = fs.readFileSync(new URL("split-any-behaviour.xml", CASES), "utf8");
    const t = new SlideTimings();
    const slides = [{ slidePath: "a", sourceXml: xml }, { slidePath: "b" }];
    t.reset(slides);
    expect(t.has("a")).toBe(false);
    expect(t.counts(slides)).toEqual([1, 1]);
    const got = t.of(slides[0], () => null)!;
    expect(got.states.length).toBe(3);
    expect(t.of(slides[0], () => null)).toBe(got);
    expect(t.counts(slides)).toEqual([3, 1]);
    expect(t.of(slides[1], () => null)).toBeNull();
    t.replace("a", xml);
    expect(t.has("a")).toBe(false);
  });
});
