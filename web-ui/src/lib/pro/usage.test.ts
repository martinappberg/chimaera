import { describe, expect, it } from "vitest";
import { usagePresentation } from "./usage";

describe("account usage percentages", () => {
  it("uses the supplied allowance for hours and bytes without assuming a plan", () => {
    expect(usagePresentation(12.5, 50)).toMatchObject({label: "25% used", progress: 25, limited: false});
    expect(usagePresentation(3e9, 12e9)).toMatchObject({label: "25% used", progress: 25});
    expect(usagePresentation(0, 200)).toMatchObject({label: "0% used", progress: 0});
  });
  it("keeps small and near-limit positive usage honest after rounding", () => {
    expect(usagePresentation(0.001, 100).label).toBe("<0.1% used");
    expect(usagePresentation(99.99, 100)).toMatchObject({label: ">99.9% used", limited: false});
    expect(usagePresentation(100.01, 100)).toMatchObject({label: ">100% used", limited: true});
  });
  it("bounds the visual bar but retains exceeded usage in its text", () => {
    expect(usagePresentation(100, 100)).toMatchObject({label: "100% used", progress: 100, detail: "Limit reached"});
    expect(usagePresentation(125, 100)).toMatchObject({label: "125% used", progress: 100, detail: "Over limit"});
    expect(usagePresentation(1e100, 1)).toMatchObject({label: ">999% used", progress: 100, limited: true});
    expect(usagePresentation(Number.MAX_VALUE, Number.MIN_VALUE)).toMatchObject({label: "Over limit", progress: 100, limited: true});
  });
  it("distinguishes a zero allowance from a zero-use allowance", () => {
    expect(usagePresentation(0, 0)).toMatchObject({label: "No allowance", progress: null});
    expect(usagePresentation(1, 0)).toMatchObject({label: "Over limit", progress: 100, detail: "No current allowance"});
  });
  it("never turns absent, invalid or negative account fields into a percentage", () => {
    for (const value of [undefined, null, NaN, Infinity, -1]) {
      expect(usagePresentation(value, 100)).toMatchObject({label: "Usage unavailable", progress: null});
      expect(usagePresentation(0, value)).toMatchObject({label: "Usage unavailable", progress: null});
    }
  });
});
