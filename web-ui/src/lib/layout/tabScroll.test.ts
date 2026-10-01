import { describe, expect, it } from "vitest";
import { revealTabScrollLeft, tabFadeWidths, tabInView } from "./tabScroll";

describe("tab strip reveal", () => {
  it("keeps the close edge in view when an agent tab fills the narrow strip", () => {
    const tab = { left: 218, width: 169 };
    const scroll = revealTabScrollLeft(194, 169, 515, tab);
    expect(scroll).toBe(218);
    expect(tabInView(scroll, 169, tab)).toBe(true);
    expect(tabFadeWidths(scroll, 169, tab)).toEqual({ left: 0, right: 0 });
    expect(revealTabScrollLeft(scroll, 169, 515, tab)).toBe(scroll);
  });

  it("re-reveals the trailing tab after the dropdown consumes strip width", () => {
    const tab = { left: 398, width: 120 };
    const scroll = revealTabScrollLeft(253.5, 240, 526, tab);
    expect(scroll).toBe(286);
    expect(tabInView(scroll, 240, tab)).toBe(true);
    expect(tabFadeWidths(scroll, 240, tab)).toEqual({ left: 24, right: 8 });
  });

  it("reveals the close edge without adding gutters when a tab nearly fills the strip", () => {
    const tab = { left: 200, width: 180 };
    const scroll = revealTabScrollLeft(176, 200, 600, tab);
    expect(scroll).toBe(200);
    expect(tabFadeWidths(scroll, 200, tab)).toEqual({ left: 0, right: 20 });
    expect(revealTabScrollLeft(scroll, 200, 600, tab)).toBe(scroll);
  });

  it("aligns a previously clipped leading edge flush with the strip", () => {
    const tab = { left: 200, width: 180 };
    const scroll = revealTabScrollLeft(225, 240, 600, tab);
    expect(scroll).toBe(200);
    expect(tabFadeWidths(scroll, 240, tab).left).toBe(0);
    expect(revealTabScrollLeft(scroll, 240, 600, tab)).toBe(scroll);
  });

  it("does not leave the preceding tab's empty tail before a newly revealed tab", () => {
    expect(revealTabScrollLeft(0, 227, 672, { left: 124, width: 180 })).toBe(124);
  });

  it("preserves a revealed tab's position and normal fade margins", () => {
    const tab = { left: 200, width: 180 };
    expect(revealTabScrollLeft(150, 300, 600, tab)).toBe(150);
    expect(tabFadeWidths(150, 300, tab)).toEqual({ left: 24, right: 24 });
  });

  it("clamps at both ends and prioritizes the close edge below the tab floor", () => {
    expect(revealTabScrollLeft(150, 200, 600, { left: 0, width: 120 })).toBe(0);
    expect(revealTabScrollLeft(0, 200, 120, { left: 0, width: 120 })).toBe(0);
    expect(revealTabScrollLeft(100, 40, 300, { left: 120, width: 64 })).toBe(144);
  });

  it("keeps fades while the user scrolls the active tab out of view", () => {
    const tab = { left: 200, width: 180 };
    expect(tabInView(0, 200, tab)).toBe(false);
    expect(tabFadeWidths(0, 200, tab)).toEqual({ left: 24, right: 24 });
  });
});
