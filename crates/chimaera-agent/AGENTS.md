# chimaera-agent — the structured-agent engine

Orientation for coding agents. This crate is the **chat surface's back half**:
it drives coding-agent CLIs through their *structured* protocols (not a PTY) and
turns them into one normalized, seq-numbered, replayable event stream. Read this
before touching the crate; read [`PROTOCOL.md`](PROTOCOL.md) before touching a
driver. The parent map is the repo-root [AGENTS.md](../../AGENTS.md); deep
rationale is in the [architecture guide](../../docs/agent-guides/architecture.md)
(`### Agent integration`).

## What this crate is (and is NOT)

- **IS**: an id-keyed registry of live structured sessions. Each session is a
  driver task (a child process + protocol translation) feeding a *pump* that
  assigns sequence numbers, journals every event, and fans out to attached
  clients. Reconnects replay the gap from the journal.
- **IS NOT**: HTTP, WebSockets, auth, workspaces, or PTYs. Those live in
  `chimaera-server`. This crate speaks `AgentEvent`/`AgentCommand` and knows
  nothing about the daemon around it. Keep it that way.

## The one flow to hold in your head

```
child stdout ─▶ driver (claude.rs / codex.rs / acp.rs) ─▶ AgentEvent ─▶ mpsc(events)
                                                                   │
                            ChatManager pump (lib.rs::absorb) ◀────┘
                                   │ assigns seq, folds ChatInfo
                                   ├─▶ Journal::append  (durable JSONL + ring)
                                   └─▶ broadcast::Sender (live fan-out)
                                            │
   client attach ──▶ ChatManager::attach ──┴─▶ replay (journal) + live (broadcast)
   client command ─▶ ChatManager::command ──▶ mpsc(commands) ─▶ driver ─▶ child stdin
```

The **seq number is the contract**: assigned once in `Journal::append`, so the
journal, the live broadcast, and every client agree. A reconnecting client sends
its `last_seq`; `attach` returns everything after it (replay) plus a live
receiver whose tail may overlap (consumers dedupe by seq). This is the same
gap-replay idea as the PTY transport, realized for structured streams.

## File map

Native UI is a separate transient lane: `ChatManager::native_ui` → driver →
Claude controls → bounded broadcast. It never takes a journal seq. Each
authenticated view owns an attachment; render trees, modules and callback
handles must be discarded on disconnect. See the [Mods feature](../../docs/features/claude-mods.md).

| File | What it owns | Start here when… |
|---|---|---|
| `native_ui.rs` | Bounded Claude Mod controls, per-window response routing, host callback ownership, status snapshots and timeouts. | native Mod panes, render hooks, composer/clipboard callbacks. |
| `lib.rs` | `ChatManager`: the session registry + pump task (`absorb`) + `spawn`/`attach`/`command`/`kill`/`remove`; keeps an internal unused-startup bit (prior journal, Init or admitted Send clears it) so the server can close empty failed launches without losing work; owns the 32 MiB / 64-message retained-Send budget across its channel and the drivers' pending FIFOs (each reservation pairs with its driver echo, which is how `command_as` stamps a daemon-sent message's `UserMessage.origin`; a `SendIfRunning` pairs by its caller-minted key, so one settled with no echo can't lend its origin to the next send). `annotate` journals a daemon-authored event (`AgentMessage`) through the pump's own queue (a weak sender: the channel still closes when the driver drops its). Also folds each session's `Carryover` — what dies with the process (bridge, ultracode, running turn, background work) — for the server's ledger (`carryover()`). | adding session lifecycle, changing fan-out, touching `ChatInfo`, or command admission. |
| `driver.rs` | The `AgentAdapter`/`Mapper` traits, `SpawnSpec` (incl. protocol-side `initial_model`, `initial_ultracode`, `agent_version`, `rollback_turns`, native `fork_at`, quiet `portable_context`, and codex's `developer_note`), `DriverIo`, `DriverExit`, handshake/kill timeouts and pre-Init startup progress; the harness `run_driver` (journals the probed version on `Init` + a non-fatal drift log line when no whole token of it equals `tested_version()`, surfaces startup-failure as a visible event, and drives the `tick`/`drain_pending` mapper hooks). | adding a new agent, changing spawn inputs, exit classification, or the version/startup/teardown harness. |
| `model.rs` | The normalized `AgentEvent` / `AgentCommand` types (ACP-shaped), including bounded slash catalogs, native skill input blocks, the latest-wins `RemoteControl` event + `SetRemoteControl` command, `UserMessage.origin`, and the transcript surfaces (`ToolSummary`, `SubagentFinished`, `TurnTokens`, `ActivityLine`; `BackgroundTask.monitor/ambient`); agent communication's `AgentMessage` (journal-only) and `SendIfRunning` (join the running turn or settle `Dropped`, never open one), the `agent`/`mastermind` origins, `comms_title_suffix` (both drivers name a comms call's target); authoritative command-ingress validation, `Usage`, the delta `Coalescer`, and the size caps (`COMMAND_*`, `cap_output`, `cap_head_tail`, `DIFF_*_BUDGET`, `BG_*`). | adding an event/command kind, or a cap. |
| `claude.rs` | The Claude Code driver: bidirectional `stream-json` + the `control_response` protocol, incl. the Remote Control bridge (`remote_control` control + `system/bridge_state`, `SpawnSpec.remote_control` at-start), the initialize offer flags, the tool-family titles, narration classification (a thinking block's signature says whether it is prose — held briefly until known; `signature_is_narration`), per-batch tool labels, and the subagent/background/monitor lifecycle. Pinned to `TESTED_CLAUDE_VERSION`. | claude protocol work. |
| `acp.rs` | Shared ACP v1 adapter for Grok Build and Google Antigravity: runtime catalogs/capabilities, approval replies, queued turns, cancellation and deferred conversation-copy forks. | ACP protocol work. |
| `elicitation.rs` | Bounded MCP form schema normalization and typed reply validation shared by Claude/Codex. Input requests are distinct from tool approvals; accepted values are never added to resolution events. | MCP forms, URL requests, nested objects, or input bounds. |
| `capabilities.rs` | Normalized session controls and compatibility defaults for legacy Claude/Codex journals. | adding UI capabilities. |
| `codex.rs` | The Codex driver: `codex app-server` JSON-RPC 2.0, thread/turn/steer lifecycle, cwd-scoped `skills/list` + native skill inputs, questions, approvals (incl. the `{permissions, scope}` profile reply) + default auto-review, the clock (`currentTime/read`), a `-32601` refusal (+ one Notice) for every other server request, model/mode settings, Remote Control status relay, reasoning summaries (`turn/start.summary: "auto"` unless configured), collab-stint finishes, and `commandActions` exploration rows. Pinned to `TESTED_CODEX_VERSION`. | codex protocol work. |
| `journal.rs` | Per-session append-only JSONL + bounded replay ring + the native-id→session index (+ each conversation's own model/effort/mode, so a reopen comes back as it was) + the per-agent-kind `AgentPrefsStore` (last model/effort/mode the user picked, `prefs.json`) + dir pruning (`prune_dir`: history oldest-first, never a `keep` id — `ChatManager::prune_journal_dir` adds its live registry; the server adds every agent session — incl. one between processes — and holds the prune until boot restore settles). The gap-replay crown jewel. | anything touching durability, replay, seq numbering, or what a new chat starts with. |
| `transcript.rs` | Bounded offline import of Claude's native transcript into normalized events, reusing the live driver's block helpers so a reopened TUI conversation can seed a chat journal. Blocking reads must run off the reactor. | importing native conversation history. |
| `ndjson.rs` | Line-oriented JSON transport over child stdio (`JsonlChild` and its split halves), with per-line length caps. Shared by the structured drivers. | transport/framing, process spawn. |
| `bin/fake-claude.rs` | A scripted fake that speaks enough of the claude wire to exercise ordinary permission turns plus deterministic `background`, `question`, `plan`, `subagent`, `showcase` (every 2.1.281 transcript surface in one turn — narration, labels, a finished subagent, a background command + Monitor and their closes, a monitor-woken turn; `FAKE_SHOWCASE_PAUSE_MS`/`_SETTLE_MS` slow it for a live UI check), hang, and failure modes. | writing a hermetic driver/registry or live-UI test. |
| `tests/manager.rs` | Hermetic end-to-end tests via `fake-claude` (no network, no billing). | regression-proofing a change. |
| `tests/live.rs` | The `just chat-smoke` suite against the real Claude/Codex CLIs. Ignored by ordinary `cargo test`; live runs need auth/network and bill turns. | verifying native protocol facts. |
| `tests/acp_live.rs` | `just chat-smoke-acp`, against official Grok/Antigravity executables named by `CHIMAERA_TEST_GROK` / `CHIMAERA_TEST_AGY_ACP`; ignored by ordinary tests and bills turns. | verifying ACP protocol facts. |

## Invariants (breaking these is a review failure)

Model selection and custom model input are separate capabilities:
`custom_model` defaults false on older snapshots and unknown adapters; only
the Claude/Codex adapters grant it. ACP remains catalog-only. Codex model
picks wait for `thread/settings/update` acknowledgement before becoming
`ModelSwitched` picks; older runtimes fall back only for advertised models.
Effort carried to an explicit model must be supported by its current catalog,
including at startup. An unlisted model keeps its native controls.

1. **Bounded allocations, always.** The daemon must stay light on shared
   HPC hosts (target ~150 MB RSS), including compute allocations and an
   explicitly allowed login-node deployment. Every channel is bounded; the journal ring and file are
   capped; per-line reads are capped in `ndjson.rs`; oversized events are
   *replaced*, not stored. Event caps live **at event construction** (`model.rs`)
   so a giant tool input never reaches the journal, the ring, or a client;
   every `AgentCommand` is validated before enqueue so WS and programmatic
   callers share the same allocation budgets. `ChatManager` then reserves every
   Send until its `UserMessage`/`UserMessageUpdate` says the driver consumed or
   dropped it, bounding repeated individually-valid commands too.
2. **Never block the async pump.** `Journal::append` is `async` and yields under
   backpressure; the writer thread does the blocking fs. Never hold the `info`
   mutex across an `.await`, and never do blocking fs on the pump's worker
   (spawn_blocking it — see `absorb`'s index write).
3. **The seq is monotonic and gap-free per session.** Don't reset it, don't skip
   it, don't reorder it. `open` repairs a crash-torn tail rather than reusing a
   seq; `attach` clamps a client whose `last_seq` is ahead of the journal head
   (stale → replay from 0). If you change `SeqEvent`'s serialization, keep `seq`
   the first key (the write-path scan and a `debug_assert` depend on it).
4. **Wire formats are pinned, not trusted.** Claude/Codex's wires are
   unversioned; ACP negotiates protocol v1, but CLI behavior can still drift.
   Every driver pins a tested CLI version (`TESTED_*_VERSION`). A native-driver
   or CLI change requires **`just chat-smoke`**; ACP changes also require
   **`just chat-smoke-acp`** against the affected real agents. Live runs bill
   turns; hermetic tests cannot catch upstream drift. Record new wire facts in
   [`PROTOCOL.md`](PROTOCOL.md) the moment you learn them.
5. **Drivers share the same normalized contract.** They implement the same
   trait and model. For caps, turn-end resets and unhandled-frame cases, audit
   the other adapters too. Provider capabilities may differ; never manufacture
   controls a protocol does not support.

## Adding a new agent (the happy path)

1. Implement `AgentAdapter` in a new module; spawn a `JsonlChild`, translate the
   native protocol into `AgentEvent`s, consume `AgentCommand`s, classify exit as
   a `DriverExit`. Reuse `ndjson.rs` for framing — do not re-roll a line reader.
2. Emit only normalized `model.rs` events. If you need a new event kind, add it
   there (stable serde tags) so every surface gets it for free.
3. Implement `tested_version()` + `kind()` (no default impl) and pin the tested
   CLI version; the harness journals the launcher-probed version on `Init` and
   warns (never blocks) on drift from your pin — see PROTOCOL.md "Version
   detection". Override `tick`/`drain_pending` if your driver has time-driven
   work or asks/queued sends whose reply route dies with the process (otherwise
   they no-op). Add a `tests/live.rs` case behind the same env gate; extend
   `PROTOCOL.md`.
4. The server (`chimaera-server`) decides *which* adapter to spawn — this crate
   just runs the one it is handed.

## Common gotchas

- Hooks are unreliable under structured mode (claude `UserPromptSubmit` never
  fires for stdin `user` messages; `Stop` misses). The **protocol is
  authoritative** for the session lifecycle in chat mode — derive state from
  events, not hooks.
- Background work is cross-turn but process-owned. Every successful driver
  spawn journals an empty `BackgroundTasks` level-set before its first event;
  this is the crash/restart boundary that keeps a reused journal from reviving
  tasks that died with the previous daemon process.
- Take a resumed claude session's native id from `system/init`: older CLIs
  forked a NEW id on `--resume`, 2.1.283 keeps it (PROTOCOL.md Pass 33). Never
  pin `--session-id` with `--resume`. Codex resumes in-protocol (the id survives).
- Every stop we initiate SIGTERMs the child before closing stdin
  (`ChildGuard::terminate`; then the grace, then SIGKILL), and a daemon stop
  runs it for every live driver (the server's `stop_all_for_exit`) instead of
  the runtime's drop-time SIGKILL. Claude's Bash shells and Monitors are
  detached, so only its SIGTERM handler ends them; a bare stdin close leaves
  the shell running and wakes a turn when the Monitor stops (PROTOCOL.md
  Pass 32). What the process held is `Carryover`; the server snapshots it
  BEFORE the stop, because the teardown journals the bridge off and an empty
  background set.
- Remote Control is process-owned on both wires: claude's bridge dies with the
  driver (teardown journals the Off), codex's lives on its app-server DAEMON
  (a per-session app-server only reports `disabled`; no enable RPC exists).
  The `RemoteControl` event is latest-wins and a fresh `Init` resets it.
- Portable branch context is spawn initialization, never a synthetic `Send`:
  Claude reads the runtime prompt file from argv; Codex carries top-level
  `developerInstructions` on thread open. Opening a branch must stay idle. ACP has no system-context field: it holds the copy until the first real user prompt; the server recovers an undelivered copy from the fork marker after restart.
- `DriverExit::ProtocolError` sessions are kept in the registry *dead* so the UI
  can show the failure — remove them deliberately, don't assume `contains(id)`
  means alive.
