import { describe, expect, it } from "vitest";
import { clampRegion, frameRegion } from "./imageRegion";

describe("clampRegion", () => {
  it("keeps a region inside the image as is", () => {
    expect(clampRegion({ x: 10, y: 20, w: 30, h: 40 }, 100, 100)).toEqual({ x: 10, y: 20, w: 30, h: 40 });
  });
  it("clips to the image and normalizes negative sizes", () => {
    expect(clampRegion({ x: 90, y: -10, w: 50, h: 30 }, 100, 100)).toEqual({ x: 90, y: 0, w: 10, h: 20 });
    expect(clampRegion({ x: 50, y: 50, w: -20, h: -10 }, 100, 100)).toEqual({ x: 30, y: 40, w: 20, h: 10 });
  });
  it("drops regions that miss the image or are not numbers", () => {
    expect(clampRegion({ x: 200, y: 0, w: 10, h: 10 }, 100, 100)).toBeNull();
    expect(clampRegion({ x: 0, y: 0, w: 0, h: 10 }, 100, 100)).toBeNull();
    expect(clampRegion({ x: Number.NaN, y: 0, w: 10, h: 10 }, 100, 100)).toBeNull();
  });
});

describe("frameRegion", () => {
  const opts = { fill: 0.6, minScale: 0.25, maxScale: 8 };
  it("zooms the region to fill the tighter axis and centers it", () => {
    const v = frameRegion({ x: 100, y: 50, w: 100, h: 50 }, 1000, 500, opts);
    // 0.6 × 1000 / 100 = 6, 0.6 × 500 / 50 = 6.
    expect(v.scale).toBe(6);
    // The region's center (150, 75) lands on the viewport's center.
    expect(v.tx + 150 * v.scale).toBe(500);
    expect(v.ty + 75 * v.scale).toBe(250);
  });
  it("never zooms past the ceiling or below fit", () => {
    expect(frameRegion({ x: 0, y: 0, w: 1, h: 1 }, 800, 600, opts).scale).toBe(8);
    expect(frameRegion({ x: 0, y: 0, w: 5000, h: 5000 }, 800, 600, opts).scale).toBe(0.25);
  });
});
