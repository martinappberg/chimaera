import { describe, expect, it } from "vitest";
import { armedKey, stopReach, stopReachText, type StopAgentRow, type StopLane } from "./stopConfirm";

function row(title: string, over: Partial<StopAgentRow> = {}): StopAgentRow {
  return { title, status: "in_progress", crossTurn: false, ...over };
}
function lane(taskType: string, description: string, over: Partial<StopLane> = {}): StopLane {
  return { taskType, description, ambient: false, ...over };
}
const text = (agents: StopAgentRow[], lanes: StopLane[]) => stopReachText(stopReach(agents, lanes));

describe("stopReach", () => {
  it("stops at once when only the turn would end", () => {
    expect(text([], [])).toBeNull();
  });

  it("names a foreground subagent the turn waits on", () => {
    expect(text([row("Agent: audit the loader")], [])).toBe("Also stops 1 subagent");
  });

  it("names background agents and workflows, which a claude Stop kills", () => {
    expect(
      text([], [lane("local_agent", "bg probe"), lane("local_agent", "second"), lane("local_workflow", "probe")]),
    ).toBe("Also stops 2 subagents and 1 workflow");
    expect(text([], [lane("local_workflow", "a"), lane("local_workflow", "b")])).toBe("Also stops 2 workflows");
  });

  it("never names work a Stop leaves running", () => {
    // claude: run_in_background commands and Monitor watches ride local_bash
    // and survive the interrupt; remote agents are unprobed.
    expect(text([], [lane("local_bash", "npm test"), lane("local_bash", "watch log"), lane("remote_agent", "r")])).toBeNull();
    // codex: collab agents are process-owned and outlive turn/interrupt.
    expect(text([row("Agent: agent_a", { crossTurn: true })], [])).toBeNull();
  });

  it("skips settled rows and housekeeping lanes", () => {
    expect(text([row("Agent: done", { status: "completed" }), row("Agent: x", { status: "failed" })], [])).toBeNull();
    expect(text([], [lane("local_agent", "tidy", { ambient: true })])).toBeNull();
    expect(text([row("Agent: queued", { status: "pending" })], [])).toBe("Also stops 1 subagent");
  });

  it("counts a foreground agent moved to the background once", () => {
    expect(text([row("Agent: audit")], [lane("local_agent", "audit")])).toBe("Also stops 1 subagent");
    expect(text([row("Task: audit")], [lane("local_agent", "audit"), lane("local_agent", "other")])).toBe(
      "Also stops 2 subagents",
    );
  });
});

describe("armedKey", () => {
  const key = (k: string, defaultPrevented = false) => ({ key: k, defaultPrevented });

  it("confirms on a second Esc in this chat", () => {
    expect(armedKey(key("Escape"), true)).toBe("confirm");
  });

  it("leaves an Esc another surface handled alone", () => {
    // The composer's own Esc (it confirms itself), a card's, a popover's.
    expect(armedKey(key("Escape", true), true)).toBe("ignore");
  });

  it("lets the stop go on any other key or an Esc in another pane", () => {
    expect(armedKey(key("a"), true)).toBe("dismiss");
    expect(armedKey(key("Enter"), true)).toBe("dismiss");
    expect(armedKey(key("Escape"), false)).toBe("dismiss");
  });

  it("leaves Enter or Space on a focused Stop to that Stop's own click", () => {
    expect(armedKey(key("Enter"), true, true)).toBe("ignore");
    expect(armedKey(key(" "), true, true)).toBe("ignore");
    expect(armedKey(key("a"), true, true)).toBe("dismiss");
  });

  it("ignores a bare modifier", () => {
    for (const k of ["Shift", "Control", "Alt", "Meta"]) expect(armedKey(key(k), true)).toBe("ignore");
  });
});
