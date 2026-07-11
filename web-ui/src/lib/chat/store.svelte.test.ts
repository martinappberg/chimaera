import { describe, expect, it } from "vitest";

import type { SeqEvent } from "./chatWs";
import { ChatStore } from "./store.svelte";

/** Build a numbered event stream (seq assigned in order) and fold it through a
 *  fresh store — the reducer's only input, exactly as the wire delivers it. */
function fold(events: Record<string, unknown>[]): ChatStore {
  const store = new ChatStore();
  events.forEach((ev, i) => store.apply({ seq: i + 1, ts: i, ev } as SeqEvent));
  return store;
}

/** The scenario at the heart of the ordering bug: a message queued WHILE the
 *  agent is streaming a single response. The two prose deltas straddle the
 *  queued send and its checkpoint. */
const QUEUED_MID_TURN: Record<string, unknown>[] = [
  { type: "turn_started", turn_id: "t1" },
  { type: "message_chunk", turn_id: "t1", text: "hel" },
  // Queued mid-stream — must NOT land between the two prose deltas.
  { type: "user_message", text: "meanwhile do X", id: "q1", queued: true },
  { type: "checkpoint", user_message_id: "q1", preceding_uuid: "p0" },
  { type: "message_chunk", turn_id: "t1", text: "lo" },
  { type: "turn_completed", turn_id: "t1", usage: { output_tokens: 2 } },
  // The turn drained; the queued message resolves sent.
  { type: "user_message_update", id: "q1", state: "sent" },
];

describe("ChatStore pending-send ordering", () => {
  it("keeps a queued send out of the transcript until it is sent", () => {
    // Fold only up to just before the turn ends (the mid-stream window).
    const store = fold(QUEUED_MID_TURN.slice(0, 5));
    // The agent's message is a SINGLE unbroken block — not split by the queued
    // send (the old bug rendered [msg][user][msg]).
    const msgs = store.blocks.filter((b) => b.kind === "message");
    expect(msgs).toHaveLength(1);
    expect(msgs[0]).toMatchObject({ kind: "message", text: "hello" });
    // No user block is in the transcript yet — the queued send is in its own
    // stack, carrying the checkpoint anchor it was stamped with.
    expect(store.blocks.some((b) => b.kind === "user")).toBe(false);
    expect(store.pendingSends).toHaveLength(1);
    expect(store.pendingSends[0]).toMatchObject({
      id: "q1",
      text: "meanwhile do X",
      state: "queued",
      checkpoint: { id: "q1", preceding: "p0" },
    });
  });

  it("appends a delivered send AFTER the full agent message, never splicing it", () => {
    const store = fold(QUEUED_MID_TURN);
    // Pending stack is now empty; the send moved into history.
    expect(store.pendingSends).toHaveLength(0);
    const kinds = store.blocks.map((b) => b.kind);
    // The single message, then the turn end, then the delivered user message —
    // the user bubble sits AFTER the whole agent response, not inside it.
    expect(kinds).toEqual(["message", "turn_end", "user"]);
    const msgIdx = kinds.indexOf("message");
    const userIdx = kinds.indexOf("user");
    expect(userIdx).toBeGreaterThan(msgIdx);
    // The message is whole and the checkpoint rode along into the block.
    expect(store.blocks[msgIdx]).toMatchObject({ text: "hello" });
    expect(store.blocks[userIdx]).toMatchObject({
      kind: "user",
      text: "meanwhile do X",
      id: "q1",
      checkpoint: { id: "q1", preceding: "p0" },
    });
  });

  it("replay rebuilds the identical transcript order", () => {
    // Two stores fed the SAME journaled events must agree — the pending→blocks
    // transition is pure reducer, so a reconnect/replay is byte-for-byte equal.
    const live = fold(QUEUED_MID_TURN);
    const replay = fold(QUEUED_MID_TURN);
    expect(replay.blocks).toEqual(live.blocks);
    expect(replay.pendingSends).toEqual(live.pendingSends);
    expect(replay.blocks.map((b) => b.kind)).toEqual(["message", "turn_end", "user"]);
  });

  it("a cancelled send vanishes from both the stack and the transcript", () => {
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      { type: "message_chunk", turn_id: "t1", text: "working" },
      { type: "user_message", text: "oops nvm", id: "q1", queued: true },
      { type: "checkpoint", user_message_id: "q1", preceding_uuid: "p0" },
      { type: "user_message_update", id: "q1", state: "cancelled" },
    ]);
    expect(store.pendingSends).toHaveLength(0);
    expect(store.blocks.some((b) => b.kind === "user")).toBe(false);
    // The agent's message is untouched (never split).
    expect(store.blocks.filter((b) => b.kind === "message")).toHaveLength(1);
  });

  it("a dropped send stays in the stack as not-delivered, never in the transcript", () => {
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      { type: "user_message", text: "run this too", id: "q1", queued: true },
      { type: "turn_aborted", turn_id: "t1", reason: "interrupted", interrupted: true },
      { type: "user_message_update", id: "q1", state: "dropped" },
    ]);
    expect(store.blocks.some((b) => b.kind === "user")).toBe(false);
    expect(store.pendingSends).toHaveLength(1);
    expect(store.pendingSends[0]).toMatchObject({ id: "q1", state: "dropped" });
  });

  it("a fresh (turn-opening) send goes straight into the transcript", () => {
    const store = fold([
      { type: "user_message", text: "hi", id: "u1", queued: false },
      { type: "checkpoint", user_message_id: "u1", preceding_uuid: null },
      { type: "turn_started", turn_id: "t1" },
      { type: "message_chunk", turn_id: "t1", text: "hello" },
    ]);
    expect(store.pendingSends).toHaveLength(0);
    expect(store.blocks.map((b) => b.kind)).toEqual(["user", "message"]);
    expect(store.blocks[0]).toMatchObject({
      kind: "user",
      text: "hi",
      id: "u1",
      checkpoint: { id: "u1", preceding: null },
    });
  });
});
