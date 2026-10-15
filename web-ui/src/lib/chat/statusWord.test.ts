import { describe, expect, it } from "vitest";

import type { SeqEvent } from "./chatWs";
import { statusWordShown, type StatusWordInput } from "./statusWord";
import { ChatStore } from "./store.svelte";

function fold(events: Record<string, unknown>[]): ChatStore {
  const store = new ChatStore();
  events.forEach((ev, i) => store.apply({ seq: i + 1, ts: 1000 + i, ev } as SeqEvent));
  return store;
}

function inputFrom(store: ChatStore, over: Partial<StatusWordInput> = {}): StatusWordInput {
  return {
    compacting: store.compacting,
    activityLine: store.activityLine,
    activity: store.activity,
    running: store.running,
    newest: store.blocks[store.blocks.length - 1],
    tailInView: true,
    liveToolLineInView: false,
    ...over,
  };
}

describe("the status row's phase word", () => {
  it("says Thinking while a reasoning item draws no thought row (Codex, empty summary)", () => {
    const store = fold([
      { type: "turn_started", turn_id: "t" },
      { type: "user_message", text: "go", id: "u1" },
      { type: "thinking_tokens", tokens: 0 },
    ]);
    // The phase as the drift pass maps a zero estimate: thinking, no detail.
    const input = inputFrom(store, { activity: { kind: "thinking", detail: "" } });
    expect(input.newest?.kind).not.toBe("thought");
    expect(statusWordShown(input)).toBe(true);
  });

  it("leaves it to a streaming thought row at the tail, in view", () => {
    const store = fold([
      { type: "turn_started", turn_id: "t" },
      { type: "thought_chunk", turn_id: "t", text: "weighing options" },
    ]);
    expect(store.activity).toEqual({ kind: "thinking", detail: "" });
    expect(statusWordShown(inputFrom(store))).toBe(false);
    // Scrolled away, or the turn over (the row settled): the word is said.
    expect(statusWordShown(inputFrom(store, { tailInView: false }))).toBe(true);
    expect(statusWordShown(inputFrom(store, { running: false }))).toBe(true);
    // A settled thought followed by other work is not the live row.
    const later = fold([
      { type: "turn_started", turn_id: "t" },
      { type: "thought_chunk", turn_id: "t", text: "weighing options" },
      { type: "tool_call", id: "x", kind: "read", title: "Read: a", status: "completed" },
    ]);
    expect(statusWordShown(inputFrom(later, { activity: { kind: "thinking", detail: "" } }))).toBe(true);
  });

  it("keeps waiting, token estimates and off-screen tool work", () => {
    const store = fold([{ type: "turn_started", turn_id: "t" }]);
    expect(statusWordShown(inputFrom(store, { activity: { kind: "waiting", detail: "" } }))).toBe(true);
    expect(statusWordShown(inputFrom(store, { activity: { kind: "thinking", detail: "~1.2k tokens" } }))).toBe(true);
    const tool = { kind: "tool" as const, detail: "ls" };
    expect(statusWordShown(inputFrom(store, { activity: tool, liveToolLineInView: true }))).toBe(false);
    expect(statusWordShown(inputFrom(store, { activity: tool, liveToolLineInView: false }))).toBe(true);
  });

  it("leaves Writing to streaming prose at the tail, in view", () => {
    const store = fold([
      { type: "turn_started", turn_id: "t" },
      { type: "message_chunk", turn_id: "t", text: "Here is" },
    ]);
    expect(statusWordShown(inputFrom(store))).toBe(false);
    expect(statusWordShown(inputFrom(store, { tailInView: false }))).toBe(true);
  });
});
