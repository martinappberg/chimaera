import { describe, expect, it, vi } from "vitest";
import { finishPdfRender, pdfRaster } from "./pdfCanvas";

describe("PDF raster bounds", () => {
  it("keeps ordinary pages at the requested resolution", () => {
    expect(pdfRaster(960, 540, 2)).toEqual({ width: 1920, height: 1080, scale: 2 });
  });

  it("keeps a thin transparent page's canvas nonempty", () => {
    expect(pdfRaster(9600, 5, 0.1)).toEqual({ width: 960, height: 1, scale: 0.1 });
  });

  it.each([[960, 540, 100], [1e8, 1, 1], [1, 1e8, 1], [1e8, 1e8, 6]])(
    "bounds both memory and edge length for %s × %s at scale %s",
    (w, h, desired) => {
      const raster = pdfRaster(w, h, desired);
      expect(raster.width * raster.height).toBeLessThanOrEqual(12_000_000);
      expect(raster.width).toBeGreaterThanOrEqual(1);
      expect(raster.height).toBeGreaterThanOrEqual(1);
      expect(Math.max(raster.width, raster.height)).toBeLessThanOrEqual(8192);
      expect(raster.scale).toBeLessThanOrEqual(desired);
    },
  );

  it.each([[0, 540, 1], [960, -1, 1], [Infinity, 540, 1], [960, NaN, 1], [960, 540, 0]])(
    "rejects unusable dimensions (%s, %s, %s)",
    (w, h, scale) => expect(() => pdfRaster(w, h, scale)).toThrow("invalid PDF page dimensions"),
  );
});

describe("PDF render cleanup", () => {
  it("releases a failed initialization before another render uses the canvas", async () => {
    let reserved = true;
    const error = new Error("Invalid canvas size");
    await expect(finishPdfRender({
      promise: Promise.reject(error),
      cancel: () => { reserved = false; },
    })).rejects.toBe(error);
    expect(reserved).toBe(false);
  });

  it("preserves the original failure if partial graphics also fail cleanup", async () => {
    const error = new Error("initialization failed");
    await expect(finishPdfRender({
      promise: Promise.reject(error),
      cancel: () => { throw new Error("cleanup failed"); },
    })).rejects.toBe(error);
  });

  it("does not cancel a completed render", async () => {
    const cancel = vi.fn();
    await finishPdfRender({ promise: Promise.resolve(), cancel });
    expect(cancel).not.toHaveBeenCalled();
  });
});
