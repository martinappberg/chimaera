import { describe, expect, it } from "vitest";

import { awaitsDecision, dotState, dotTitle, isBusy, needsApproval, needsAttention, type AgentState, type Session } from "./sessions";

function chat(state: AgentState | null, over: Partial<Session> = {}): Session {
  return {
    id: "s-chat",
    name: "claude",
    cwd: "/w",
    cols: 80,
    rows: 24,
    created_at: 0,
    alive: true,
    exit_status: null,
    title: null,
    workspace_id: "ws",
    kind: "agent",
    agent_kind: "claude",
    agent_state: state,
    agent_title: null,
    ui: "chat",
    ...over,
  };
}

describe("a conversation waiting on the user", () => {
  it("reads the driver's additive needs_permission exactly like the state", () => {
    const byState = chat("needs_permission");
    const byDriver = chat("running", { needs_permission: true });
    for (const s of [byState, byDriver]) {
      expect(awaitsDecision(s)).toBe(true);
      expect(needsApproval(s)).toBe(true);
      expect(needsAttention(s)).toBe(true);
    }
    expect(dotState(byDriver)).toBe(dotState(byState));
    expect(dotTitle(byDriver)).toBe(dotTitle(byState));
  });

  it("changes nothing for rows without the field or with it false", () => {
    for (const state of ["running", "finished", "idle_prompt", "errored", null] as const) {
      const absent = chat(state);
      const off = chat(state, { needs_permission: false });
      expect(needsApproval(off)).toBe(needsApproval(absent));
      expect(needsAttention(off)).toBe(needsAttention(absent));
      expect(dotState(off)).toBe(dotState(absent));
      expect(dotTitle(off)).toBe(dotTitle(absent));
      expect(isBusy(off)).toBe(isBusy(absent));
    }
    expect(needsApproval(chat("running"))).toBe(false);
  });

  it("never marks a shell", () => {
    const shell = chat(null, { kind: "shell", agent_kind: null, ui: "term", needs_permission: true, phase: "ready" });
    expect(dotState(shell)).toBe("idle");
  });
});
