import { describe, expect, it } from "vitest";
import {
  fragmentLabel,
  fragmentReveal,
  parseClock,
  parseEmbedFragment,
  parseRange,
  tableWindow,
} from "./fragment";

describe("parseEmbedFragment", () => {
  it("reads GitHub line ranges", () => {
    expect(parseEmbedFragment("L10-L30")).toEqual({ lines: { start: 10, end: 30 } });
    expect(parseEmbedFragment("#L12")).toEqual({ lines: { start: 12, end: 12 } });
    expect(parseEmbedFragment("L12C3-L14C1")).toEqual({ lines: { start: 12, end: 14 } });
    expect(parseEmbedFragment("L5-20")).toEqual({ lines: { start: 5, end: 20 } });
    expect(parseEmbedFragment("L9-L3")).toEqual({ lines: { start: 9, end: 9 } });
    expect(parseEmbedFragment("L0")).toEqual({});
  });

  it("reads PDF pages and regions", () => {
    expect(parseEmbedFragment("page=3")).toEqual({ page: 3 });
    expect(parseEmbedFragment("page=4&xywh=160,120,320,240")).toEqual({
      page: 4,
      region: { x: 160, y: 120, w: 320, h: 240, unit: "pixel" },
    });
    expect(parseEmbedFragment("xywh=percent:10,10,50,50").region).toEqual({
      x: 10,
      y: 10,
      w: 50,
      h: 50,
      unit: "percent",
    });
    expect(parseEmbedFragment("xywh=1,2,0,5")).toEqual({});
    expect(parseEmbedFragment("page=0")).toEqual({});
  });

  it("reads media moments in seconds or clock time", () => {
    expect(parseEmbedFragment("t=30,45")).toEqual({ time: { start: 30, end: 45 } });
    expect(parseEmbedFragment("t=1:02")).toEqual({ time: { start: 62 } });
    expect(parseEmbedFragment("t=,10")).toEqual({ time: { start: 0, end: 10 } });
    expect(parseEmbedFragment("t=npt:1:00:05.5").time).toEqual({ start: 3605.5 });
    expect(parseEmbedFragment("t=20,10")).toEqual({ time: { start: 20 } });
    expect(parseClock("nope")).toBeNull();
  });

  it("reads table rows, sheets and ranges", () => {
    expect(parseEmbedFragment("row=5-9")).toEqual({ rows: { start: 5, end: 9 } });
    expect(parseEmbedFragment("row=4")).toEqual({ rows: { start: 4, end: 4 } });
    expect(parseEmbedFragment("row=4-*")).toEqual({ rows: { start: 4, end: null } });
    expect(parseEmbedFragment("row=9-4")).toEqual({});
    expect(parseEmbedFragment("sheet=Summary%20Q3&range=B2:F9")).toEqual({
      sheet: "Summary Q3",
      range: { c1: 2, r1: 2, c2: 6, r2: 9 },
    });
    expect(parseRange("AA10")).toEqual({ c1: 27, r1: 10, c2: 27, r2: 10 });
    expect(parseRange("A:C")).toEqual({ c1: 1, r1: null, c2: 3, r2: null });
    expect(parseRange("$B$2:$D$4")).toEqual({ c1: 2, r1: 2, c2: 4, r2: 4 });
    expect(parseRange("B2:??")).toBeNull();
  });

  it("reads cells, slides, and falls back to a heading anchor", () => {
    expect(parseEmbedFragment("cell=7")).toEqual({ cell: 7 });
    expect(parseEmbedFragment("slide=3")).toEqual({ slide: 3 });
    expect(parseEmbedFragment("Results")).toEqual({ anchor: "Results" });
    expect(parseEmbedFragment("#my-section%20two")).toEqual({ anchor: "my-section two" });
    expect(parseEmbedFragment("")).toEqual({});
    expect(parseEmbedFragment(null)).toEqual({});
    expect(parseEmbedFragment("bogus=1")).toEqual({});
  });
});

describe("fragmentLabel / fragmentReveal", () => {
  it("names the piece shown", () => {
    expect(fragmentLabel(parseEmbedFragment("L10-L30"))).toBe("lines 10–30");
    expect(fragmentLabel(parseEmbedFragment("L7"))).toBe("line 7");
    expect(fragmentLabel(parseEmbedFragment("page=3&xywh=1,2,3,4"))).toBe("page 3 · region");
    expect(fragmentLabel(parseEmbedFragment("row=2-*"))).toBe("rows 2–end");
    expect(fragmentLabel(parseEmbedFragment("sheet=S&range=A1:F20"))).toBe("S!A1:F20");
    expect(fragmentLabel(parseEmbedFragment("t=30,45"))).toBe("0:30–0:45");
    expect(fragmentLabel(parseEmbedFragment("t=3725"))).toBe("from 1:02:05");
    expect(fragmentLabel(parseEmbedFragment("Results"))).toBe("# Results");
    expect(fragmentLabel({})).toBe("");
  });

  it("opens the full viewer at the same spot", () => {
    expect(fragmentReveal(parseEmbedFragment("L10-L30"))).toEqual({ line: 10, endLine: 30 });
    expect(fragmentReveal(parseEmbedFragment("page=3"))).toEqual({ line: 1, page: 3 });
    expect(fragmentReveal(parseEmbedFragment("t=5,9"))).toEqual({ line: 1, time: { start: 5, end: 9 } });
    expect(fragmentReveal(parseEmbedFragment("xywh=1,2,3,4"))).toEqual({
      line: 1,
      region: { x: 1, y: 2, w: 3, h: 4 },
    });
    expect(fragmentReveal(parseEmbedFragment("cell=2"))).toEqual({ line: 1, cell: 2 });
    expect(fragmentReveal(parseEmbedFragment("Results"))).toBeUndefined();
    expect(fragmentReveal({})).toBeUndefined();
  });
});

describe("tableWindow", () => {
  it("counts the header as row 1, per RFC 7111", () => {
    expect(tableWindow({ start: 2, end: 4 }, true)).toEqual({ offset: 0, limit: 3 });
    expect(tableWindow({ start: 5, end: 7 }, true)).toEqual({ offset: 3, limit: 3 });
    // row=1 selects the header: the data starts right after it.
    expect(tableWindow({ start: 1, end: 3 }, true)).toEqual({ offset: 0, limit: 2 });
  });

  it("starts data at row 1 without a header", () => {
    expect(tableWindow({ start: 1, end: 3 }, false)).toEqual({ offset: 0, limit: 3 });
  });

  it("peeks without a selection and caps a long one", () => {
    expect(tableWindow(undefined, true)).toEqual({ offset: 0, limit: 10 });
    expect(tableWindow(undefined, true, 6)).toEqual({ offset: 0, limit: 6 });
    expect(tableWindow({ start: 2, end: 10_000 }, true)).toEqual({ offset: 0, limit: 50 });
    expect(tableWindow({ start: 12, end: null }, true)).toEqual({ offset: 10, limit: 10 });
  });
});
