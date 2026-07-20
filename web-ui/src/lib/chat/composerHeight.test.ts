import { describe, expect, it } from "vitest";
import { composerHeightForContent } from "./composerHeight";

describe("composerHeightForContent", () => {
  it("fits unresized drafts and caps long content", () => {
    expect(composerHeightForContent(96, null, 38, 200)).toBe(96);
    expect(composerHeightForContent(260, null, 38, 200)).toBe(200);
  });

  it("uses manually expanded room before resuming content growth", () => {
    const manual = { height: 120, contentHeight: 56 };
    expect(composerHeightForContent(96, manual, 38, 200)).toBe(120);
    expect(composerHeightForContent(148, manual, 38, 200)).toBe(148);
    expect(composerHeightForContent(260, manual, 38, 200)).toBe(200);
  });

  it("preserves a manual contraction while growing for additional content", () => {
    const manual = { height: 80, contentHeight: 104 };
    expect(composerHeightForContent(104, manual, 38, 200)).toBe(80);
    expect(composerHeightForContent(152, manual, 38, 200)).toBe(128);
  });
});
