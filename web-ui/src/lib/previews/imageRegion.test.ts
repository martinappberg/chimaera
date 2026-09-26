import { describe, expect, it } from "vitest";
import { clampRegion, cropSize, dragRegion, frameRegion, screenToImage, screenToPage } from "./imageRegion";

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

describe("screen → image pixels and PDF points", () => {
  it("inverts the image viewer's pan and zoom, at any zoom", () => {
    const origin = { x: 100, y: 50 };
    for (const view of [
      { scale: 1, tx: 0, ty: 0 },
      { scale: 0.25, tx: 40, ty: -12 },
      { scale: 3.5, tx: -900, ty: -410 },
    ]) {
      // Image pixel (123, 45) is drawn at origin + t + p·scale.
      const p = screenToImage(origin.x + view.tx + 123 * view.scale, origin.y + view.ty + 45 * view.scale, origin, view);
      expect(p.x).toBeCloseTo(123, 9);
      expect(p.y).toBeCloseTo(45, 9);
    }
  });
  it("maps a PDF page point at any zoom and scroll", () => {
    // Page 2 of the column: its slot's client top rises as the pane scrolls.
    for (const scale of [0.5, 1, 1.37, 4]) {
      for (const scrollTop of [0, 300, 5000]) {
        const slot = { x: 32, y: 14 + 792 * scale + 12 - scrollTop };
        const p = screenToPage(slot.x + 72 * scale, slot.y + 144 * scale, slot, scale);
        expect(p.x).toBeCloseTo(72, 9);
        expect(p.y).toBeCloseTo(144, 9);
      }
    }
  });
});

describe("dragRegion", () => {
  it("normalizes corners and snaps outward to whole units", () => {
    expect(dragRegion({ x: 30.6, y: 40.2 }, { x: 10.4, y: 20.9 }, 100, 100)).toEqual({ x: 10, y: 20, w: 21, h: 21 });
  });
  it("clips to the image or page", () => {
    expect(dragRegion({ x: -20, y: 90 }, { x: 20, y: 140 }, 100, 100)).toEqual({ x: 0, y: 90, w: 20, h: 10 });
    expect(dragRegion({ x: 0, y: 0 }, { x: 612.5, y: 900 }, 612, 792)).toEqual({ x: 0, y: 0, w: 612, h: 792 });
  });
  it("is null outside, or with no area", () => {
    expect(dragRegion({ x: 120, y: 0 }, { x: 150, y: 10 }, 100, 100)).toBeNull();
    expect(dragRegion({ x: 5, y: 5 }, { x: 5, y: 50 }, 100, 100)).toBeNull();
  });
});

describe("cropSize", () => {
  it("renders a PDF region at 2×", () => {
    expect(cropSize({ x: 0, y: 0, w: 200, h: 120 }, 2, 1568)).toEqual({ w: 400, h: 240, scale: 2 });
  });
  it("caps the long side at 1568", () => {
    const s = cropSize({ x: 0, y: 0, w: 1200, h: 300 }, 2, 1568);
    expect([s.w, s.h]).toEqual([1568, 392]);
    const img = cropSize({ x: 0, y: 0, w: 4000, h: 3000 }, 1, 1568);
    expect([img.w, img.h]).toEqual([1568, 1176]);
  });
  it("keeps a raster image's pixels 1:1 under the cap", () => {
    expect(cropSize({ x: 5, y: 5, w: 320, h: 240 }, 1, 1568)).toEqual({ w: 320, h: 240, scale: 1 });
  });
});
