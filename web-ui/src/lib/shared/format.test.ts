import { describe, expect, it } from "vitest";
import { formatTokenCount } from "./format";

describe("formatTokenCount", () => {
  it("mirrors the drivers' fmt_tokens spelling", () => {
    expect(formatTokenCount(512)).toBe("512");
    expect(formatTokenCount(12_345)).toBe("12.3k");
    expect(formatTokenCount(999_949)).toBe("999.9k");
    expect(formatTokenCount(999_950)).toBe("1.0M");
    expect(formatTokenCount(2_300_000)).toBe("2.3M");
  });
  it("drops the decimal past ten units in the compact spelling", () => {
    expect(formatTokenCount(1234, true)).toBe("1.2k");
    expect(formatTokenCount(45_000, true)).toBe("45k");
    expect(formatTokenCount(1_500_000, true)).toBe("1.5M");
    expect(formatTokenCount(12_000_000, true)).toBe("12M");
  });
});
