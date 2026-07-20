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
child stdout ─▶ driver (claude.rs / codex.rs) ─▶ AgentEvent ─▶ mpsc(events)
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

| File | What it owns | Start here when… |
|---|---|---|
| `lib.rs` | `ChatManager`: the session registry + pump task (`absorb`) + `spawn`/`attach`/`command`/`kill`/`remove`; owns the 32 MiB / 64-message retained-Send budget across its channel and both drivers' pending FIFOs. | adding session lifecycle, changing fan-out, touching `ChatInfo`, or command admission. |
| `driver.rs` | The `AgentAdapter`/`Mapper` traits, `SpawnSpec` (incl. protocol-side `initial_model`, `agent_version`, `rollback_turns`, and Codex `fork_at`), `DriverIo`, `DriverExit`, handshake/kill timeouts; the harness `run_driver` (journals the probed version on `Init` + a non-fatal drift Notice vs `tested_version()`, surfaces startup-failure as a visible event, and drives the `tick`/`drain_pending` mapper hooks). | adding a new agent, changing spawn inputs, exit classification, or the version/startup/teardown harness. |
| `model.rs` | The normalized `AgentEvent` / `AgentCommand` types (ACP-shaped), including bounded slash catalogs and native skill input blocks; authoritative command-ingress validation, `Usage`, the delta `Coalescer`, and the size caps (`COMMAND_*`, `cap_output`, `cap_head_tail`, `DIFF_*_BUDGET`, `BG_*`). | adding an event/command kind, or a cap. |
| `claude.rs` | The Claude Code driver: bidirectional `stream-json` + the `control_response` protocol. Pinned to `TESTED_CLAUDE_VERSION`. | claude protocol work. |
| `codex.rs` | The Codex driver: `codex app-server` JSON-RPC 2.0, thread/turn/steer lifecycle, cwd-scoped `skills/list` + native skill inputs, questions, approvals + default auto-review, model/mode settings. Pinned to `TESTED_CODEX_VERSION`. | codex protocol work. |
| `journal.rs` | Per-session append-only JSONL + bounded replay ring + the native-id→session index + dir pruning. The gap-replay crown jewel. | anything touching durability, replay, or seq numbering. |
| `ndjson.rs` | Line-oriented JSON transport over child stdio (`JsonlChild` and its split halves), with per-line length caps. Shared by both drivers. | transport/framing, process spawn. |
| `bin/fake-claude.rs` | A scripted fake that speaks just enough of the claude wire to exercise the pipeline hermetically. | writing a hermetic driver/registry test. |
| `tests/manager.rs` | Hermetic end-to-end tests via `fake-claude` (no network, no billing). | regression-proofing a change. |
| `tests/live.rs` | The `just chat-smoke` suite against the REAL CLIs. **Env-gated so a plain `cargo test` never bills money.** | verifying protocol facts. |

## Invariants (breaking these is a review failure)

1. **Bounded allocations, always.** The daemon runs on shared HPC login nodes
   (target ~150 MB RSS). Every channel is bounded; the journal ring and file are
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
4. **Wire formats are pinned, not trusted.** Both protocols are unversioned.
   Every driver is verified against a pinned CLI version (`TESTED_*_VERSION`).
   Touching a driver — or bumping a CLI — **requires `just chat-smoke`** (live,
   bills a few cents); hermetic tests cannot catch upstream drift. Record new
   wire facts in [`PROTOCOL.md`](PROTOCOL.md) the moment you learn them.
5. **The two drivers must stay symmetric.** They implement the same trait and
   the same normalized model. If one handles a case (a cap, a state reset on
   turn end, an unhandled-frame arm), check the other does too — asymmetries
   have been real bugs.

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
- A `--resume` forks a NEW native session id (claude); never pin `--session-id`
  with `--resume`. Codex resumes in-protocol (the id survives).
- `DriverExit::ProtocolError` sessions are kept in the registry *dead* so the UI
  can show the failure — remove them deliberately, don't assume `contains(id)`
  means alive.
