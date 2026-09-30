import { describe, expect, it } from "vitest";
import {
  commsCall,
  commsTitle,
  readableToolTitle,
  toolGroupTitle,
  toolRunHealth,
  type HealthTool,
  type LabelledTool,
} from "./toolLabels";

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

describe("agent-communication tools", () => {
  it("recognizes every driver's MCP naming", () => {
    // claude titles `tool (server)`; codex `server.tool` (an item) or
    // `server · tool` (an approval); the raw CLI name too.
    for (const title of [
      "message_agent (chimaera)",
      "chimaera.message_agent",
      "chimaera · message_agent",
      "mcp__chimaera__message_agent",
    ]) {
      expect(commsCall(title)).toEqual({ verb: "message_agent", target: null });
    }
    expect(commsCall("read_messages (chimaera)")).toEqual({ verb: "read_messages", target: null });
    expect(commsCall("chimaera.workspace_agents")).toEqual({ verb: "workspace_agents", target: null });
  });

  it("leaves other servers and other chimaera tools alone", () => {
    expect(commsCall("search_memory (goldfish)")).toBeNull();
    expect(commsCall("goldfish.message_agent")).toBeNull();
    expect(commsCall("workspace_status (chimaera)")).toBeNull();
    expect(commsCall("SendMessage → teammate: hi")).toBeNull();
  });

  it("takes the target from the input, else from the title", () => {
    expect(commsCall("mcp__chimaera__message_agent", { to: "loader refactor", text: "x" })).toEqual({
      verb: "message_agent",
      target: "loader refactor",
    });
    expect(commsCall("mcp__chimaera__read_agent", { agent: "s-1a2b" })?.target).toBe("s-1a2b");
    expect(commsCall("message_agent (chimaera) → fix CI: the loader changed")?.target).toBe("fix CI");
    expect(commsCall("chimaera.read_agent: s-9f")?.target).toBe("s-9f");
  });

  it("titles each call in words", () => {
    expect(commsTitle({ verb: "message_agent", target: "loader refactor" })).toBe("Message to loader refactor");
    expect(commsTitle({ verb: "message_agent", target: "mastermind" })).toBe("Message to the Mastermind");
    expect(commsTitle({ verb: "message_agent", target: "everyone" })).toBe("Message to everyone");
    expect(commsTitle({ verb: "message_agent", target: null })).toBe("Sent a message");
    expect(commsTitle({ verb: "read_messages", target: null })).toBe("Checked messages");
    expect(commsTitle({ verb: "workspace_agents", target: null })).toBe("Listed agents");
    expect(commsTitle({ verb: "read_agent", target: "fix CI" })).toBe("Read fix CI's work");
    expect(readableToolTitle({ tool: "other", title: "chimaera.workspace_agents" })).toBe("Listed agents");
    // Only MCP-kind rows are renamed; everything else keeps the driver's title.
    expect(readableToolTitle({ tool: "execute", title: "chimaera.workspace_agents" })).toBe("chimaera.workspace_agents");
    expect(readableToolTitle({ tool: "other", title: "search_memory (goldfish)" })).toBe(
      "search_memory (goldfish)",
    );
  });

  it("says what a group did instead of 'used a tool'", () => {
    expect(toolGroupTitle([tool("other", { title: "workspace_agents (chimaera)" })])).toBe("Listed agents");
    expect(
      toolGroupTitle([
        tool("other", { title: "read_agent (chimaera) → fix CI" }),
        tool("other", { title: "message_agent (chimaera) → fix CI" }),
        tool("other", { title: "search_memory (goldfish)" }),
      ]),
    ).toBe("Messaged fix CI, read fix CI's work, used a tool");
    expect(
      toolGroupTitle([
        tool("other", { title: "chimaera.message_agent" }),
        tool("other", { title: "chimaera.message_agent" }),
        tool("other", { title: "chimaera.read_messages", status: "in_progress" }),
      ]),
    ).toBe("Sent 2 messages, checking messages");
  });
});
