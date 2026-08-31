import { describe, expect, it } from "vitest";
import {
  advanceTailWindow,
  autoPageEarlier,
  pageEarlier,
  pageLater,
  restoreWindow,
  tailWindow,
  toArrayCoords,
  TRANSCRIPT_WINDOW,
  type TranscriptWindow,
} from "./transcriptWindow";

describe("transcript DOM window", () => {
  it("starts at the newest page", () => {
    expect(tailWindow(500)).toEqual({ start: 436, end: 500 });
    expect(tailWindow(20)).toEqual({ start: 0, end: 20 });
  });

  it("keeps a visible live tail rendering while staying bounded", () => {
    expect(advanceTailWindow({ start: 436, end: 500 }, 501)).toEqual({
      start: 436,
      end: 501,
    });
    expect(advanceTailWindow({ start: 436, end: 501 }, 700)).toEqual({
      start: 508,
      end: 700,
    });
  });

  it("pages backward without retaining unbounded newer DOM", () => {
    let current: TranscriptWindow = tailWindow(500);
    for (let i = 0; i < 8; i += 1) {
      current = pageEarlier(current, 500).settled;
      expect(current.end - current.start).toBeLessThanOrEqual(TRANSCRIPT_WINDOW);
    }
    expect(current).toEqual({ start: 0, end: 192 });
  });

  it("fills a short viewport without abandoning live-tail follow", () => {
    expect(autoPageEarlier(tailWindow(500), 500, true)).toEqual({
      expanded: { start: 372, end: 500 },
      settled: { start: 372, end: 500 },
      preserveTail: true,
    });
    expect(autoPageEarlier({ start: 308, end: 500 }, 500, true)).toBeNull();
  });

  it("treats a reader at the top as ordinary history paging", () => {
    expect(autoPageEarlier({ start: 308, end: 500 }, 500, false)).toEqual({
      expanded: { start: 244, end: 500 },
      settled: { start: 244, end: 436 },
      preserveTail: false,
    });
  });

  it("pages forward again and returns to the live tail", () => {
    let current: TranscriptWindow = { start: 0, end: 192 };
    for (let i = 0; i < 8; i += 1) {
      current = pageLater(current, 500).settled;
      expect(current.end - current.start).toBeLessThanOrEqual(TRANSCRIPT_WINDOW);
    }
    expect(current).toEqual({ start: 308, end: 500 });
  });

  it("repairs a saved range after the reducer compacts", () => {
    expect(restoreWindow({ start: 300, end: 492 }, 120)).toEqual({ start: 0, end: 120 });
  });
});

describe("virtual-coordinate cursors across cap trims", () => {
  it("restores a cursor saved before later trims to the same rows", () => {
    // Saved at array [1800,1992] when 100 blocks were already trimmed
    // (virtual [1900,2092]); 60 more were trimmed while the view was parked.
    expect(toArrayCoords({ start: 1900, end: 2092 }, 160, 2000)).toEqual({
      start: 1740,
      end: 1932,
    });
  });

  it("is the identity while nothing has been trimmed", () => {
    expect(toArrayCoords({ start: 436, end: 500 }, 0, 500)).toEqual({ start: 436, end: 500 });
  });

  it("clamps a window straddling the trim point to the array front", () => {
    expect(toArrayCoords({ start: 100, end: 292 }, 150, 2000)).toEqual({ start: 0, end: 142 });
  });

  it("rejects a window whose rows were all trimmed away", () => {
    expect(toArrayCoords({ start: 0, end: 192 }, 500, 2000)).toBeNull();
  });

  it("rejects a cursor stranded beyond the transcript by a reset/rewind", () => {
    expect(toArrayCoords({ start: 2400, end: 2592 }, 100, 2100)).toBeNull();
    // Partially beyond: what survives is kept, clamped to the tail.
    expect(toArrayCoords({ start: 2100, end: 2292 }, 100, 2100)).toEqual({
      start: 2000,
      end: 2100,
    });
  });

  it("advances the tail for at-cap appends when tracked virtually", () => {
    // At the reducer cap the ARRAY length stops growing, but the virtual
    // total (length + trimmedCount) still counts every append — so the same
    // window math keeps advancing instead of freezing the tail.
    const atCap = advanceTailWindow({ start: 1873, end: 2065 }, 2066);
    expect(atCap).toEqual({ start: 1874, end: 2066 });
  });
});
