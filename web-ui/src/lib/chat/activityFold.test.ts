import { describe, expect, it } from "vitest";
import { foldSpans, foldTitle } from "./activityFold";
import type { LabelledTool } from "./toolLabels";

// a = activity, m = a reply (closes a run), u / n = other rows (user, notice).
const spans = (row: string) =>
  foldSpans(
    row.split(""),
    (c) => c === "a",
    (c) => c === "m",
  );

function tool(kind: string, over: Partial<LabelledTool> = {}): LabelledTool {
  return { tool: kind, status: "completed", locations: [], summary: null, ...over };
}

describe("foldSpans", () => {
  it("folds a run of two or more that a reply follows", () => {
    expect(spans("uaaam")).toEqual([[1, 4]]);
    expect(spans("aamaaam")).toEqual([
      [0, 2],
      [3, 6],
    ]);
  });

  it("leaves a lone line, a trailing run, and a run closed by anything else", () => {
    expect(spans("uam")).toEqual([]);
    expect(spans("amaaa")).toEqual([]);
    expect(spans("aanm")).toEqual([]);
    expect(spans("aau")).toEqual([]);
  });
});

describe("foldTitle", () => {
  it("leads with thinking, then the counted calls", () => {
    expect(
      foldTitle(3, [tool("execute"), tool("execute"), tool("read", { summary: "Read notes" })], 0),
    ).toBe("Thought, ran 2 commands, read a file");
    expect(foldTitle(0, [tool("agent", { title: "Agent: Measure sizes" })], 1)).toBe(
      "Ran agent “Measure sizes”",
    );
  });

  it("counts thoughts only when they are all there is", () => {
    expect(foldTitle(2, [], 0)).toBe("Thought 2 times");
    expect(foldTitle(1, [], 1)).toBe("Thought");
  });

  it("falls back to the finished work", () => {
    expect(foldTitle(0, [], 1)).toBe("Work finished");
    expect(foldTitle(0, [], 2)).toBe("2 tasks finished");
  });
});
