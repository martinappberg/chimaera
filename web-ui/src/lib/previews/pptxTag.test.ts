import { describe, expect, it } from "vitest";
import { boxExact, boxMatches, matchByBox, planByOrder, type Box, type Candidate } from "./pptxTag";

/**
 * The matching behind `tagSlide` (the DOM half is driven live: the UI's
 * Vitest runs in node, with no DOM). Draw order is trusted only while every
 * child sits at its node's box; past that, shapes are found by their boxes.
 */

const box = (x: number, y: number, w = 100, h = 50): Box => ({ x, y, w, h });
const cand = (b: Box | null, placeholder = false): Candidate => ({ box: b, placeholder });

describe("boxMatches", () => {
  it("allows the edit layer's 0.75 px", () => {
    expect(boxMatches(box(10.5, 20), box(10, 20))).toBe(true);
    expect(boxMatches(box(10.8, 20), box(10, 20))).toBe(false);
  });

  it("takes a zero-height line drawn 1 px thick", () => {
    expect(boxMatches(box(0, 0, 100, 1), box(0, 0, 100, 0))).toBe(true);
    expect(boxMatches(box(0, 0, 100, 1), box(0, 0, 100, 2))).toBe(false);
  });

  it("takes a box grown to fit its text, never a shrunk or moved one", () => {
    expect(boxMatches(box(10, 20, 100, 180), box(10, 20, 100, 50))).toBe(true);
    expect(boxMatches(box(10, 20, 140, 50), box(10, 20, 100, 50))).toBe(true);
    expect(boxMatches(box(10, 20, 100, 49), box(10, 20, 100, 50))).toBe(false);
    expect(boxMatches(box(11, 20, 100, 180), box(10, 20, 100, 50))).toBe(false);
    expect(boxExact(box(10, 20, 100, 180), box(10, 20, 100, 50))).toBe(false);
  });
});

describe("planByOrder", () => {
  it("takes the last N children, in order, behind the master's and layout's", () => {
    const cands = [cand(box(0, 0, 960, 540)), cand(box(1, 1)), cand(box(2, 2)), cand(box(3, 3))];
    expect(planByOrder(cands, [box(2, 2), box(3, 3)])).toEqual({ at: [2, 3], failed: false });
  });

  it("skips the renderer's error placeholder without failing the slide", () => {
    const cands = [cand(box(1, 1)), cand(box(2, 2), true), cand(box(3, 3))];
    expect(planByOrder(cands, [box(1, 1), box(2, 2), box(3, 3)])).toEqual({ at: [0, null, 2], failed: false });
  });

  it("fails the slide on a child at the wrong box", () => {
    const cands = [cand(box(1, 1)), cand(box(9, 9))];
    expect(planByOrder(cands, [box(1, 1), box(2, 2)])).toEqual({ at: [0, null], failed: true });
  });

  it("fails the slide when there are fewer children than nodes", () => {
    expect(planByOrder([cand(box(1, 1))], [box(1, 1), box(2, 2)])).toEqual({ at: [null, null], failed: true });
  });

  it("fails the slide on a child that isn't positioned", () => {
    expect(planByOrder([cand(null)], [box(1, 1)]).failed).toBe(true);
  });
});

describe("matchByBox", () => {
  it("finds the last child at the box that isn't taken", () => {
    const cands = [cand(box(1, 1)), cand(box(5, 5)), cand(box(1, 1))];
    expect(matchByBox(cands, box(1, 1), new Set())).toBe(2);
    expect(matchByBox(cands, box(1, 1), new Set([2]))).toBe(0);
    expect(matchByBox(cands, box(7, 7), new Set())).toBe(null);
  });

  it("prefers an exact box to one grown from the same origin", () => {
    const cands = [cand(box(1, 1, 100, 50)), cand(box(1, 1, 100, 300))];
    expect(matchByBox(cands, box(1, 1, 100, 50), new Set())).toBe(0);
    expect(matchByBox(cands, box(1, 1, 100, 50), new Set([0]))).toBe(1);
  });

  it("trusts the draw order for a text box grown to fit", () => {
    const cands = [cand(box(1, 1)), cand(box(5, 5, 100, 400))];
    expect(planByOrder(cands, [box(1, 1), box(5, 5, 100, 50)])).toEqual({ at: [0, 1], failed: false });
  });

  it("never picks a placeholder", () => {
    expect(matchByBox([cand(box(1, 1), true)], box(1, 1), new Set())).toBe(null);
  });
});
