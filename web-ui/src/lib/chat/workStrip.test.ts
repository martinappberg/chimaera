import { describe, expect, it } from "vitest";
import { foldWork, workParts } from "./workStrip";

describe("pinned work folding", () => {
  it("folds tasks, background work and a schedule into one strip", () => {
    const parts = workParts({ goal: false, plan: true, agents: false, background: true, schedule: true });
    expect(parts).toEqual(["plan", "background", "schedule"]);
    expect(foldWork(parts, false)).toBe(true);
  });

  it("keeps a folded strip stable as work finishes, then releases the lone card", () => {
    expect(foldWork(["plan", "schedule"], true)).toBe(true);
    expect(foldWork(["plan", "schedule"], false)).toBe(false);
    expect(foldWork(["plan"], true)).toBe(false);
    expect(foldWork([], true)).toBe(false);
  });
});
