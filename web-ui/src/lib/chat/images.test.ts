import { describe, expect, it } from "vitest";

import { attachmentSrc, tileBox } from "./images";

describe("attachment tiles", () => {
  it("take their width from the picture at one row height", () => {
    expect(tileBox({ width: 1400, height: 880 }, 112, 56, 240)).toEqual({ width: 178, height: 112 });
    expect(tileBox({ width: 600, height: 900 }, 112, 56, 240)).toEqual({ width: 75, height: 112 });
  });

  it("clamp panoramas and slivers, and are square until the size is known", () => {
    expect(tileBox({ width: 4000, height: 400 }, 112, 56, 240).width).toBe(240);
    expect(tileBox({ width: 100, height: 2000 }, 112, 56, 240).width).toBe(56);
    expect(tileBox(null, 112, 56, 240)).toEqual({ width: 112, height: 112 });
    expect(tileBox({ width: 0, height: 10 }, 56, 40, 140)).toEqual({ width: 56, height: 56 });
  });

  it("never scale a small picture up", () => {
    expect(tileBox({ width: 48, height: 32 }, 112, 24, 240)).toEqual({ width: 48, height: 32 });
  });

  it("render a draft straight from its base64", () => {
    expect(attachmentSrc({ media_type: "image/png", data: "QUJD", label: "x" })).toBe("data:image/png;base64,QUJD");
  });
});
