import { describe, expect, it } from "vitest";
import { wantsCoverStill } from "./paneView";

describe("wantsCoverStill", () => {
  const base = { shownNow: true, hiding: true, covered: true, hostDraws: true, stillUp: false };

  it("takes a still when a menu comes over a view on screen", () => {
    expect(wantsCoverStill(base)).toBe(true);
  });

  it("takes none for any other hiding, a hidden view, a host without stills, or one already up", () => {
    expect(wantsCoverStill({ ...base, covered: false })).toBe(false); // a park, a drag, a load
    expect(wantsCoverStill({ ...base, shownNow: false })).toBe(false); // nothing on screen to capture
    expect(wantsCoverStill({ ...base, hiding: false })).toBe(false);
    expect(wantsCoverStill({ ...base, hostDraws: false })).toBe(false);
    expect(wantsCoverStill({ ...base, stillUp: true })).toBe(false);
  });
});
