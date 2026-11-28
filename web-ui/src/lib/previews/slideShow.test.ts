import { describe, expect, it } from "vitest";
import { deckStep, showEdge, showPosition, showReach, showStep } from "./slideShow";

const H = [false, true, true, false, false, true];

describe("showStep", () => {
  it("skips hidden slides in both directions", () => {
    expect(showStep(H, 0, 1)).toBe(3);
    expect(showStep(H, 3, 1)).toBe(4);
    expect(showStep(H, 3, -1)).toBe(0);
  });
  it("stays put when nothing shown lies that way", () => {
    expect(showStep(H, 4, 1)).toBe(4);
    expect(showStep(H, 0, -1)).toBe(0);
    expect(showStep([true, true], 0, 1)).toBe(0);
  });
  it("leaves a hidden slide the show started on", () => {
    expect(showStep(H, 1, 1)).toBe(3);
    expect(showStep(H, 2, -1)).toBe(0);
  });
});

describe("showEdge", () => {
  it("is the outermost slide that isn't hidden", () => {
    expect(showEdge(H, "first")).toBe(0);
    expect(showEdge(H, "last")).toBe(4);
    expect(showEdge([true, false, true], "first")).toBe(1);
  });
  it("falls back to the deck's ends", () => {
    expect(showEdge([true, true], "first")).toBe(0);
    expect(showEdge([true, true], "last")).toBe(1);
    expect(showEdge([], "last")).toBe(0);
  });
});

describe("showPosition", () => {
  it("counts only the slides of the show", () => {
    expect(showPosition(H, 0)).toEqual({ n: 1, of: 3 });
    expect(showPosition(H, 3)).toEqual({ n: 2, of: 3 });
    expect(showPosition(H, 4)).toEqual({ n: 3, of: 3 });
  });
  it("reads a hidden slide as the shown one before it", () => {
    expect(showPosition(H, 2)).toEqual({ n: 1, of: 3 });
    expect(showPosition([true, false], 0)).toEqual({ n: 1, of: 1 });
    expect(showPosition([true], 0)).toEqual({ n: 0, of: 0 });
  });
});

describe("deckStep", () => {
  const builds = [1, 3, 2, 0, 2];
  const hidden = [false, false, true, false, false];
  it("walks a slide's builds before leaving it", () => {
    expect(deckStep(builds, hidden, { slide: 1, build: 0 }, 1, true)).toEqual({ slide: 1, build: 1 });
    expect(deckStep(builds, hidden, { slide: 1, build: 2 }, -1, true)).toEqual({ slide: 1, build: 1 });
  });
  it("enters the next slide on its first build and the previous on its last", () => {
    expect(deckStep(builds, hidden, { slide: 0, build: 0 }, 1, true)).toEqual({ slide: 1, build: 0 });
    expect(deckStep(builds, hidden, { slide: 3, build: 0 }, -1, false)).toEqual({ slide: 2, build: 1 });
  });
  it("skips hidden slides only in a show", () => {
    expect(deckStep(builds, hidden, { slide: 1, build: 2 }, 1, true)).toEqual({ slide: 3, build: 0 });
    expect(deckStep(builds, hidden, { slide: 1, build: 2 }, 1, false)).toEqual({ slide: 2, build: 0 });
    expect(deckStep(builds, hidden, { slide: 3, build: 0 }, -1, true)).toEqual({ slide: 1, build: 2 });
  });
  it("stays put at the ends and clamps a stale build", () => {
    expect(deckStep(builds, hidden, { slide: 4, build: 1 }, 1, true)).toEqual({ slide: 4, build: 1 });
    expect(deckStep(builds, hidden, { slide: 0, build: 0 }, -1, true)).toEqual({ slide: 0, build: 0 });
    expect(deckStep(builds, hidden, { slide: 1, build: 9 }, 1, true)).toEqual({ slide: 3, build: 0 });
    expect(deckStep(builds, hidden, { slide: 9, build: 0 }, 1, true)).toEqual({ slide: 9, build: 0 });
  });
});

describe("showReach", () => {
  const hidden = [false, true, false];
  it("names the slides a click lands on, hidden ones skipped in a show", () => {
    expect(showReach(hidden, 2, true)).toEqual([0]);
    expect(showReach(hidden, 0, true)).toEqual([2]);
    expect(showReach(hidden, 2, false)).toEqual([1]);
    expect(showReach([false, false, false], 1, true)).toEqual([0, 2]);
    expect(showReach([false], 0, true)).toEqual([]);
  });
  it("is what a step back over a hidden slide needs: the build count of the slide it lands on", () => {
    // Slide 0 has three builds, slide 1 is hidden: parsing by index (slide 1)
    // would leave slide 0 counted as one build, and back would land on 0.
    const unparsed = [1, 1, 2];
    const parsed = [3, 1, 2];
    expect(deckStep(unparsed, hidden, { slide: 2, build: 0 }, -1, true)).toEqual({ slide: 0, build: 0 });
    for (const j of showReach(hidden, 2, true)) unparsed[j] = parsed[j]!;
    expect(deckStep(unparsed, hidden, { slide: 2, build: 0 }, -1, true)).toEqual({ slide: 0, build: 2 });
  });
});
