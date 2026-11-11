import { describe, expect, it } from "vitest";
import { cropRaster, pageFragment, shownToView, snapshotSource, viewBox, viewToShown } from "./htmlRegion";

const view = { w: 800, h: 600 };

describe("shown ↔ view", () => {
  it("is the identity while the pane kept its size", () => {
    expect(shownToView({ x: 10, y: 20 }, view, view)).toEqual({ x: 10, y: 20 });
    expect(viewToShown({ x: 10, y: 20, w: 30, h: 40 }, view, view)).toEqual({ x: 10, y: 20, w: 30, h: 40 });
  });

  it("scales when the pane was resized after the snapshot", () => {
    const shown = { w: 400, h: 300 };
    expect(shownToView({ x: 100, y: 50 }, shown, view)).toEqual({ x: 200, y: 100 });
    expect(viewToShown({ x: 200, y: 100, w: 80, h: 40 }, shown, view)).toEqual({ x: 100, y: 50, w: 40, h: 20 });
  });

  it("survives a zero-size stage", () => {
    expect(shownToView({ x: 5, y: 5 }, { w: 0, h: 0 }, view)).toEqual({ x: 5, y: 5 });
  });
});

describe("viewBox", () => {
  it("spans the corners either way, snapped outward to whole px", () => {
    expect(viewBox({ x: 10.4, y: 20.6 }, { x: 5.2, y: 30.1 }, view)).toEqual({ x: 5, y: 20, w: 6, h: 11 });
  });
  it("clips to the viewport and drops a box outside it", () => {
    expect(viewBox({ x: 790, y: 590 }, { x: 900, y: 700 }, view)).toEqual({ x: 790, y: 590, w: 10, h: 10 });
    expect(viewBox({ x: 900, y: 0 }, { x: 950, y: 10 }, view)).toBeNull();
  });
});

describe("snapshotSource", () => {
  it("scales by the snapshot's density", () => {
    expect(snapshotSource({ x: 10, y: 20, w: 30, h: 40 }, view, { w: 1600, h: 1200 })).toEqual({
      x: 20,
      y: 40,
      w: 60,
      h: 80,
    });
  });
  it("rounds outward and clips to the PNG", () => {
    expect(snapshotSource({ x: 1, y: 1, w: 1, h: 1 }, { w: 3, h: 3 }, { w: 4, h: 4 })).toEqual({ x: 1, y: 1, w: 2, h: 2 });
    expect(snapshotSource({ x: 790, y: 0, w: 20, h: 10 }, view, { w: 1600, h: 1200 })).toEqual({
      x: 1580,
      y: 0,
      w: 20,
      h: 20,
    });
  });
  it("gives nothing for an empty snapshot", () => {
    expect(snapshotSource({ x: 0, y: 0, w: 10, h: 10 }, { w: 0, h: 0 }, { w: 10, h: 10 })).toBeNull();
    expect(snapshotSource({ x: 0, y: 0, w: 10, h: 10 }, view, { w: 0, h: 0 })).toBeNull();
  });
});

describe("cropRaster", () => {
  it("keeps the snapshot's pixels, capping the long side", () => {
    expect(cropRaster({ x: 0, y: 0, w: 600, h: 300 }, 1568)).toEqual({ w: 600, h: 300 });
    expect(cropRaster({ x: 0, y: 0, w: 3136, h: 1000 }, 1568)).toEqual({ w: 1568, h: 500 });
  });
});

describe("pageFragment", () => {
  it("places the box in the document by the page's scroll", () => {
    expect(pageFragment({ x: 10, y: 20, w: 30, h: 40 }, 0, 1500)).toBe("xywh=10,1520,30,40");
    expect(pageFragment({ x: 10, y: 20, w: 30, h: 40 }, 4.6, 0.4)).toBe("xywh=15,20,30,40");
  });
  it("ignores a scroll that isn't a number", () => {
    expect(pageFragment({ x: 1, y: 2, w: 3, h: 4 }, Number.NaN, Number.POSITIVE_INFINITY)).toBe("xywh=1,2,3,4");
  });
});
