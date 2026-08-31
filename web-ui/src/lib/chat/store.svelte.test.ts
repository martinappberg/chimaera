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

describe("ChatStore block-boundary normalization", () => {
  it("strips driver separators that open a NEW block and drops whitespace-only chunks", () => {
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      { type: "message_chunk", turn_id: "t1", text: "before the thought" },
      { type: "thought_chunk", turn_id: "t1", text: "thinking" },
      // The drivers mark paragraph breaks at block boundaries; when a thought
      // (or tool card) split the same-kind stream, the break arrives at the
      // START of a fresh block and must not render/copy as leading blanks.
      { type: "message_chunk", turn_id: "t1", text: "\n\nafter the thought" },
      // A whitespace-only chunk must not mint a phantom empty bubble.
      { type: "thought_chunk", turn_id: "t1", text: "more thinking" },
      { type: "message_chunk", turn_id: "t1", text: "\n\n" },
    ]);
    const texts = store.blocks
      .filter((b) => b.kind === "message" || b.kind === "thought")
      .map((b) => ({ kind: b.kind, text: b.text }));
    expect(texts).toEqual([
      { kind: "message", text: "before the thought" },
      { kind: "thought", text: "thinking" },
      { kind: "message", text: "after the thought" },
      { kind: "thought", text: "more thinking" },
    ]);
  });

  it("keeps separators that CONTINUE a block verbatim", () => {
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      { type: "message_chunk", turn_id: "t1", text: "para one" },
      { type: "message_chunk", turn_id: "t1", text: "\n\npara two" },
    ]);
    const msg = store.blocks.find((b) => b.kind === "message");
    expect(msg?.text).toBe("para one\n\npara two");
  });
});

describe("ChatStore transcript activity", () => {
  it("separates visible conversation changes from control telemetry", () => {
    const store = new ChatStore();
    store.apply({ seq: 1, ts: 1, ev: { type: "rate_limit", utilization: 12 } } as SeqEvent);
    store.apply({ seq: 2, ts: 2, ev: { type: "mode_changed", mode_id: "ask" } } as SeqEvent);
    expect(store.lastSeq).toBe(2);
    expect(store.transcriptVersion).toBe(0);

    store.apply({
      seq: 3,
      ts: 3,
      ev: { type: "message_chunk", turn_id: "t1", text: "visible" },
    } as SeqEvent);
    expect(store.transcriptVersion).toBe(1);

    const afterChunk = store.transcriptVersion;
    store.notice("local feedback", "info");
    expect(store.transcriptVersion).toBe(afterChunk + 1);
  });
});

describe("ChatStore context compaction", () => {
  it("keeps progress replay-safe and settles with the summarized token count", () => {
    const started = fold([
      { type: "turn_started", turn_id: "compact-1" },
      { type: "context_compaction", phase: "started" },
    ]);
    expect(started.running).toBe(true);
    expect(started.compacting).toBe(true);

    const events = [
      { type: "turn_started", turn_id: "compact-1" },
      { type: "context_compaction", phase: "started" },
      { type: "context_compaction", phase: "completed", pre_tokens: 168_000 },
      { type: "turn_completed", turn_id: "compact-1", usage: {} },
    ];
    const live = fold(events);
    const replay = fold(events);
    expect(live.compacting).toBe(false);
    expect(replay.blocks).toEqual(live.blocks);
    const notice = live.blocks.find(
      (b) => b.kind === "notice" && b.text.includes("tokens summarized"),
    );
    expect(notice).toMatchObject({ kind: "notice", tone: "info" });
    expect(notice?.kind === "notice" ? notice.text : "").toContain("168");
  });

  it("clears failed or terminally-incomplete progress without duplicating agent output", () => {
    const failed = fold([
      { type: "context_compaction", phase: "started" },
      { type: "context_compaction", phase: "failed" },
    ]);
    expect(failed.compacting).toBe(false);
    expect(failed.blocks).toEqual([]);

    const missingTerminalItem = fold([
      { type: "turn_started", turn_id: "compact-2" },
      { type: "context_compaction", phase: "started" },
      { type: "turn_aborted", turn_id: "compact-2", reason: "interrupted", interrupted: true },
    ]);
    expect(missingTerminalItem.compacting).toBe(false);
  });
});

describe("ChatStore pending-send ordering", () => {
  it("tracks exact portable boundaries and completed-turn native fork points", () => {
    const partial = fold([
      { type: "user_message", text: "question", id: "u1", queued: false },
      { type: "checkpoint", user_message_id: "u1", preceding_uuid: null },
      { type: "turn_started", turn_id: "t1" },
      { type: "message_chunk", turn_id: "t1", text: "ans" },
      { type: "message_chunk", turn_id: "t1", text: "wer" },
    ]);
    expect(partial.blocks[0]).toMatchObject({
      kind: "user",
      forkSeq: 2,
      checkpoint: { id: "u1" },
    });
    expect(partial.blocks[1]).toMatchObject({
      kind: "message",
      text: "answer",
      sentAtMs: 3,
      forkSeq: 5,
      nativeTurnComplete: false,
    });

    partial.apply({
      seq: 6,
      ts: 6,
      ev: { type: "turn_completed", turn_id: "t1", usage: {} },
    } as SeqEvent);
    expect(partial.blocks[1]).toMatchObject({
      forkSeq: 6,
      nativeTurnComplete: true,
      turnId: "t1",
    });

    partial.apply({
      seq: 7,
      ts: 7,
      ev: { type: "forked", source_agent: "codex", source_seq: 6, native: false },
    } as SeqEvent);
    expect(partial.blocks[0]).toMatchObject({ checkpoint: null });
    expect(partial.blocks[1]).toMatchObject({ nativeTurnComplete: false });
  });

  it("clears source-process telemetry at a portable fork marker", () => {
    const store = fold([
      {
        type: "init",
        model: "claude-source",
        current_mode: "source-mode",
        modes: [{ id: "source-mode", label: "Source" }],
        slash_commands: [{ name: "source-command" }],
        models: [{ id: "source-model", label: "Source model", efforts: ["high"] }],
      },
      { type: "effort_state", effort: "high", ultracode: true },
      { type: "context_usage", percentage: 72, total_tokens: 720, max_tokens: 1_000 },
      {
        type: "rate_limit",
        utilization: 81,
        label: "source weekly",
        resets_at: "tomorrow",
        limit_reached: false,
      },
      {
        type: "rewind_result",
        user_message_id: "u1",
        can_rewind: true,
        files_changed: ["source.txt"],
        applied: false,
      },
      { type: "mcp_servers", servers: [{ name: "source", status: "connected", tools: 3 }] },
      { type: "prompt_suggestion", text: "source suggestion" },
      {
        type: "plan",
        entries: [{ content: "source plan", status: "in_progress", id: "1" }],
      },
      { type: "error", message: "source process failed", fatal: true },
      { type: "forked", source_agent: "claude", source_seq: 9, native: false },
    ]);

    expect(store.model).toBeNull();
    expect(store.modes).toEqual([]);
    expect(store.currentMode).toBeNull();
    expect(store.slashCommands).toEqual([]);
    expect(store.models).toEqual([]);
    expect(store.effort).toBeNull();
    expect(store.ultracode).toBe(false);
    expect(store.contextPct).toBeNull();
    expect(store.contextTokens).toBeNull();
    expect(store.rateLimit).toBeNull();
    expect(store.rewind).toBeNull();
    expect(store.mcpServers).toBeNull();
    expect(store.promptSuggestion).toBeNull();
    expect(store.fatalError).toBeNull();
    expect(store.plan).toEqual([]);
    expect(store.exited).toBeNull();
    expect(store.degraded).toBe(false);
  });

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

  it("upserts repeated permission/question identities without duplicate keyed cards", () => {
    const store = fold([
      {
        type: "permission_request",
        request_id: "perm-1",
        title: "old title",
        options: [{ id: "allow", label: "Allow", kind: "allow_once" }],
      },
      {
        type: "permission_request",
        request_id: "perm-1",
        title: "current title",
        options: [
          { id: "allow", label: "Allow", kind: "allow_once" },
          { id: "allow", label: "Duplicate", kind: "allow_once" },
          { id: "deny", label: "Deny", kind: "reject_once" },
        ],
      },
      {
        type: "question_request",
        request_id: "ask-1",
        questions: [{ id: "scope", question: "Old?", options: [] }],
      },
      {
        type: "question_request",
        request_id: "ask-1",
        questions: [
          { id: "scope", question: "Current?", options: [] },
          { id: "scope", question: "Duplicate?", options: [] },
        ],
      },
    ]);

    expect(store.pending).toHaveLength(1);
    expect(store.pending[0]).toMatchObject({ requestId: "perm-1", title: "current title" });
    expect(store.pending[0].options.map((option) => option.id)).toEqual(["allow", "deny"]);
    expect(store.questions).toHaveLength(1);
    expect(store.questions[0].questions).toHaveLength(1);
    expect(store.questions[0].questions[0]).toMatchObject({ id: "scope", question: "Current?" });
    expect(store.blocks.filter((block) => block.kind === "question")).toHaveLength(1);
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
    expect(tool).toMatchObject({
      kind: "tool",
      id: "r1",
      status: "completed",
      streaming: false,
    });
    // Replay agrees — the reconciliation is a pure reducer over the journal.
    expect(fold(events).blocks).toEqual(store.blocks);
  });

  it("keeps a cross-turn Codex agent live after the parent turn", () => {
    const events = [
      { type: "turn_started", turn_id: "parent" },
      {
        type: "tool_call",
        id: "agent:sub-1",
        kind: "agent",
        title: "Agent: analyst",
        status: "in_progress",
        cross_turn: true,
      },
      { type: "turn_completed", turn_id: "parent", usage: {} },
      {
        type: "tool_call_update",
        id: "agent:sub-1",
        status: "in_progress",
        content: { kind: "output", text: "running tests" },
      },
    ];
    const store = fold(events);
    const live = store.blocks.find((b) => b.kind === "tool" && b.id === "agent:sub-1");
    expect(live).toMatchObject({ status: "in_progress", crossTurn: true });
    expect(live).toMatchObject({ content: { kind: "output", text: "running tests" } });

    store.apply({
      seq: events.length + 1,
      ts: events.length,
      ev: { type: "tool_call_update", id: "agent:sub-1", status: "completed" },
    } as SeqEvent);
    expect(live).toMatchObject({ status: "completed" });
  });

  it("closes a cross-turn Codex agent when a new driver process starts", () => {
    const store = fold([
      { type: "init", native_session_id: "old-process" },
      { type: "turn_started", turn_id: "parent" },
      {
        type: "tool_call",
        id: "agent:sub-1",
        kind: "agent",
        title: "Agent: analyst",
        status: "in_progress",
        cross_turn: true,
      },
      { type: "turn_completed", turn_id: "parent", usage: {} },
      // Every spawn emits this reset before Init, but it only owns the
      // background-task lane. The fresh Init owns stale tool-row cleanup.
      { type: "background_tasks", tasks: [] },
      { type: "init", native_session_id: "new-process" },
    ]);

    const stale = store.blocks.find((b) => b.kind === "tool" && b.id === "agent:sub-1");
    expect(stale).toMatchObject({ status: "completed", crossTurn: true });
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
    store.markThinkingPending();
    expect(store.thinkingPushed).toBe(false);
    store.markThinkingPushed();
    store.apply({ seq: 99, ts: 99, ev: { type: "init", model: "claude-x" } } as SeqEvent);
    expect(store.thinkingPushed).toBe(false);
  });

  it("treats init as a complete catalog snapshot", () => {
    const store = fold([
      {
        type: "init",
        model: "old-model",
        current_mode: "old-mode",
        modes: [{ id: "old-mode", label: "Old" }],
        slash_commands: [{ name: "old-command" }],
        models: [{ id: "old-model", label: "Old model" }],
      },
      // Empty vectors/options are omitted by serde. They still mean the new
      // driver has no catalog/state, not "keep the previous process's".
      { type: "init", native_session_id: "new-driver" },
    ]);
    expect(store.model).toBeNull();
    expect(store.currentMode).toBeNull();
    expect(store.modes).toEqual([]);
    expect(store.slashCommands).toEqual([]);
    expect(store.models).toEqual([]);
  });

  it("keeps client-side notices under the transcript cap", () => {
    const store = new ChatStore();
    for (let i = 0; i < 2_100; i++) store.notice(`offline ${i}`, "error");
    // Crossed the hysteresis threshold once (at 2065 → trimmed back to the
    // cap), then grew the remaining 35 into the slack.
    expect(store.blocks).toHaveLength(2_035);
    expect(store.blocks[0]).toMatchObject({ kind: "notice", text: "earlier history trimmed" });
    expect(store.virtualTotal).toBe(2_100);
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

  it("a late in_progress update never walks a finished tool back to running", () => {
    // The driver's per-turn map wipe makes this unreachable today; the guard
    // keeps it that way once cross-turn background tasks start streaming.
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      { type: "tool_call", id: "a1", kind: "agent", title: "Task: probe", status: "in_progress" },
      { type: "tool_call_update", id: "a1", status: "completed" },
      {
        type: "tool_call_update",
        id: "a1",
        status: "in_progress",
        content: { kind: "output", text: "straggler line" },
      },
    ]);
    const a1 = store.blocks.find((b) => b.kind === "tool" && b.id === "a1");
    // Status holds; the straggler's content still lands.
    expect(a1).toMatchObject({ status: "completed" });
    expect(a1).toMatchObject({ content: { kind: "output", text: "straggler line" } });
  });

  it("a late output delta enriches a finished tool without reviving its cursor", () => {
    const store = fold([
      { type: "tool_call", id: "a1", kind: "execute", title: "build", status: "in_progress" },
      { type: "tool_output_delta", id: "a1", text: "first\n" },
      { type: "tool_call_update", id: "a1", status: "completed" },
      { type: "tool_output_delta", id: "a1", text: "late\n" },
    ]);
    const tool = store.blocks.find((b) => b.kind === "tool" && b.id === "a1");
    expect(tool).toMatchObject({
      status: "completed",
      streaming: false,
      content: { kind: "output", text: "first\nlate\n" },
    });
  });

  it("a journal reset clears the plan and turn state with the transcript", () => {
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      { type: "plan", entries: [{ content: "step 1", status: "in_progress" }] },
    ]);
    expect(store.plan).toHaveLength(1);
    expect(store.running).toBe(true);
    // The journal was pruned/recreated server-side: head below our lastSeq.
    store.onReady(
      {
        id: "s1",
        agent: "claude",
        alive: true,
        exit_status: null,
        native_session_id: null,
        model: null,
        current_mode: null,
        pending_permission: false,
      },
      0,
      0,
    );
    expect(store.blocks).toHaveLength(0);
    expect(store.plan).toHaveLength(0);
    expect(store.running).toBe(false);
    expect(store.activity).toBeNull();
  });

  it("holds initial rendering until replay reaches the advertised journal head", () => {
    const store = new ChatStore();
    const session = {
      id: "s1",
      agent: "claude",
      alive: true,
      exit_status: null,
      native_session_id: null,
      model: null,
      current_mode: null,
      pending_permission: false,
    };
    store.onReady(session, 0, 3);
    expect(store.hydrating).toBe(true);

    store.apply({ seq: 1, ts: 1, ev: { type: "user_message", text: "oldest" } } as SeqEvent);
    store.apply({ seq: 2, ts: 2, ev: { type: "message_chunk", turn_id: "t1", text: "middle" } } as SeqEvent);
    expect(store.hydrating).toBe(true);

    // A reconnect partway through keeps the partial transcript gated.
    store.onReady(session, 2, 3);
    expect(store.hydrating).toBe(true);
    store.apply({ seq: 3, ts: 3, ev: { type: "turn_completed", turn_id: "t1", usage: {} } } as SeqEvent);
    expect(store.hydrating).toBe(false);
  });

  it("preserves Codex question auto-resolution deadlines across replay", () => {
    const events = [
      {
        type: "question_request",
        request_id: "codex-91",
        expires_at_ms: 1_800_000_000_000,
        questions: [{ id: "scope", question: "Which scope?", options: [] }],
      },
    ];
    const live = fold(events);
    const replay = fold(events);
    expect(live.questions[0]).toMatchObject({
      requestId: "codex-91",
      expiresAtMs: 1_800_000_000_000,
    });
    expect(replay.questions).toEqual(live.questions);

    const oldJournal = fold([
      {
        type: "question_request",
        request_id: "codex-92",
        questions: [{ id: "scope", question: "Which scope?" }],
      },
    ]);
    expect(oldJournal.questions[0].expiresAtMs).toBeNull();
  });
});

describe("ChatStore background tasks", () => {
  const BG = (over: Record<string, unknown> = {}): Record<string, unknown> => ({
    id: "bg-1",
    task_type: "local_bash",
    description: "sleep 30",
    status: "running",
    started_at_ms: 1000,
    ...over,
  });

  it("replaces the set on every event (level-set, never a patch)", () => {
    const store = fold([
      { type: "background_tasks", tasks: [BG()] },
      { type: "background_tasks", tasks: [BG({ id: "bg-2", description: "make -j" })] },
    ]);
    // The second event REPLACED the set — bg-1 is gone, only bg-2 remains.
    expect(store.backgroundTasks).toHaveLength(1);
    expect(store.backgroundTasks[0]).toMatchObject({
      id: "bg-2",
      taskType: "local_bash",
      description: "make -j",
      status: "running",
      startedAtMs: 1000,
    });
  });

  it("parses a workflow lane's name and per-agent progress", () => {
    const store = fold([
      {
        type: "background_tasks",
        tasks: [
          BG({
            id: "wf-1",
            task_type: "local_workflow",
            description: "sweep the repo",
            workflow_name: "probe",
            agents: [
              { index: 1, label: "agent 1", state: "done", result_preview: "ok" },
              { index: 2, label: "agent 2", state: "start" },
            ],
            agents_total: 2,
            agents_done: 1,
          }),
        ],
      },
    ]);
    expect(store.backgroundTasks[0]).toMatchObject({
      workflowName: "probe",
      agentsTotal: 2,
      agentsDone: 1,
    });
    expect(store.backgroundTasks[0].agents).toEqual([
      { index: 1, label: "agent 1", state: "done", resultPreview: "ok" },
      { index: 2, label: "agent 2", state: "start", resultPreview: null },
    ]);
    // Absent workflow fields (a bash lane, an old journal) parse to calm
    // defaults — no undefined leaking into the tray's render.
    const bash = fold([{ type: "background_tasks", tasks: [BG()] }]);
    expect(bash.backgroundTasks[0]).toMatchObject({
      workflowName: null,
      agents: [],
      agentsTotal: 0,
      agentsDone: 0,
    });
  });

  it("background card ticks never flick a running turn's activity", () => {
    // A workflow's "N/M agents done" updates land on its long-COMPLETED
    // launch card while an unrelated turn runs a tool. Only a genuine
    // in_progress→terminal transition hands the floor back — repeated
    // updates to an already-terminal card must leave the activity alone.
    const store = fold([
      // The workflow launched in an earlier turn; its card completed.
      { type: "tool_call", id: "wf-card", kind: "other", title: "Workflow", status: "in_progress" },
      { type: "tool_call_update", id: "wf-card", status: "completed" },
      // A new turn is running a tool — that's the live activity.
      { type: "turn_started", turn_id: "t2" },
      { type: "tool_call", id: "c9", kind: "execute", title: "make -j", status: "in_progress" },
      // Background workflow transition ticks the completed card.
      {
        type: "tool_call_update",
        id: "wf-card",
        status: "in_progress",
        content: { kind: "output", text: "1/4 agents done" },
      },
      // …and its close verdict re-completes it.
      {
        type: "tool_call_update",
        id: "wf-card",
        status: "completed",
        content: { kind: "output", text: "workflow “probe” completed · 4/4 agents · 4s" },
      },
    ]);
    expect(store.activity).toMatchObject({ kind: "tool", detail: "make -j" });
    // The genuine completion of the RUNNING tool still hands the floor back.
    const done = fold([
      { type: "turn_started", turn_id: "t2" },
      { type: "tool_call", id: "c9", kind: "execute", title: "make -j", status: "in_progress" },
      { type: "tool_call_update", id: "c9", status: "completed" },
    ]);
    expect(done.activity).toMatchObject({ kind: "waiting" });
  });

  it("dedupes agent indexes so the keyed dot render can't throw", () => {
    // Same defense as the task-id filter one level down: a corrupt line or
    // an older build's journal can carry duplicate indexes, and Svelte's
    // keyed each throws on a repeated key.
    const store = fold([
      {
        type: "background_tasks",
        tasks: [
          BG({
            id: "wf-1",
            task_type: "local_workflow",
            agents: [
              { index: 1, label: "a", state: "start" },
              { index: 1, label: "b", state: "done" },
              { label: "no index", state: "start" },
              { label: "also none", state: "start" },
            ],
          }),
        ],
      },
    ]);
    const indexes = store.backgroundTasks[0].agents.map((a) => a.index);
    expect(indexes).toEqual([...new Set(indexes)]);
  });

  it("keeps the newest duplicate task id so the keyed tray cannot throw", () => {
    const store = fold([
      {
        type: "background_tasks",
        tasks: [
          BG({ id: "same", description: "stale", status: "running" }),
          BG({ id: "same", description: "current", status: "waiting" }),
        ],
      },
    ]);
    expect(store.backgroundTasks).toHaveLength(1);
    expect(store.backgroundTasks[0]).toMatchObject({
      id: "same",
      description: "current",
      status: "waiting",
    });
  });

  it("folds a close verdict into history as a notice and empties the set", () => {
    const store = fold([
      { type: "background_tasks", tasks: [BG()] },
      {
        type: "background_tasks",
        tasks: [],
        closed: [{ id: "bg-1", description: "sleep 30", status: "completed", summary: "exit 0" }],
      },
    ]);
    expect(store.backgroundTasks).toHaveLength(0);
    const notices = store.blocks.filter((b) => b.kind === "notice");
    expect(notices).toHaveLength(1);
    expect(notices[0]).toMatchObject({ tone: "info" });
    expect((notices[0] as { text: string }).text).toContain("sleep 30");
    expect((notices[0] as { text: string }).text).toContain("completed");
    expect((notices[0] as { text: string }).text).toContain("exit 0");
  });

  it("renders a self-contained wire summary alone (no stutter)", () => {
    // The natural-close summary already names the command AND the verdict
    // (live shape: 'Background command "…" completed (exit code 0)') —
    // rendering desc + status + summary would say everything twice.
    const store = fold([
      { type: "background_tasks", tasks: [BG()] },
      {
        type: "background_tasks",
        tasks: [],
        closed: [
          {
            id: "bg-1",
            description: "sleep 30",
            status: "completed",
            summary: 'Background command "sleep 30" completed (exit code 0)',
          },
        ],
      },
    ]);
    const notices = store.blocks.filter((b) => b.kind === "notice");
    expect((notices[0] as { text: string }).text).toBe(
      'Background command "sleep 30" completed (exit code 0)',
    );
  });

  it("renders a failed verdict as an error notice", () => {
    const store = fold([
      { type: "background_tasks", tasks: [BG()] },
      {
        type: "background_tasks",
        tasks: [],
        closed: [{ id: "bg-1", description: "sleep 30", status: "failed" }],
      },
    ]);
    const notices = store.blocks.filter((b) => b.kind === "notice");
    expect(notices[0]).toMatchObject({ tone: "error" });
  });

  it("survives a turn end and model switch, dies with the process", () => {
    // Cross-turn: the turn ending does not clear the set (that's the point
    // of background work). Neither does a ModelSwitched event while the tasks
    // still run; the old fake-Init model refresh could expire unrelated state.
    // The lifecycle ends are a driver exit / fatal error (the tasks were the
    // CLI's children), and the manager journals an empty level-set before a
    // replacement driver's Init so replay agrees.
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      { type: "background_tasks", tasks: [BG()] },
      { type: "turn_completed", turn_id: "t1", usage: {} },
      {
        type: "model_switched",
        from: "claude-old",
        to: "claude-new",
        retract_current_turn: false,
      },
    ]);
    expect(store.backgroundTasks).toHaveLength(1);
    store.apply({ seq: 5, ts: 5, ev: { type: "exited", status: 0 } } as SeqEvent);
    expect(store.backgroundTasks).toHaveLength(0);

    const fatal = fold([
      { type: "background_tasks", tasks: [BG()] },
      { type: "error", message: "driver died", fatal: true },
    ]);
    expect(fatal.backgroundTasks).toHaveLength(0);
  });

  it("replay converges on the last set event", () => {
    const events = [
      { type: "background_tasks", tasks: [BG()] },
      { type: "background_tasks", tasks: [BG(), BG({ id: "bg-2", description: "audit" })] },
      {
        type: "background_tasks",
        tasks: [BG({ id: "bg-2", description: "audit" })],
        closed: [{ id: "bg-1", description: "sleep 30", status: "stopped" }],
      },
    ];
    const live = fold(events);
    const replay = fold(events);
    expect(replay.backgroundTasks).toEqual(live.backgroundTasks);
    expect(replay.blocks).toEqual(live.blocks);
    expect(live.backgroundTasks.map((t) => t.id)).toEqual(["bg-2"]);
  });
});

describe("ChatStore transcript cap: hysteresis, uids, virtual total", () => {
  const CAP = 2_000;
  const SLACK = 64;

  it("trims in batches: the array runs to cap+slack, then settles at the cap", () => {
    const store = new ChatStore();
    for (let i = 0; i < CAP + SLACK; i++) store.notice(`n${i}`, "info");
    // At the threshold, still untrimmed — the O(n) rebuild has not run once.
    expect(store.blocks).toHaveLength(CAP + SLACK);
    expect(store.trimmedCount).toBe(0);

    store.notice("over", "info");
    expect(store.blocks).toHaveLength(CAP);
    expect(store.blocks[0]).toMatchObject({ kind: "notice", text: "earlier history trimmed" });
    expect(store.trimmedCount).toBe(SLACK + 1);

    // The next trim needs another full slack of appends, not one per event.
    for (let i = 0; i < SLACK; i++) store.notice(`m${i}`, "info");
    expect(store.blocks).toHaveLength(CAP + SLACK);
    expect(store.trimmedCount).toBe(SLACK + 1);
    store.notice("over again", "info");
    expect(store.blocks).toHaveLength(CAP);
    expect(store.trimmedCount).toBe(2 * (SLACK + 1));
  });

  it("the virtual total keeps counting appends at the cap", () => {
    const store = new ChatStore();
    const appended = CAP + SLACK + 40; // one trim landed along the way
    for (let i = 0; i < appended; i++) store.notice(`n${i}`, "info");
    expect(store.trimmedCount).toBeGreaterThan(0);
    // blocks.length alone under-counts at cap; the virtual total still equals
    // every block ever appended, and grows by one per at-cap append — the
    // trim-stable coordinate system saved window cursors persist in.
    expect(store.virtualTotal).toBe(appended);
    const before = store.virtualTotal;
    store.notice("one more", "info");
    expect(store.virtualTotal).toBe(before + 1);
  });

  it("row uids are stable across trims and never repeat", () => {
    const store = new ChatStore();
    for (let i = 0; i < CAP + SLACK; i++) store.notice(`n${i}`, "info");
    const tailUid = store.blocks[store.blocks.length - 1].uid;
    store.notice("trigger trim", "info");
    // The surviving block kept its uid at its shifted array position.
    const survivor = store.blocks.find(
      (b) => b.kind === "notice" && b.text === `n${CAP + SLACK - 1}`,
    );
    expect(survivor?.uid).toBe(tailUid);
    const uids = store.blocks.map((b) => b.uid);
    expect(new Set(uids).size).toBe(uids.length);
  });

  it("id-indexed patches still land after a trim's index rebuild", () => {
    const store = new ChatStore();
    let seq = 0;
    const apply = (ev: Record<string, unknown>) =>
      store.apply({ seq: ++seq, ts: seq, ev } as SeqEvent);
    for (let i = 0; i < CAP; i++) apply({ type: "notice", text: `n${i}` });
    apply({ type: "tool_call", id: "t1", kind: "execute", title: "build", status: "in_progress" });
    for (let i = 0; i < 2 * SLACK; i++) apply({ type: "notice", text: `m${i}` });
    expect(store.trimmedCount).toBeGreaterThan(0);
    apply({ type: "tool_call_update", id: "t1", status: "completed" });
    const tool = store.blocks.find((b) => b.kind === "tool" && b.id === "t1");
    expect(tool).toMatchObject({ status: "completed" });
  });

  it("a checkpoint bumps the transcript version only when it touches a rendered block", () => {
    const store = new ChatStore();
    store.apply({
      seq: 1,
      ts: 1,
      ev: { type: "user_message", text: "queued", id: "q1", queued: true },
    } as SeqEvent);
    const afterQueue = store.transcriptVersion;
    // Lands on the still-queued send (pendingSends, not a block): nothing the
    // reader sees changes — must not defeat the activation early-out or light
    // the unread chip. The anchor rides along at promotion.
    store.apply({
      seq: 2,
      ts: 2,
      ev: { type: "checkpoint", user_message_id: "q1", preceding_uuid: "p0" },
    } as SeqEvent);
    expect(store.transcriptVersion).toBe(afterQueue);

    store.apply({
      seq: 3,
      ts: 3,
      ev: { type: "user_message", text: "sent", id: "u1", queued: false },
    } as SeqEvent);
    const afterUser = store.transcriptVersion;
    // Lands on a RENDERED user block (its rewind affordance mutates in
    // place): a frozen view must learn to reconcile, so the version bumps.
    store.apply({
      seq: 4,
      ts: 4,
      ev: { type: "checkpoint", user_message_id: "u1", preceding_uuid: "p1" },
    } as SeqEvent);
    expect(store.transcriptVersion).toBe(afterUser + 1);
  });

  it("a retract-and-reappend that nets out still reads as a structural change", () => {
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      { type: "message_chunk", turn_id: "t1", text: "the wrong answer" },
    ]);
    const structuralBefore = store.structuralVersion;
    const lengthBefore = store.blocks.length;
    const virtualBefore = store.virtualTotal;
    store.apply({ seq: 3, ts: 3, ev: { type: "messages_superseded" } } as SeqEvent);
    store.apply({
      seq: 4,
      ts: 4,
      ev: { type: "message_chunk", turn_id: "t2", text: "the retry" },
    } as SeqEvent);
    // Net lengths cancel out — even the virtual total — so only the
    // structural counter can tell a view its rendered rows are stale.
    expect(store.blocks.length).toBe(lengthBefore);
    expect(store.virtualTotal).toBe(virtualBefore);
    expect(store.structuralVersion).toBeGreaterThan(structuralBefore);
  });

  it("a journal reset bumps the transcript generation", () => {
    const store = fold([{ type: "user_message", text: "hi" }]);
    expect(store.epoch).toBe(0);
    // head below lastSeq ⇒ pruned/recreated journal: old coordinates (ranges,
    // trim counts, saved cursors) belong to a dead numbering system.
    store.onReady(
      {
        id: "s1",
        agent: "claude",
        alive: true,
        exit_status: null,
        native_session_id: null,
        model: null,
        current_mode: null,
        pending_permission: false,
      },
      0,
      0,
    );
    expect(store.epoch).toBe(1);
  });

  it("a journal reset zeroes the trim count but never recycles uids", () => {
    const store = new ChatStore();
    // Journaled events (they advance lastSeq, so ready's head-below-lastSeq
    // reset actually triggers), enough of them to cross the trim threshold.
    for (let i = 0; i < CAP + SLACK + 1; i++) {
      store.apply({ seq: i + 1, ts: i, ev: { type: "notice", text: `n${i}` } } as SeqEvent);
    }
    expect(store.trimmedCount).toBeGreaterThan(0);
    const maxUid = Math.max(...store.blocks.map((b) => b.uid));
    // head below lastSeq ⇒ the journal was pruned/recreated: hard reset.
    store.onReady(
      {
        id: "s1",
        agent: "claude",
        alive: true,
        exit_status: null,
        native_session_id: null,
        model: null,
        current_mode: null,
        pending_permission: false,
      },
      0,
      0,
    );
    expect(store.trimmedCount).toBe(0);
    store.notice("fresh", "info");
    // A keyed render may still hold pre-reset rows; a recycled uid would make
    // Svelte patch stale DOM into an unrelated block instead of remounting.
    expect(store.blocks[0].uid).toBeGreaterThan(maxUid);
  });
});

describe("ChatStore live tool-output cap", () => {
  // Mirrors chimaera-agent cap_output: 12 KiB head + 4 KiB rolling tail, one
  // 4 KiB slack before each re-slice.
  const HEAD = 12 * 1024;
  const TAIL = 4 * 1024;
  const SLACK = 4 * 1024;
  const CALL = { type: "tool_call", id: "x1", kind: "execute", title: "run", status: "in_progress" };
  const outputOf = (store: ChatStore) => {
    const tool = store.blocks.find((b) => b.kind === "tool" && b.id === "x1");
    if (tool?.kind !== "tool" || tool.content?.kind !== "output") throw new Error("no output");
    return tool.content;
  };

  it("keeps small streams verbatim, unmarked", () => {
    const store = fold([CALL, { type: "tool_output_delta", id: "x1", text: "a".repeat(1000) }]);
    const content = outputOf(store);
    expect(content.text).toBe("a".repeat(1000));
    expect(content.truncated).not.toBe(true);
  });

  it("caps an overflowing stream to head + marker + tail, server-style", () => {
    const store = fold([CALL, { type: "tool_output_delta", id: "x1", text: "x".repeat(30000) }]);
    const content = outputOf(store);
    expect(content.truncated).toBe(true);
    expect(content.text!.startsWith("x".repeat(HEAD))).toBe(true);
    // The server's marker shape (model.rs cap_head_tail), byte count honest.
    expect(content.text).toContain(`… [${30000 - HEAD - TAIL} bytes omitted] …`);
    expect(content.text!.endsWith("x".repeat(TAIL))).toBe(true);
    expect(content.text!.length).toBeLessThanOrEqual(HEAD + TAIL + 40);
  });

  it("retained size stays bounded under a sustained stream, keeping the tail", () => {
    const deltas = Array.from({ length: 100 }, (_, i) => ({
      type: "tool_output_delta",
      id: "x1",
      text: `chunk-${String(i).padStart(3, "0")}-`.repeat(100), // 1.1 KB each
    }));
    const store = fold([CALL, ...deltas]);
    const content = outputOf(store);
    expect(content.truncated).toBe(true);
    // Head + rolling tail + slack + the marker line — never the ~110 KB fed.
    expect(content.text!.length).toBeLessThanOrEqual(HEAD + TAIL + SLACK + 40);
    expect(content.text!.endsWith("chunk-099-")).toBe(true);
    expect(content.text!.startsWith("chunk-000-")).toBe(true);
    expect(content.text).toContain("bytes omitted");
  });

  it("replay rebuilds the identical capped text (pure reducer)", () => {
    const events = [
      CALL,
      ...Array.from({ length: 40 }, (_, i) => ({
        type: "tool_output_delta",
        id: "x1",
        text: `line ${i} `.repeat(120),
      })),
    ];
    expect(outputOf(fold(events)).text).toBe(outputOf(fold(events)).text);
  });

  it("budgets are UTF-8 bytes and slices land on code-point boundaries", () => {
    // 3-byte € and 4-byte 🚀 deltas: 400 units of € is 1200 bytes, so ~25
    // deltas (30 000 bytes) must overflow the 20 480-byte enter threshold
    // even though the UTF-16 length (10 000 units) is far below it.
    const deltas = Array.from({ length: 25 }, () => ({
      type: "tool_output_delta",
      id: "x1",
      text: "€".repeat(398) + "🚀",
    }));
    const store = fold([CALL, ...deltas]);
    const content = outputOf(store);
    expect(content.truncated).toBe(true);
    expect(content.text).toMatch(/… \[\d+ bytes omitted\] …/);
    // No lone surrogates anywhere — the cuts respected code points.
    expect(/(^|[^\uD800-\uDBFF])[\uDC00-\uDFFF]|[\uD800-\uDBFF]($|[^\uDC00-\uDFFF])/u.test(
      content.text ?? "",
    )).toBe(false);
    expect(content.text!.endsWith("🚀")).toBe(true);
  });

  it("re-capping text that already carries the server marker absorbs it (never nests)", () => {
    const store = fold([
      CALL,
      {
        type: "tool_call_update",
        id: "x1",
        status: "completed",
        content: {
          kind: "output",
          text: `head-part\n… [999 bytes omitted] …\ntail-part`,
          truncated: true,
        },
      },
      // Straggler deltas push the server-capped text past the budget again.
      ...Array.from({ length: 30 }, () => ({
        type: "tool_output_delta",
        id: "x1",
        text: "y".repeat(1024),
      })),
    ]);
    const content = outputOf(store);
    const markers = (content.text ?? "").match(/bytes omitted/g) ?? [];
    expect(markers).toHaveLength(1);
    // The absorbed base count rides along in the merged marker.
    const omitted = Number(/\[(\d+) bytes omitted\]/.exec(content.text ?? "")?.[1]);
    expect(omitted).toBeGreaterThanOrEqual(999);
    expect(content.text!.startsWith("head-part")).toBe(true);
  });

  it("the authoritative result replaces the capped live text entirely", () => {
    const store = fold([
      CALL,
      { type: "tool_output_delta", id: "x1", text: "x".repeat(30000) },
      {
        type: "tool_call_update",
        id: "x1",
        status: "completed",
        content: { kind: "output", text: "authoritative", truncated: false },
      },
      // A straggler delta appends to the authoritative text, uncapped small.
      { type: "tool_output_delta", id: "x1", text: " + late" },
    ]);
    const content = outputOf(store);
    expect(content.text).toBe("authoritative + late");
    expect(content.truncated).not.toBe(true);
  });
});

describe("ChatStore live subagents set (activeAgents)", () => {
  const AGENT = (id: string, over: Record<string, unknown> = {}): Record<string, unknown> => ({
    type: "tool_call",
    id,
    kind: "agent",
    title: `Agent: ${id}`,
    status: "in_progress",
    ...over,
  });

  it("tracks agent rows incrementally: join on launch, leave on completion", () => {
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      AGENT("a1"),
      { type: "tool_call", id: "b1", kind: "execute", title: "make", status: "in_progress" },
      AGENT("a2"),
    ]);
    expect(store.activeAgents.map((a) => a.id)).toEqual(["a1", "a2"]);
    store.apply({
      seq: 5,
      ts: 5,
      ev: { type: "tool_call_update", id: "a1", status: "completed" },
    } as SeqEvent);
    expect(store.activeAgents.map((a) => a.id)).toEqual(["a2"]);
  });

  it("entries are the SAME rows the transcript renders — patches land in both", () => {
    const store = fold([AGENT("a1")]);
    store.apply({
      seq: 2,
      ts: 2,
      ev: AGENT("a1", { title: "Agent: renamed" }),
    } as SeqEvent);
    expect(store.activeAgents).toHaveLength(1);
    expect(store.activeAgents[0].title).toBe("Agent: renamed");
    const row = store.blocks.find((b) => b.kind === "tool" && b.id === "a1");
    expect(store.activeAgents[0]).toBe(row);
  });

  it("turn-end reconciliation drops dangling agents but keeps cross-turn ones", () => {
    const store = fold([
      { type: "turn_started", turn_id: "t1" },
      AGENT("a1"),
      AGENT("c1", { cross_turn: true }),
      { type: "turn_completed", turn_id: "t1", usage: {} },
    ]);
    // a1 was reconciled to completed; the cross-turn collab agent stays live.
    expect(store.activeAgents.map((a) => a.id)).toEqual(["c1"]);
    store.apply({ seq: 5, ts: 5, ev: { type: "exited", status: 0 } } as SeqEvent);
    expect(store.activeAgents).toEqual([]);
  });

  it("replay rebuilds the identical set", () => {
    const events = [
      { type: "turn_started", turn_id: "t1" },
      AGENT("a1"),
      AGENT("a2"),
      { type: "tool_call_update", id: "a2", status: "failed" },
    ];
    expect(fold(events).activeAgents.map((a) => a.id)).toEqual(
      fold(events).activeAgents.map((a) => a.id),
    );
    expect(fold(events).activeAgents.map((a) => a.id)).toEqual(["a1"]);
  });
});
