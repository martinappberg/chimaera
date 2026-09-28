import { describe, expect, it } from "vitest";
import { toolGroupTitle, toolRunHealth, type HealthTool, type LabelledTool } from "./toolLabels";

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

describe("toolRunHealth", () => {
  function call(over: Partial<HealthTool> = {}): HealthTool {
    return { tool: "edit", title: "Edit a.rs", status: "completed", locations: [], denied: false, ...over };
  }

  it("is clean when nothing failed", () => {
    expect(toolRunHealth([call(), call({ status: "in_progress" })])).toBeNull();
  });

  it("recovers a failure a later same-target call completed", () => {
    expect(
      toolRunHealth([
        call({ status: "failed", locations: ["/a.rs"] }),
        call({ locations: ["/a.rs"] }),
      ]),
    ).toBe("recovered");
    expect(toolRunHealth([call({ status: "failed" }), call()])).toBe("recovered");
  });

  it("stays failed when the retry hit another target or came first", () => {
    expect(
      toolRunHealth([
        call({ status: "failed", locations: ["/a.rs"] }),
        call({ locations: ["/b.rs"] }),
      ]),
    ).toBe("failed");
    expect(toolRunHealth([call(), call({ status: "failed" })])).toBe("failed");
  });

  it("never recovers a denial", () => {
    expect(toolRunHealth([call({ denied: true, status: "failed" }), call()])).toBe("failed");
  });

  it("recovers a failed command with any later completed command", () => {
    const cmd = (command: string, over: Partial<HealthTool> = {}) =>
      call({ tool: "execute", title: command, command, ...over });
    const failed = cmd("cargo build", { status: "failed" });
    expect(toolRunHealth([failed, cmd("cargo build 2>&1 | tail")])).toBe("recovered");
    expect(toolRunHealth([failed, cmd("cargo build", { status: "in_progress" })])).toBe("failed");
    expect(toolRunHealth([failed, call({ title: "cargo build" })])).toBe("failed");
  });

  it("doesn't let a call that ran no command recover one", () => {
    const failed = call({ tool: "execute", title: "cargo test", command: "cargo test", status: "failed" });
    const kill = call({ tool: "execute", title: "KillShell: bg-1", command: null });
    expect(toolRunHealth([failed, kill])).toBe("failed");
    // A pre-field journal row still recovers the old way: the same title.
    const ls = (over: Partial<HealthTool> = {}) => call({ tool: "execute", title: "ls", ...over });
    expect(toolRunHealth([ls({ status: "failed" }), ls()])).toBe("recovered");
  });

  it("looks past the run into the rest of its turn", () => {
    const failedEdit = call({ status: "failed", locations: ["/a.rs"] });
    const retry = call({ locations: ["/a.rs"] });
    const other = call({ locations: ["/b.rs"] });
    expect(toolRunHealth([failedEdit], { tools: [failedEdit, other, retry], from: 1 })).toBe("recovered");
    expect(toolRunHealth([failedEdit], { tools: [failedEdit, other], from: 1 })).toBe("failed");
    // Calls before `from` belong to the run (or precede it) and don't count.
    expect(toolRunHealth([failedEdit], { tools: [retry, failedEdit], from: 2 })).toBe("failed");
  });
});
