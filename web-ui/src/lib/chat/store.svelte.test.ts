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

  it("a stop delivers the queue after the abort: aborted turn, then the sent bubble", () => {
    // The driver's stop semantics: TurnAborted first, then the held send
    // flushes `sent` — the bubble lands AFTER the aborted turn, and later
    // response chunks open a fresh block (the abort is never spliced).
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      { type: "message_chunk", turn_id: "t1", text: "half an ans" },
      { type: "user_message", text: "queued during t1", id: "q1", queued: true },
      { type: "turn_aborted", turn_id: "t1", reason: "interrupted", interrupted: true },
      { type: "user_message_update", id: "q1", state: "sent" },
      { type: "turn_started", turn_id: "t2" },
      { type: "message_chunk", turn_id: "t2", text: "answering the queued one" },
    ]);
    expect(store.pendingSends).toHaveLength(0);
    // The abort renders its "stopped" notice, THEN the delivered bubble, then
    // its fresh answer — the queued send survives the stop, in order.
    expect(store.blocks.map((b) => b.kind)).toEqual(["message", "notice", "user", "message"]);
    expect(store.blocks[2]).toMatchObject({ kind: "user", id: "q1" });
  });

  it("the ✕ tombstone dismisses a dropped bubble and no-ops for a delivered one", () => {
    // Dismiss: dropped → cancelled removes it from the stack (replay-stable).
    const dismissed = fold([
      { type: "user_message", text: "never made it", id: "q1", queued: true },
      { type: "user_message_update", id: "q1", state: "dropped" },
      { type: "user_message_update", id: "q1", state: "cancelled" },
    ]);
    expect(dismissed.pendingSends).toHaveLength(0);
    expect(dismissed.blocks.some((b) => b.kind === "user")).toBe(false);
    // No-op: sent → cancelled leaves the delivered message untouched (a late
    // ✕ click racing the flush can't un-say it).
    const delivered = fold([
      { type: "user_message", text: "made it", id: "q2", queued: true },
      { type: "user_message_update", id: "q2", state: "sent" },
      { type: "user_message_update", id: "q2", state: "cancelled" },
    ]);
    expect(delivered.pendingSends).toHaveLength(0);
    expect(delivered.blocks.filter((b) => b.kind === "user")).toHaveLength(1);
    expect(delivered.blocks[0]).toMatchObject({ kind: "user", id: "q2", text: "made it" });
  });

  it("a codex-style allow (option 'accept') marks the tool allowed, never denied", () => {
    // The bug: codex allow ids are `accept*`, not `allow_*`, so the old
    // id-prefix check marked every ALLOWED codex command denied → "1 command
    // failed". The mapping now reads the resolved option's KIND.
    const store = fold([
      { type: "tool_call", id: "c1", kind: "execute", title: "sed -i …", status: "in_progress" },
      {
        type: "permission_request",
        request_id: "r1",
        tool_call_id: "c1",
        title: "Run command",
        options: [
          { id: "accept", label: "Allow", kind: "allow_once" },
          { id: "decline", label: "Deny", kind: "reject_once" },
        ],
      },
      { type: "permission_resolved", request_id: "r1", option_id: "accept" },
      { type: "tool_call_update", id: "c1", status: "completed" },
    ]);
    const tool = store.blocks.find((b) => b.kind === "tool");
    expect(tool).toMatchObject({ kind: "tool", allowed: true, denied: false, status: "completed" });
  });

  it("a deny (option 'decline') marks the tool denied, not allowed", () => {
    const store = fold([
      { type: "tool_call", id: "c1", kind: "execute", title: "rm -rf …", status: "in_progress" },
      {
        type: "permission_request",
        request_id: "r1",
        tool_call_id: "c1",
        title: "Run command",
        options: [
          { id: "accept", label: "Allow", kind: "allow_once" },
          { id: "decline", label: "Deny", kind: "reject_once" },
        ],
      },
      { type: "permission_resolved", request_id: "r1", option_id: "decline" },
    ]);
    const tool = store.blocks.find((b) => b.kind === "tool");
    expect(tool).toMatchObject({ kind: "tool", denied: true, allowed: false });
  });

  it("reconciles a tool whose completion update never arrived at turn end", () => {
    // The stuck-"running" bug: a big image Read's result frame blows the
    // transport's per-line cap and is dropped below the event layer, so the
    // tool_call_update never lands. When the turn completes, the row must not
    // keep spinning "in_progress" (its ToolGroup would never collapse).
    const events = [
      { type: "user_message", text: "review the figures", id: "u1", queued: false },
      { type: "turn_started", turn_id: "t1" },
      { type: "tool_call", id: "r1", kind: "read", title: "Read: fig.png", status: "in_progress" },
      // No tool_call_update for r1 — its result frame was dropped.
      { type: "message_chunk", turn_id: "t1", text: "looks good" },
      { type: "turn_completed", turn_id: "t1", usage: { output_tokens: 2 } },
    ];
    const store = fold(events);
    const tool = store.blocks.find((b) => b.kind === "tool");
    expect(tool).toMatchObject({ kind: "tool", id: "r1", status: "completed" });
    // Replay agrees — the reconciliation is a pure reducer over the journal.
    expect(fold(events).blocks).toEqual(store.blocks);
  });

  it("reconciles a dangling tool when the driver dies with a fatal error", () => {
    // A fatal error is a terminal path like turn end: a kept-visible
    // ProtocolError session emits no `exited`, so a tool left in_progress must
    // not keep spinning.
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      { type: "tool_call", id: "r1", kind: "read", title: "Read: fig.png", status: "in_progress" },
      { type: "error", message: "driver protocol error", fatal: true },
    ]);
    const tool = store.blocks.find((b) => b.kind === "tool");
    expect(tool).toMatchObject({ kind: "tool", id: "r1", status: "completed" });
    expect(store.running).toBe(false);
  });

  it("re-arms the thinking push on a fresh driver init", () => {
    // The pooled thinking preference must be re-pushed to each new driver
    // process (a fresh CLI defaults thinking off) — `init` resets the flag.
    const store = fold([{ type: "turn_started", turn_id: "t1" }]);
    store.markThinkingPushed();
    expect(store.thinkingPushed).toBe(true);
    store.apply({ seq: 99, ts: 99, ev: { type: "init", model: "claude-x" } } as SeqEvent);
    expect(store.thinkingPushed).toBe(false);
  });

  it("leaves an already-completed tool from a prior turn untouched on a later turn end", () => {
    // The scan stops at the previous turn_end, so reconciliation only closes
    // the CURRENT turn's dangling rows — it never rewrites settled history.
    const store = fold([
      { type: "tool_call", id: "a1", kind: "execute", title: "ls", status: "in_progress" },
      { type: "tool_call_update", id: "a1", status: "failed" },
      { type: "turn_completed", turn_id: "t1", usage: { output_tokens: 1 } },
      { type: "turn_started", turn_id: "t2" },
      { type: "tool_call", id: "b1", kind: "read", title: "Read: x.png", status: "in_progress" },
      { type: "turn_completed", turn_id: "t2", usage: { output_tokens: 1 } },
    ]);
    const a1 = store.blocks.find((b) => b.kind === "tool" && b.id === "a1");
    const b1 = store.blocks.find((b) => b.kind === "tool" && b.id === "b1");
    expect(a1).toMatchObject({ status: "failed" }); // prior turn's outcome preserved
    expect(b1).toMatchObject({ status: "completed" }); // this turn's dangling row closed
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
