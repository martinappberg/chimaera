import { describe, expect, it } from "vitest";
import { toolGroupTitle, type LabelledTool } from "./toolLabels";

function tool(kind: string, over: Partial<LabelledTool> = {}): LabelledTool {
  return { tool: kind, status: "completed", locations: [], summary: null, ...over };
}

describe("toolGroupTitle", () => {
  it("reads plain counts in the past tense, finished work only", () => {
    expect(toolGroupTitle([tool("execute")])).toBe("Ran a command");
    expect(toolGroupTitle([tool("execute"), tool("execute"), tool("read")])).toBe(
      "Ran 2 commands, read a file",
    );
    expect(toolGroupTitle([tool("think"), tool("agent")])).toBe("Ran an agent, used a tool");
  });

  it("counts edits by distinct file", () => {
    expect(
      toolGroupTitle([
        tool("edit", { locations: ["/a.rs"] }),
        tool("edit", { locations: ["/a.rs"] }),
        tool("edit", { locations: ["/b.rs"] }),
      ]),
    ).toBe("Edited 2 files");
  });

  it("says still-running calls in the present tense, after the finished ones", () => {
    expect(
      toolGroupTitle([tool("execute"), tool("execute", { status: "in_progress" })]),
    ).toBe("Ran a command, running a command");
  });

  it("names a lone agent", () => {
    expect(
      toolGroupTitle([tool("agent", { status: "in_progress", title: "Agent: Measure file sizes" })]),
    ).toBe("Running agent “Measure file sizes”");
    expect(toolGroupTitle([tool("agent", { title: "Agent: a" }), tool("agent")])).toBe(
      "Ran 2 agents",
    );
  });

  it("prefers the agent's own batch labels, deduped, in order", () => {
    expect(
      toolGroupTitle([
        tool("execute", { summary: "Listed files" }),
        tool("read", { summary: "Listed files" }),
        tool("agent", { summary: "Measured file sizes" }),
      ]),
    ).toBe("Listed files · Measured file sizes");
  });

  it("keeps counting the newest batch until its label lands", () => {
    expect(
      toolGroupTitle([
        tool("execute", { summary: "Listed files" }),
        tool("execute", { status: "in_progress" }),
      ]),
    ).toBe("Listed files · running a command");
  });
});
