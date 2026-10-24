import { describe, expect, it } from "vitest";
import {
  actionReach,
  actionVerb,
  armedKey,
  clickVia,
  confirmHint,
  pressStep,
  sendNowReach,
  stopReach,
  stopReachText,
  type ActionContext,
  type StopAgentRow,
  type StopLane,
} from "./stopConfirm";

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
    // Esc confirms only a Stop; an armed Clear goal or Send now is cancelled.
    expect(armedKey(key("Escape"), true, false, false)).toBe("dismiss");
    expect(armedKey(key("Escape"), true, true, false)).toBe("dismiss");
  });

  it("leaves Enter or Space on a focused Stop to that Stop's own click", () => {
    expect(armedKey(key("Enter"), true, true)).toBe("ignore");
    expect(armedKey(key(" "), true, true)).toBe("ignore");
    expect(armedKey(key("a"), true, true)).toBe("dismiss");
  });

  it("swallows a held key's repeats, so one long press never answers itself", () => {
    const held = (k: string) => ({ key: k, defaultPrevented: false, repeat: true });
    expect(armedKey(held("Enter"), true, true)).toBe("hold");
    expect(armedKey(held(" "), true, true)).toBe("hold");
    expect(armedKey(held("Escape"), true)).toBe("hold");
    // Another pane's held Esc is that pane's, never swallowed here.
    expect(armedKey(held("Escape"), false)).toBe("dismiss");
    // The composer's own held Esc stays the composer's.
    expect(armedKey({ key: "Escape", defaultPrevented: true, repeat: true }, true)).toBe("ignore");
  });

  it("ignores a bare modifier", () => {
    for (const k of ["Shift", "Control", "Alt", "Meta"]) expect(armedKey(key(k), true)).toBe("ignore");
  });
});

describe("sendNowReach", () => {
  const sendText = (agents: StopAgentRow[], lanes: StopLane[]) => stopReachText(sendNowReach(agents, lanes));

  it("names the foreground subagent the turn waits on", () => {
    // claude: interrupt {send_now} stops it (Pass 44).
    expect(sendText([row("Agent: audit the loader")], [])).toBe("Also stops 1 subagent");
  });

  it("never names background work, which a claude Send now spares", () => {
    expect(sendText([], [lane("local_agent", "bg"), lane("local_workflow", "wf"), lane("local_bash", "npm test")])).toBeNull();
    // A foreground agent moved to the background is still its live row.
    expect(sendText([row("Agent: audit")], [lane("local_agent", "audit")])).toBeNull();
    expect(sendText([row("Agent: audit"), row("Task: other")], [lane("local_agent", "audit")])).toBe(
      "Also stops 1 subagent",
    );
  });

  it("never names codex's process-owned agents or settled rows", () => {
    expect(sendText([row("Agent: agent_a", { crossTurn: true })], [])).toBeNull();
    expect(sendText([row("Agent: done", { status: "completed" })], [])).toBeNull();
  });
});

describe("actionReach", () => {
  const ctx = (over: Partial<ActionContext> = {}): ActionContext => ({
    agents: [],
    lanes: [],
    busy: true,
    running: true,
    goalClearStops: true,
    queued: [],
    ...over,
  });
  const fg = [row("Agent: audit")];
  const bg = [lane("local_agent", "bg probe")];

  it("asks for a Stop only while something runs", () => {
    expect(actionReach("stop", ctx({ agents: fg }))).toBe("Also stops 1 subagent");
    expect(actionReach("stop", ctx({ lanes: bg }))).toBe("Also stops 1 subagent");
    expect(actionReach("stop", ctx({ lanes: bg, busy: false, running: false }))).toBeNull();
    expect(actionReach("stop", ctx())).toBeNull();
  });

  it("asks for claude's Clear goal as for a Stop, since it stops the turn first", () => {
    expect(actionReach("goal", ctx({ lanes: [lane("local_workflow", "wf")] }))).toBe("Also stops 1 workflow");
    expect(actionReach("goal", ctx({ agents: fg, lanes: bg }))).toBe("Also stops 2 subagents");
    // Idle, it is only the /goal clear message.
    expect(actionReach("goal", ctx({ lanes: bg, running: false }))).toBeNull();
    // codex's clear_goal lets the turn finish.
    expect(actionReach("goal", ctx({ agents: fg, goalClearStops: false }))).toBeNull();
  });

  it("asks for Send now only for what Send now ends", () => {
    const queued = ["m1"];
    expect(actionReach("send:m1", ctx({ agents: fg, queued }))).toBe("Also stops 1 subagent");
    // Background agents and workflows survive a Send now.
    expect(actionReach("send:m1", ctx({ lanes: [...bg, lane("local_workflow", "wf")], queued }))).toBeNull();
    // codex: turn/interrupt ends no spawned agent.
    expect(actionReach("send:m1", ctx({ agents: [row("Agent: a", { crossTurn: true })], queued }))).toBeNull();
    // Already read, or no turn to interrupt: nothing to ask.
    expect(actionReach("send:m1", ctx({ agents: fg, queued: ["m2"] }))).toBeNull();
    expect(actionReach("send:m1", ctx({ agents: fg, queued, running: false }))).toBeNull();
  });
});

describe("pressStep", () => {
  it("asks on the first press and acts on the second press of the same control", () => {
    expect(pressStep(null, "stop", "Also stops 1 subagent")).toBe("arm");
    expect(pressStep("stop", "stop", "Also stops 1 subagent")).toBe("act");
    expect(pressStep("send:m1", "send:m1", "Also stops 1 subagent")).toBe("act");
  });

  it("acts at once when nothing else would end", () => {
    expect(pressStep(null, "goal", null)).toBe("act");
    expect(pressStep(null, "send:m1", null)).toBe("act");
  });

  it("asks afresh when another control was waiting", () => {
    expect(pressStep("stop", "send:m1", "Also stops 1 subagent")).toBe("arm");
    expect(pressStep("send:m1", "send:m2", "Also stops 1 subagent")).toBe("arm");
  });
});

describe("the hint names the real gesture", () => {
  it("reads a keyboard-activated click as a key press", () => {
    expect(clickVia({ detail: 0 })).toBe("key");
    expect(clickVia({ detail: 1 })).toBe("pointer");
  });

  it("words Esc, a keyboard press and a click differently, and says the same aloud", () => {
    expect(confirmHint("stop", "esc")).toEqual({ how: "Esc again to stop", spoken: "Press Escape again to stop." });
    expect(confirmHint("stop", "key")).toEqual({ how: "Press again to stop", spoken: "Press again to stop." });
    expect(confirmHint("stop", "pointer")).toEqual({ how: "Click again to stop", spoken: "Click again to stop." });
  });

  it("ends with the action's own verb", () => {
    expect(actionVerb("stop")).toBe("stop");
    expect(actionVerb("goal")).toBe("clear");
    expect(actionVerb("send:m1")).toBe("send");
    expect(confirmHint(actionVerb("send:m1"), "pointer").how).toBe("Click again to send");
  });
});
