import { describe, expect, it } from "vitest";
import { BAND_MAX_FRAC, bandFraction, REF_BAND_FRAC } from "./dnd";

// The "@ reference" / "link to agent" band is the drop target that means
// "into this input". Its share of the pane decides whether a release on the
// composer references a file or falls through to a tile zone (and moves the
// dragged tab into the pane).
describe("bandFraction", () => {
  it("is the fixed share where the pane shows no input area (a terminal's agent)", () => {
    expect(bandFraction(800, null)).toBe(REF_BAND_FRAC);
  });

  it("keeps the fixed share while the input area fits inside it", () => {
    // A one-line composer: 90px of an 800px pane.
    expect(bandFraction(800, 710)).toBe(REF_BAND_FRAC);
  });

  it("grows to hold a taller input area, so its top is still the band", () => {
    // A long draft with attachments: 300px of an 800px pane.
    expect(bandFraction(800, 500)).toBeCloseTo(0.375);
  });

  it("holds the input area of a short pane", () => {
    // A split-down chat: the same 120px composer is 40% of a 300px pane.
    expect(bandFraction(300, 180)).toBeCloseTo(0.4);
  });

  it("never takes more than the cap: the tile zones keep the rest", () => {
    expect(bandFraction(300, 60)).toBe(BAND_MAX_FRAC);
  });

  it("falls back to the fixed share for a pane with no height", () => {
    expect(bandFraction(0, 0)).toBe(REF_BAND_FRAC);
  });
});
