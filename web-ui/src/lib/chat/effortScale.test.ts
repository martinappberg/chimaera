import { describe, expect, it } from "vitest";
import { effortScale } from "./effortScale";

describe("effortScale", () => {
  it("orders known levels faster to smarter, whatever order the agent lists", () => {
    expect(effortScale(["xhigh", "high", "medium", "low"])).toEqual(["low", "medium", "high", "xhigh"]);
    expect(effortScale(["low", "medium", "high", "xhigh", "max"])).toEqual(["low", "medium", "high", "xhigh", "max"]);
    expect(effortScale(["high", "minimal"])).toEqual(["minimal", "high"]);
  });

  it("keeps the agent's order when a level is outside the shared vocabulary", () => {
    expect(effortScale(["turbo", "low", "high"])).toEqual(["turbo", "low", "high"]);
    expect(effortScale([])).toEqual([]);
  });
});
