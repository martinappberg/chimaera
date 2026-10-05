import { describe, expect, it } from "vitest";
import { finishedTitle, foldSpans, foldTitle } from "./activityFold";
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
      foldTitle(3, [tool("execute"), tool("execute"), tool("read", { summary: "Read notes" })]),
    ).toBe("Thought, ran 2 commands, read a file");
    expect(foldTitle(1, [tool("agent", { title: "Agent: Measure sizes" })])).toBe(
      "Thought, ran agent “Measure sizes”",
    );
  });

  it("counts thoughts only when they are all there is", () => {
    expect(foldTitle(2, [])).toBe("Thought 2 times");
    expect(foldTitle(1, [])).toBe("Thought");
  });
});

describe("finished-work folds", () => {
  const spans = (kinds: string) =>
    foldSpans(
      kinds.split(""),
      (k) => k === "f",
      () => true,
      3,
    );

  it("folds three or more settled finished lines, never fewer or the trailing run", () => {
    expect(spans("mfffm")).toEqual([[1, 4]]);
    expect(spans("mffm")).toEqual([]);
    expect(spans("mfff")).toEqual([]);
    expect(spans("ffffufff")).toEqual([[0, 4]]);
  });

  it("says what ended and how, naming each kind once", () => {
    const row = (source: string, status: string) => ({ source, status });
    expect(
      finishedTitle([
        row("task", "completed"),
        row("task", "completed"),
        row("task", "stopped"),
        row("task", "stopped"),
        row("task", "stopped"),
        row("agent", "completed"),
      ]),
    ).toBe("1 agent finished, 2 background tasks finished, 3 stopped");
    expect(finishedTitle([row("task", "failed"), row("monitor", "completed"), row("monitor", "completed")])).toBe(
      "1 background task failed, 2 monitors finished",
    );
    expect(finishedTitle([row("task", "killed"), row("task", "completed"), row("task", "killed")])).toBe(
      "1 background task finished, 2 killed",
    );
  });
});
