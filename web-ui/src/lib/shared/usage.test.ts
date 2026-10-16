import { describe, expect, it } from "vitest";

import { resetsAtMs, resetsLabel, usageHeadline, usageLevel, usageRows, usageTitle } from "./usage";

const NOW = Date.UTC(2026, 9, 15, 12, 0, 0);
const seconds = (ms: number) => String(Math.floor(ms / 1000));

describe("plan usage reading", () => {
  it("reads both reset spellings", () => {
    expect(resetsAtMs("1792110000")).toBe(1792110000 * 1000);
    expect(resetsAtMs("2026-10-16T05:19:59.549408+00:00")).toBe(Date.parse("2026-10-16T05:19:59.549408+00:00"));
    expect(resetsAtMs("soon")).toBeNull();
    expect(resetsAtMs(null)).toBeNull();
  });

  it("says when a window resets in the fewest words", () => {
    expect(resetsLabel(seconds(NOW + 40 * 60_000), NOW)).toBe("in 40m");
    expect(resetsLabel(seconds(NOW + (3 * 60 + 12) * 60_000), NOW)).toBe("in 3h 12m");
    expect(resetsLabel(seconds(NOW + 5 * 3_600_000), NOW)).toBe("in 5h");
    expect(resetsLabel(seconds(NOW - 1000), NOW)).toBe("now");
    // Within the week a weekday and a time; further out a date.
    expect(resetsLabel(seconds(NOW + 3 * 86_400_000), NOW, "en-US")).toMatch(/^[A-Z][a-z]{2} \d{2}:\d{2}/);
    expect(resetsLabel(seconds(NOW + 20 * 86_400_000), NOW, "en-US")).toBe("Nov 4");
  });

  it("stays quiet until it matters", () => {
    expect(usageLevel(79)).toBe("quiet");
    expect(usageLevel(80)).toBe("warn");
    expect(usageLevel(100)).toBe("full");
    expect(usageLevel(10, true)).toBe("full");
    const limits = usageRows([
      { label: "session limit", utilization: 20 },
      { label: "weekly limit", utilization: 56, resets_at: "1792551600" },
    ]);
    // Everything quiet: the header shows only its glyph.
    expect(usageHeadline(64, limits, false)).toBeNull();
    // The most pressing reading speaks, by its short name.
    expect(usageHeadline(84, limits, false)).toEqual({ label: "context", pct: 84, level: "warn" });
    const tight = usageRows([{ label: "weekly limit", utilization: 92 }]);
    expect(usageHeadline(30, tight, false)).toEqual({ label: "weekly", pct: 92, level: "warn" });
    // A refused limit wins whatever the numbers say.
    expect(usageHeadline(90, limits, true)).toEqual({ label: "weekly", pct: 56, level: "full" });
    expect(usageHeadline(null, [], true)).toEqual({ label: "usage", pct: 100, level: "full" });
  });

  it("takes the wire's windows defensively", () => {
    const rows = usageRows([
      { label: "weekly limit", utilization: 140 },
      { label: "weekly limit", utilization: 3 },
      { label: 7, utilization: 3 },
      { label: "session limit", utilization: "x" },
    ]);
    expect(rows).toEqual([{ label: "weekly limit", utilization: 100, resetsAt: null }]);
    expect(usageTitle("weekly Opus limit")).toBe("Weekly Opus limit");
  });
});
