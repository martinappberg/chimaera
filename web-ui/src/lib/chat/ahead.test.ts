import { describe, expect, it } from "vitest";

import { compactCount, goalLive, jobPeriodMs, parseGoal, parseSchedule, ringFraction, untilLabel } from "./ahead";

describe("ahead", () => {
  it("says when, calmly: minutes, seconds only at the end, a clock time past the hour", () => {
    const now = new Date(2026, 9, 15, 17, 0, 0).getTime();
    expect(untilLabel(null, now)).toBe("");
    expect(untilLabel(now - 1, now)).toBe("now");
    expect(untilLabel(now + 38_000, now)).toBe("in 38s");
    expect(untilLabel(now + 75_000, now)).toBe("in 1m");
    expect(untilLabel(now + 4 * 60_000 + 12_000, now)).toBe("in 4m");
    expect(untilLabel(now + 90 * 60_000, now)).toMatch(/^at /);
    expect(untilLabel(new Date(2026, 9, 16, 3, 47).getTime(), now)).toMatch(/^tomorrow /);
  });

  it("drains the ring over the wait", () => {
    expect(ringFraction(200, 100, 150)).toBe(0.5);
    expect(ringFraction(200, 100, 250)).toBe(0);
    expect(ringFraction(null, 0, 0)).toBe(0);
  });

  it("knows a plain step's period", () => {
    expect(jobPeriodMs("*/10 * * * *")).toBe(600_000);
    expect(jobPeriodMs("* * * * *")).toBe(60_000);
    expect(jobPeriodMs("0 */2 * * *")).toBe(7_200_000);
    expect(jobPeriodMs("17 3 * * *")).toBe(86_400_000);
    expect(jobPeriodMs("0 9 * * MON")).toBe(600_000);
  });

  it("parses the wire defensively", () => {
    expect(parseSchedule({ wakeup: { reason: "x" }, jobs: [{ id: "" }, { id: "a" }, { id: "a" }] })).toEqual({
      wakeup: null,
      jobs: [expect.objectContaining({ id: "a", nextAtMs: null })],
    });
    expect(parseGoal({ objective: "" })).toBeNull();
    const done = parseGoal({ objective: "x", status: "complete" });
    expect(goalLive(done)).toBe(false);
    expect(goalLive(parseGoal({ objective: "x" }))).toBe(true);
  });

  it("counts tokens at a glance", () => {
    expect([compactCount(950), compactCount(1200), compactCount(41_000), compactCount(1_250_000)]).toEqual([
      "950",
      "1.2k",
      "41k",
      "1.3M",
    ]);
  });
});
