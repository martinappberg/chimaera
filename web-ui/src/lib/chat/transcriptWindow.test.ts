import { describe, expect, it } from "vitest";
import {
  advanceTailWindow,
  pageEarlier,
  pageLater,
  restoreWindow,
  tailWindow,
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
