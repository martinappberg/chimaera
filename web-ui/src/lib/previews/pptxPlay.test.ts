import { describe, expect, it } from "vitest";
import { planState, type ShapeInfo } from "./pptxPlay";
import type { BuildState } from "./pptxTiming";

/**
 * How a build state's keys land on drawn shapes (the timing contract's R5,
 * plus what the renderer can't draw apart). The DOM half, `applyState` and
 * `clearState`, is checked live against the corpus.
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
