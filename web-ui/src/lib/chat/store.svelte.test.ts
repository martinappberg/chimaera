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
    expect(store.blocks).toHaveLength(2_000);
    expect(store.blocks[0]).toMatchObject({ kind: "notice", text: "earlier history trimmed" });
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
