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
| `native_ui.rs` | Bounded Claude Mod controls, per-window response routing, host callback ownership, status snapshots and timeouts. The separate native control channel checks the existing command-budget pause fence while holding its lock through enqueue; detach still releases view ownership. Accepted explicit tree controls invalidate prior completed-turn maintenance evidence under that same budget lock; automatic RPCs do not. A later real protocol completion must establish idle again. | native Mod panes, render hooks, composer/clipboard callbacks. |
| `lib.rs` | `ChatManager`: the session registry + pump task (`absorb`) + `spawn`/`attach`/`command`/`kill`/`remove`; keeps an internal unused-startup bit (prior journal, Init or admitted Send clears it) so the server can close empty failed launches without losing work; owns the 32 MiB / 64-message retained-Send budget across its channel and the drivers' pending FIFOs (each reservation pairs with its driver echo, which is how `command_as` stamps a daemon-sent message's `UserMessage.origin`; a `SendIfRunning` pairs by its caller-minted key, so one settled with no echo can't lend its origin to the next send). `send_from_client` is `command` for a send made under a client-minted id (`model::valid_client_id`): the manager persists dispatch before handing input to the driver; duplicates of active/confirmed IDs queue nothing, withdrawn IDs fail `SendCancelled`, and a missing receipt after process replacement fails `SendUncertain` without automatic replay. The original reservation stays admitted until its bounded durable receipt or withdrawal attempt settles; duplicate reads hold the budget lock while reading the in-memory durable snapshot, so confirmation cannot leave a false uncertainty gap. The manager stamps `UserMessage.client_id` (never a driver), retains queued correlation through `UserMessageUpdate::Sent/Cancelled`, and distinguishes active `Accepted` from durable `Confirmed`. A queued echo alone cannot confirm delivery across replacement. `active_queued_ids` exposes the current driver's bounded queue for ready/replay reconciliation. A matched live Cancelled update durably withdraws its client ID, releasing outstanding capacity without weakening a receipt. A send's echo is always journaled with its ids: `Journal::append` cuts a user message too long for one line instead of replacing it. `annotate` journals a daemon-authored event (`AgentMessage`) through the pump's own queue (a weak sender: the channel still closes when the driver drops its). `has_submitted_input` retains conservative lifetime Send evidence after queue quota is released, for callers proving a conversation never started. `command_as_checked` evaluates a daemon's captured authority after the command-order and actor-channel waits, retaining its returned reservation through enqueue; refusal releases quota and leaves unused-startup evidence intact. `pause_commands` provides a cancellation-safe ingress guard for lifecycle proofs; committed kill keeps ingress closed only for that process. Its captured cleanup handle fences and observes the original child after registry removal, so a missing row cannot release cleanup ownership before reaping. `input_activity` atomically joins queued/delivered-before-start input with carryover for safe pause checks. Also folds each session's `Carryover` — what dies with the process (bridge, ultracode, running turn, background work) — for the server's ledger (`carryover()`). | adding session lifecycle, changing fan-out, touching `ChatInfo`, or command admission. |
| `driver.rs` | The `AgentAdapter`/`Mapper` traits, `SpawnSpec` (incl. protocol-side `initial_model`, `initial_ultracode`, `agent_version`, `rollback_turns`, native `fork_at`, quiet `portable_context`, and codex's `developer_note`), `DriverIo`, `DriverExit`, handshake/kill timeouts and pre-Init startup progress; the harness `run_driver` (journals the probed version on `Init` + a non-fatal drift log line when no whole token of it equals `tested_version()`, surfaces startup-failure as a visible event, and drives the `tick`/`drain_pending` mapper hooks). | adding a new agent, changing spawn inputs, exit classification, or the version/startup/teardown harness. |
| `model.rs` | The normalized `AgentEvent` / `AgentCommand` types (ACP-shaped), including bounded slash catalogs, native skill input blocks, the latest-wins `RemoteControl` event + `SetRemoteControl` command, `UserMessage.origin`, `UserMessage.client_id` (the sending client's id for that send, additive), and the transcript surfaces (`ToolSummary`, `SubagentFinished`, `TurnTokens`, `ActivityLine`, `SubagentInfo` — a subagent's handle, served model and kind, latest-wins per row; `BackgroundTask.monitor/ambient/model`); agent communication's `AgentMessage` (journal-only) and `SendIfRunning` (join the running turn or settle `Dropped`, never open one), the `agent`/`mastermind` origins, `comms_title_suffix` (both drivers name a comms call's target); authoritative command-ingress validation, `Usage`, the delta `Coalescer`, and the size caps (`COMMAND_*`, `cap_output`, `cap_head_tail`, `DIFF_*_BUDGET`, `BG_*`). | adding an event/command kind, or a cap. |
| `claude.rs` | The Claude Code driver: bidirectional `stream-json` + the `control_response` protocol, incl. the Remote Control bridge (`remote_control` control + `system/bridge_state`, `SpawnSpec.remote_control` at-start), the initialize offer flags, the tool-family titles, narration classification (a thinking block's signature says whether it is prose — held briefly until known; `signature_is_narration`), per-batch tool labels, the subagent/background/monitor lifecycle, and `note_subagent` (the served model from the hidden `parent_tool_use_id` frames and `tool_use_result.resolvedModel`). Pinned to `TESTED_CLAUDE_VERSION`. | claude protocol work. |
| `acp.rs` | Shared ACP v1 adapter for Grok Build and Google Antigravity: runtime catalogs/capabilities, approval replies, queued turns, cancellation and deferred conversation-copy forks. Grok advertises no modes, so the adapter offers Normal / Always-approve while Grok lists `/always-approve` — the switch is a `session/prompt` that occupies the session like a turn (sends queue behind it, it waits for a running turn), confirmed only by a zero-token reply; a handshake restore is not a pick (`restoring`). | ACP protocol work. |
| `elicitation.rs` | Bounded MCP form schema normalization and typed reply validation shared by Claude/Codex. Input requests are distinct from tool approvals; accepted values are never added to resolution events. | MCP forms, URL requests, nested objects, or input bounds. |
| `capabilities.rs` | Normalized session controls and compatibility defaults for legacy Claude/Codex journals. | adding UI capabilities. |
| `codex.rs` | The Codex driver: `codex app-server` JSON-RPC 2.0, thread/turn/steer lifecycle, cwd-scoped `skills/list` + native skill inputs, questions, approvals (incl. the `{permissions, scope}` profile reply) + default auto-review, the clock (`currentTime/read`), a `-32601` refusal (+ one Notice) for every other server request, model/mode settings, Remote Control status relay, reasoning summaries (`turn/start.summary: "auto"` unless configured), collab-stint finishes, `commandActions` exploration rows, and the child-thread reads (`thread/read` for a subagent's model at row open; `on_query` → `thread_to_events` replays a child's turns through an offline mapper for the subagent view). Pinned to `TESTED_CODEX_VERSION`. | codex protocol work. |
| `journal.rs` | Per-session append-only JSONL + bounded replay ring + the native-id→session index (+ each conversation's own model/effort/mode, so a reopen comes back as it was) + the per-agent-kind `AgentPrefsStore` (last model/effort/mode the user picked, `prefs.json`) + dir pruning (`prune_dir`: history oldest-first, counts companion-only groups, never a `keep` id or unresolved/damaged delivery evidence — `ChatManager::prune_journal_dir` adds its live registry; the server adds every agent session — incl. one between processes — and holds the prune until boot restore settles). The gap-replay crown jewel. | anything touching durability, replay, seq numbering, or what a new chat starts with. |
| `send_state.rs` | Independent session-bound v1 dispatch/receipt/withdrawal evidence (no prompt content). Durable only for `managed_execution` sessions (`Receipts::Durable`); ordinary chats keep the record in memory, write nothing, and ignore a damaged sidecar with a log line (PROTOCOL Pass 49 scope). Durable form: atomic durable sidecar plus enrollment marker, newest 128 settled IDs and all outstanding IDs up to 64, 32 KiB per sidecar, at most 512 retained stores. A shared owned I/O gate survives canceled callers/process replacement; unresolved or damaged metadata is never silently reset or pruned. Journal helpers export/validate/read-only merge/import this state for bounded transfers. | keyed-send restart/transfer safety, cancellation, or delivery uncertainty. |
| `transcript.rs` | Bounded offline import of Claude's native transcript into normalized events, reusing the live driver's block helpers so a reopened TUI conversation can seed a chat journal; `import_subagent_transcript` reads one subagent's own file (`<session>/subagents/agent-<id>.jsonl`) from a stable 1 MiB-stepped window so a live view can append instead of reloading. Blocking reads must run off the reactor. | importing native conversation history. |
| `subagent.rs` | Reading one subagent's conversation: `SubagentTranscript` (events + `epoch` window stamp + model), `DriverQuery` (an ephemeral read the daemon asks a LIVE driver — `Mapper::on_query`, `ChatManager::subagent_transcript`; never journaled), `valid_agent_id`. | the subagent view's data path. |
| `maintenance_idle.rs` / `managed_process.rs` | Idle proof and owned-child identity for a daemon extension's maintenance parking: exact command/pump ownership, a completed structured turn at a pinned protocol, no pending input/permission/background work, a bounded tail drain and a checked journal sync. Linux leader identity uses a pidfd plus start ticks. Only reachable when an extension asks; a plain daemon never calls it. | maintenance parking, child identity, manual restoration. |
| `ndjson.rs` | Line-oriented JSON transport over child stdio (`JsonlChild` and its split halves), with per-line length caps, cancel-safe line reads, a stderr tail bounded by bytes and line count, and opt-in owned-child execution fencing, including stalled handshakes. Each Unix child gets a fresh owned process group even without managed lease admission: a launcher cannot leave its native subprocess behind when direct-child fallback kills it. Group fallback and canceled Drop signal only the original unreaped leader’s group; ordinary shutdown retains its caller’s grace, managed shutdown its existing two-second group cap. This does not contain descendants that create their own sessions or certify Windows descendant cleanup. The child guard owns and cancels the stderr reader after bounded drainage or canceled shutdown. Spawn strips the startup-only supervisor cleanup marker. Every spawned leader is registered with `reaper` and taken off when its guard drops. Shared by the structured drivers. | transport/framing, process spawn. |
| `reaper.rs` | Agents never outlive their daemon. `install` (daemon boot, before anything resumes) stops the agents a killed predecessor on this host recorded and that still run (each checked against its leader's start time), then arms a `/bin/sh` watcher in its own process group that holds a pipe from the daemon plus the live list (mirrored to the per-host record file). When the pipe closes, however the daemon died, the watcher reads the process tree, SIGTERMs each agent (the CLIs end their own detached work on it) and SIGKILLs every agent's and descendant's group two seconds later. Without `install` registration is a no-op. | a killed daemon's leftovers, agent process cleanup. |
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
   *replaced*, not stored (an oversized user message is cut to fit, head and
   tail kept, so it stays in the transcript with its delivery identities). Event caps live **at event construction** (`model.rs`)
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
- Codex `turn/started` precedes its blocking startup hooks and does not prove the
  opening input reached native history. `hook/started|completed` exposes synchronous
  SessionStart/UserPromptSubmit progress; `userMessage.clientId` confirms delivery.
  An interrupt before that confirmation warns the user to resend the original input.
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

Extension seams (used only when a daemon extension is installed): transfer
imports set `SpawnSpec.fork_head` for an offline native head fork (PROTOCOL
Pass 43), and the manager stamps the `moved`/`home`/`recovered` pick-up origins
(`model::is_pickup_origin`) at the matching Send echo. Managed execution closes
command admission before signaling the owned process group and observes its
exit; ordinary drivers keep their launch mode.

Keyed sends persist dispatch before driver enqueue. A nonqueued echo or a queued
`Sent` update confirms driver delivery; a queued echo alone stays unresolved in
durable state. `Accepted` is a live reservation/driver queue, `Confirmed` is a
durable receipt, `Cancelled` is a durable withdrawal, and `Uncertain` refuses
automatic replay. Receipts are bounded deduplication evidence, never a guarantee
that agent side effects run exactly once. Legacy journal-only transfers retain
only echoed IDs; an absent new metadata member must not clear existing evidence.

Legacy journal evidence folds queued echoes with their exact native-message Sent
or Cancelled updates; queued-only rows seed uncertainty, never Confirmed. A
new transfer's unresolved state therefore cannot be promoted by its own queued
journal row. Generic unresolved cancellation remains forbidden; only a matching
live queued reservation may settle a Cancelled update as withdrawn.
