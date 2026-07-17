import { beforeEach, describe, expect, it } from "vitest";

import type { AgentState, Session } from "./sessions";
import { foldUnread, isUnread, markSeen } from "./unread.svelte";

/** A minimal agent session row — only the fields foldUnread reads matter. */
function agent(id: string, over: Partial<Session> = {}): Session {
  return {
    id,
    name: id,
    cwd: "/w",
    cols: 80,
    rows: 24,
    created_at: 0,
    alive: true,
    exit_status: null,
    title: null,
    workspace_id: "ws",
    kind: "agent",
    agent_state: "running" as AgentState,
    agent_title: null,
    ...over,
  };
}

/** Fold prev→next and return the resulting unread set for the given ids. */
function fold(prev: Session[], next: Session[], focusedId: string | null = null): void {
  foldUnread(new Map(prev.map((s) => [s.id, s])), next, focusedId);
}

describe("foldUnread", () => {
  beforeEach(() => {
    // Reset the module's set between cases (no live ids → prunes to empty).
    foldUnread(new Map(), [], null);
  });

  it("marks a session whose main turn finished while unfocused", () => {
    fold([agent("a", { agent_state: "running" })], [agent("a", { agent_state: "finished" })]);
    expect(isUnread("a")).toBe(true);
  });

  it("marks a running→idle_prompt transition (handed the floor back)", () => {
    fold([agent("a", { agent_state: "running" })], [agent("a", { agent_state: "idle_prompt" })]);
    expect(isUnread("a")).toBe(true);
  });

  it("does NOT mark a mid-turn output lull (output_active true→false while running)", () => {
    fold(
      [agent("a", { agent_state: "running", output_active: true })],
      [agent("a", { agent_state: "running", output_active: false })],
    );
    expect(isUnread("a")).toBe(false);
  });

  it("does NOT mark a finished turn while subagents are still on the wire", () => {
    fold(
      [agent("a", { agent_state: "running", subagents: [{ id: "s1", label: "Explore", started_at: 0 }] })],
      [
        agent("a", {
          agent_state: "finished",
          subagents: [{ id: "s1", label: "Explore", started_at: 0 }],
        }),
      ],
    );
    expect(isUnread("a")).toBe(false);
  });

  it("marks once the finished turn has no subagents left", () => {
    fold(
      [agent("a", { agent_state: "running", subagents: [{ id: "s1", label: "Explore", started_at: 0 }] })],
      [agent("a", { agent_state: "finished", subagents: [] })],
    );
    expect(isUnread("a")).toBe(true);
  });

  it("never marks the focused session (finished under the user's eyes)", () => {
    fold([agent("a", { agent_state: "running" })], [agent("a", { agent_state: "finished" })], "a");
    expect(isUnread("a")).toBe(false);
  });

  it("never marks the workspace Mastermind", () => {
    fold(
      [agent("a", { agent_state: "running", mastermind: true })],
      [agent("a", { agent_state: "finished", mastermind: true })],
    );
    expect(isUnread("a")).toBe(false);
  });

  it("clears on markSeen and prunes ids gone from the snapshot", () => {
    fold([agent("a", { agent_state: "running" })], [agent("a", { agent_state: "finished" })]);
    expect(isUnread("a")).toBe(true);
    markSeen("a");
    expect(isUnread("a")).toBe(false);

    // Re-mark, then fold a snapshot without it → pruned.
    fold([agent("a", { agent_state: "running" })], [agent("a", { agent_state: "finished" })]);
    expect(isUnread("a")).toBe(true);
    fold([agent("a", { agent_state: "finished" })], [agent("b", { agent_state: "running" })]);
    expect(isUnread("a")).toBe(false);
  });
});
