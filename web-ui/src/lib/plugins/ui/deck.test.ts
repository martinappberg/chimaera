import { describe, expect, it } from "vitest";
import { clampPlace, clockText, isForward, notesAt, parseDeck, placeOf, readSync, syncName } from "./deck";

const RAW = [
  { title: "Title", file: "deck/slides.py", line: 6, builds: [{ src: "output:d/a.png" }], notes: "Welcome." },
  { title: "Plan", hidden: true, builds: ["output:d/b0.png", { src: "output:d/b1.png", notes: "Second point." }], notes: "Go slow." },
  { title: "Broken", tag: "stale", builds: [] },
];

describe("parseDeck", () => {
  it("reads slides, their builds, notes and place", () => {
    const d = parseDeck(RAW);
    expect(d.map((s) => s.title)).toEqual(["Title", "Plan", "Broken"]);
    expect(d[0]).toMatchObject({ file: "deck/slides.py", line: 6, hidden: false, tag: null });
    expect(d[1]?.hidden).toBe(true);
    expect(d[1]?.builds).toEqual([
      { src: "output:d/b0.png", notes: "", clip: null },
      { src: "output:d/b1.png", notes: "Second point.", clip: null },
    ]);
    expect(d[2]).toMatchObject({ tag: "stale", builds: [] });
  });
  it("drops what isn't a slide and names the nameless", () => {
    const d = parseDeck([null, 3, "x", { builds: [{ src: "" }, {}, 7, { src: "p.png" }], line: -2, file: 9 }]);
    expect(d).toHaveLength(1);
    expect(d[0]).toMatchObject({ title: "Slide 1", file: null, line: null });
    expect(d[0]?.builds).toEqual([{ src: "p.png", notes: "", clip: null }]);
    expect(parseDeck("nope")).toEqual([]);
    expect(parseDeck(undefined)).toEqual([]);
  });
  it("reads a build's animation, and plays a bad one at its own speed", () => {
    const d = parseDeck([
      {
        title: "A",
        builds: [
          { src: "a.png", clip: { src: "deck/slides/files/A/1.mp4", loop: true, rate: 2 } },
          { src: "b.png", clip: { src: "b.mp4", auto: true, rate: 900 } },
          { src: "c.png", clip: { loop: true } },
          { src: "d.png", clip: "d.mp4" },
        ],
      },
    ]);
    expect(d[0]?.builds.map((b) => b.clip)).toEqual([
      { src: "deck/slides/files/A/1.mp4", loop: true, auto: false, rate: 2 },
      { src: "b.mp4", loop: false, auto: true, rate: 1 },
      null,
      null,
    ]);
  });
  it("knows a move on from a move back", () => {
    expect(isForward({ slide: 1, build: 0 }, { slide: 1, build: 1 })).toBe(true);
    expect(isForward({ slide: 1, build: 2 }, { slide: 2, build: 0 })).toBe(true);
    expect(isForward({ slide: 1, build: 1 }, { slide: 1, build: 1 })).toBe(false);
    expect(isForward({ slide: 2, build: 0 }, { slide: 1, build: 3 })).toBe(false);
  });
  it("caps a runaway deck", () => {
    const many = Array.from({ length: 2000 }, (_, i) => ({ title: `S${i}`, builds: Array.from({ length: 100 }, () => "p.png") }));
    const d = parseDeck(many);
    expect(d).toHaveLength(600);
    expect(d[0]?.builds).toHaveLength(40);
  });
});

describe("places", () => {
  const d = parseDeck(RAW);
  it("clamps onto the deck", () => {
    expect(clampPlace(d, { slide: 9, build: 9 })).toEqual({ slide: 2, build: 0 });
    expect(clampPlace(d, { slide: 1, build: 9 })).toEqual({ slide: 1, build: 1 });
    expect(clampPlace(d, { slide: -1, build: -1 })).toEqual({ slide: 0, build: 0 });
    expect(clampPlace([], { slide: 3, build: 1 })).toEqual({ slide: 0, build: 0 });
  });
  it("finds a slide by name, across a re-render that moved it", () => {
    expect(placeOf(d, "Plan", "last")).toEqual({ slide: 1, build: 1 });
    expect(placeOf(d, "Plan", 0)).toEqual({ slide: 1, build: 0 });
    expect(placeOf(d, "Plan", 7)).toEqual({ slide: 1, build: 1 });
    expect(placeOf(d, "Gone", 0)).toBeNull();
  });
  it("joins a slide's notes with its build's", () => {
    expect(notesAt(d, { slide: 0, build: 0 })).toBe("Welcome.");
    expect(notesAt(d, { slide: 1, build: 1 })).toBe("Go slow.\n\nSecond point.");
    expect(notesAt(d, { slide: 2, build: 0 })).toBe("");
    expect(notesAt(d, { slide: 9, build: 0 })).toBe("");
  });
});

describe("sync", () => {
  it("names a channel per workspace, plugin and deck", () => {
    expect(syncName("w-1", "manimble", "deck-a")).toBe("chimaera-deck:w-1:manimble:deck-a");
  });
  it("reads only well-formed messages", () => {
    expect(readSync({ kind: "at", from: "a", title: "Plan", build: 1 })).toEqual({ kind: "at", from: "a", title: "Plan", build: 1 });
    expect(readSync({ kind: "where", from: "b" })).toEqual({ kind: "where", from: "b" });
    for (const bad of [null, "at", {}, { kind: "at", from: "a" }, { kind: "at", from: "", title: "x", build: 0 }, { kind: "at", from: "a", title: "x", build: -1 }, { kind: "at", from: "a", title: "x", build: 1.5 }, { kind: "go", from: "a" }]) {
      expect(readSync(bad)).toBeNull();
    }
  });
});

describe("clockText", () => {
  it("reads as a presenter's timer", () => {
    expect(clockText(0)).toBe("0:00");
    expect(clockText(247_900)).toBe("4:07");
    expect(clockText(3_735_000)).toBe("1:02:15");
    expect(clockText(-5)).toBe("0:00");
  });
});
