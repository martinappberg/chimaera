# chimaera-server — the daemon's HTTP/WS surface + business logic

Orientation for coding agents. This crate is the daemon: every HTTP route, every
WebSocket, and the logic behind them. It embeds `web-ui/dist` and serves it.
Parent map: repo-root [AGENTS.md](../../AGENTS.md). Architecture + rationale:
the [architecture guide](../../docs/agent-guides/architecture.md).

This file is a **module map + the seams that bite**. It is not exhaustive — open
the module you need and read its header doc.

## Module map

| Module | What it owns |
|---|---|
| `lib.rs` | Crate wiring only: `mod` tree + re-exports (`AppState`, `lock`, `app`, `run`) + `ServerConfig`. |
| `state.rs` | `AppState` (every shared handle) + `lock()`. |
| `router.rs` | `app()` — the axum route table. |
| `lifecycle.rs` | The daemon `run()` lifecycle (bind/handoff/manifest/serve/graceful-shutdown) + the listener helpers. |
| `ledger.rs` | The session ledger: snapshot/restore for restart handoff + resurrection. Covers **both** surfaces — PTY sessions and chat sessions (via `state.chat`); a chat resurrects through `chat::resurrect_chat`. |
| `api/` | REST, split by resource: `workspaces`/`sessions`/`exec`/`shutdown`/`env` + `mod.rs` (auth+health+re-exports). |
| `exec.rs` | `run_exec` — the transport-neutral "type a command into a live shell, with sentinel policy" helper, shared by `api::exec_session` (REST) and `mcp::run_in_terminal` (so `mcp` doesn't depend on `api`). |
| `persist.rs` | `atomic_write_json` — the shared temp-write + rename dance for the small JSON state stores (view-state/ledger/workspaces/recents/settings). |
| `session_view.rs` | The session-row JSON builders (`session_json`/`sessions_json`), shared by `api/` and `ws.rs` (so `ws` doesn't depend on `api`). |
| `ws.rs` | WebSockets: `/ws/sessions/{id}` (PTY byte pipe; 1 MiB frames chunked to the input queue), **`/ws/chat/{id}`** (structured events; 10 MiB command frames and ~512 KiB replay batches), `/ws/events` (the session-list bus). |
| `chat.rs` | **The chat-mode glue** (see below). |
| `launcher.rs` | argv assembly (`build_agent_command`, `build_chat_command`, `build_agent_resume_command` — the degrade/toggle-to-TUI argv), binary `detect`, login-shell wrapping, per-agent binary resolution. Unit-tested — argv logic lives HERE, not in drivers or `chat.rs`. |
| `agent_state.rs` | The pure state core: `AgentKind`/`AgentState`/`AgentRecord` + the hook→state / title helpers. A leaf (no transport/fs/`AppState`) — this is what lets `chat.rs` depend on it without the old agents↔chat cycle. |
| `agents.rs` | The agent glue over `agent_state`: hook ingest, settings/mcp writers, the transcript watcher. |
| `spawn.rs` | PTY session spawn (the Tier-A TUI path), theme injection. |
| `git/` | Read-only git, split into `resolve`/`parse`/`service`/`worktree`/`http`: porcelain-v2 status, side-by-side diff, worktree orchestration (confined to a managed root), login-shell git resolution gated at >=2.15. |
| `fs.rs` | The filesystem service AND the file previews (markdown, CSV/TSV incl. a gzip tier, streamed ranged raw reads, bounded archive/table parsing, atomic writes, create/rename/delete for the file-manager menus, `/raw` tickets — tickets may name directories for downloads). There is no separate `previews` module — previews live here. |
| `fs_watch.rs` | Bounded `/ws/events`-client-scoped disk monitor: stats mounted files + visible directories, with a slow capped directory-name scan for NFS/Lustre metadata-cache misses. Never recursively watches a workspace. |
| `download.rs` | `GET /download/{ticket}` — browser downloads via the ticket pattern: files stream as attachments, folders stream a zip built on the fly (bounded memory, symlinks never followed). |
| `upload.rs` | `POST /sessions/{id}/upload?name=` — the landing pad for OS-desktop drops + pasted screenshots. STREAMS the body to `uploads_root/<session-id>/` (hidden-tmp-then-rename), capped 32 MB/file + 256 MB/session + 256 files, strict basename sanitize (no traversal), bearer-authed; the per-session dir is pruned on session delete/retire/shutdown/boot-sweep. |
| `update.rs` | The self-update reporter (`GET /update`; test knobs `CHIMAERA_RELEASES_API`/`UPDATE_CURRENT`). |
| `agent_updates.rs` | Agent-CLI release awareness: bounded per-agent latest probes (same official endpoints as `runtimes`' install scripts), the 6h checker, and the `latest_version`/`update_available` fields on `GET /agents` (+ `?check=true` inline probe). |
| `environment.rs` | Environment preludes: the `env-profiles.json` store + `GET/PUT /environment`, per-session prelude materialization (host ⊕ workspace ⊕ launch → `CHIMAERA_PRELUDE`). Injection rides `api::session_env`/`spawn_env_remove` — keep those two lists disjoint (the PTY and chat transports apply env/env_remove in opposite orders). |
| `compute.rs` | Slurm awareness: login-shell detection (cached; `CHIMAERA_SLURM_BINDIR` test knob) + `GET /compute` — the user's queue + partitions via capped/timeout-fenced `squeue`/`sinfo`, 30s single-flight snapshot cache. Never a 500: a failed `squeue` CALL carries the previous jobs forward tagged `degraded` (distinct from an empty queue); everything else degrades to an empty snapshot. Also `agent_context` — the compute-session context a compute-node daemon (`SLURM_JOB_ID` at boot) injects into its claude sessions via the hook response (`agents::ingest`); baked once per daemon lifetime with an absolute walltime end. |
| `compute_jobs.rs` | Mode 2 — chimaera daemons AS Slurm jobs: `POST/GET/DELETE /compute/sessions` (DETACHED-srun launch — setsid/nohup, tmux-grade persistence, works on interactive-only partitions; charset-gated argv; job id via queue adoption; refusals surfaced from the srun log tail; stateless squeue⋈manifest⋈record listing + dismissable "ended" tombstones from orphaned records, scancel + record marking). Launch seeds the job home's `workspaces.json` with the host's whole registry over the shared FS. |
| `workspaces` / `links`+`mcp` / `settings` / `quickopen` / `recents` / `naming` / `view_state` | The rest of the workbench: roots, linked terminals, settings, palette, history, per-window view-state. |

## The status feed (v0.2)

Session rows carry four additive dashboard fields, assembled in
`session_view::session_json` (chat rows — `chat::chat_session_json` — carry
explicit nulls: the chat client derives richer versions from its journal):

- **`stalled`** — PTY liveness vs the hook claim: a live claude TUI whose
  `AgentRecord` says Running but whose PTY has been silent ≥180s. Recomputed
  per snapshot; the `/ws/events` 1s fallback tick is what flips it without
  any event arriving (same mechanism as `output_active`) — no per-session
  timers.
- **`subagents[]` / `now_line`** — from the claude TUI hook ingest
  (`agents::ingest`): `SubagentStart/Stop` identity (`agent_id`/`agent_type`,
  capped at 32, cleared on Stop/exit) and a one-line latest-hook summary
  ("ran Bash" / "edited foo.rs"), replaced per event, cleared on Stop/exit.
- **`usage`** — the statusline heartbeat: the generated `--settings` points
  `statusLine` at a per-session wrapper script that tees claude's statusline
  JSON to `/agent-events/{id}?key=…&event=statusline` (model / context % /
  cost, quantized to whole percent/cents so snapshot dedupe holds) while
  preserving any user statusline command byte-for-byte.

Plus one field that runs the OTHER way — **`background_running`**, a count of
the agent's live backgrounded Bash/workflows. **Chat rows carry it; PTY rows
carry null** (a TUI's Ctrl-B raises no hook, so null means "unknown"). It is
the deliberate exception to "the chat client derives it from its journal":
that only holds for a client attached to *that* session's socket, and the rail
renders every session while attached to none, so warm-store-only truth left an
agent working off-screen looking idle. `ChatManager`'s pump folds the
`BackgroundTasks` level-set (a count, not the set — this rides every
session-list snapshot; anything wanting the rows is on the chat socket) onto
`ChatInfo`, and `chat_session_json` emits it. Cross-turn by nature: it survives
turn ends and is zeroed on `Exited`.

The wire shapes are pinned in `session_view.rs` tests — extend additively.

## The Mastermind (v1)

One privileged chat session per workspace (the dashboard plan §6/§7 —
"read for all, act for one", v1 scopes both new MCP tiers to it). The pieces:

- **Binding** — `Workspace.mastermind: Option<MastermindCfg{session_id, mode}>`
  (`workspaces.rs`), persisted in `workspaces.json` and additive on the
  `GET /workspaces` wire. `mode` is `ask | auto`. Exactly one per workspace.
- **Routes** (`api/workspaces.rs`) — `PUT /workspaces/{id}/mastermind
  {agent, mode, model?, theme?}` **creates the chat session AND binds it in
  one step**, bind-before-spawn (the generated gating must carry the mode
  before the process exists), retiring any previous Mastermind first (its
  identity is pre-removed so the exit path can't push it into Recents — the
  Mastermind is never a roster conversation). Mode changes are a re-PUT:
  neither agent re-reads its gating after spawn. `DELETE` unbinds + kills.
  Claude and codex both qualify; agents without a chat driver are refused
  with an explanation.
- **Wire flag** — additive `"mastermind": true` on the session row (both
  builders: `session_json` / `chat_session_json`; null elsewhere), computed
  per snapshot from the binding. The UI hides flagged rows from the
  roster/rail (the observer, not the observed).
- **MCP tiers** (`mcp.rs`) — the tier is `mastermind_of()` (who you are, not
  a grant), computed per call on the stateless endpoint: firing the
  Mastermind drops the tier on the very next call. Observe:
  `workspace_status` / `read_session` / `list_changed_files` (read-only;
  `read_session` may read agent-TUI screens — reading is safe, typing never
  is). Act: `spawn_agent` / `spawn_terminal` (the normal spawn paths, never
  a mastermind) / `message_agent` / `interrupt_agent` (**chat sessions in
  the same workspace only** — the exec-409 wall: nothing types into a TUI;
  those answers say "propose to the user"). `message_agent` rides the same
  `ChatManager::command` path a `/ws/chat` Send takes, prefixed with a
  `[via the workspace Mastermind — …]` attribution naming the user-appointed
  chain of authority (provenance stamping that reads as sanctioned direction,
  not a suspicious second-hand instruction). Every act call
  logs a `tracing::info!` audit line. Every answer is capped (constants at
  the top of `mcp.rs`); journal tails read under `spawn_blocking`.
- **Harness gating** — one shared read-tool list (`mcp::MASTERMIND_READ_TOOLS`)
  generates both vendors' gates so ask modes can't drift. Claude
  (`agents.rs::write_settings`): ask pre-allows only the read tools in
  `permissions.allow` (acts raise its native permission prompt); auto
  pre-allows `mcp__chimaera`; the role prompt is argv
  (`launcher::MASTERMIND_SYSTEM_PROMPT` via `--append-system-prompt`). Codex:
  the app-server elicits EVERY MCP tool call regardless of approval-mode
  config (live-probed — PROTOCOL.md Pass 19), so the mode rides
  `SpawnSpec.mcp_auto_approve` (chat.rs sets it; the driver answers listed
  tools' elicitations itself, everything else surfaces); the role prompt is
  `-c developer_instructions`.
- **Reactive-only** — the daemon never triggers a Mastermind turn; it speaks
  only when the user (or nothing) does. No event-nudged turns, no
  `ask_mastermind` in v1 (decision 9 in the plan).
- **Lifecycle** — resurrection (`resurrect_chat`) re-resolves the mode from
  the binding; view-switch/rewind respawns resolve `ChatRecipe.mastermind`
  from the binding too. A Mastermind that dies on its own clears its binding
  in `recents::retire` (and skips Recents).

Codex chat sessions (workers) get the per-session chimaera MCP injected at
spawn via `-c mcp_servers.chimaera.url=…` (`launcher::build_codex_chat_command`,
verified codex 0.144.2) — the same key-in-URL endpoint claude's
`--mcp-config` points at.

## The chat-mode seam (`chat.rs`) — the part this doc exists for

`chimaera-agent` owns the drivers, journal, and registry (`state.chat`). This
crate wires them into the daemon. Nothing here re-implements protocol; it
decides **which** driver runs and **what happens around** its lifecycle.

- **`new_manager`** builds the `ChatManager` with hooks that push `ChatSignal`s
  onto a channel; **`spawn_signal_task`** drains them with async `AppState`
  access (the hooks run on the pump and must stay cheap).
- **`apply_chat_event`** folds protocol events into the `AgentRecord` state
  machine. In chat mode the **protocol is authoritative** for the whole
  lifecycle (hooks are unreliable under `-p stream-json`).
- **`spawn_chat_session`** is the one spawn recipe (create, view-switch, rewind,
  and non-destructive branch all route through it). It assembles argv via
  `launcher`, seeds the journal
  from a previous life on resume, and hands a `SpawnSpec` to `state.chat.spawn`.
- **`handle_chat_exit`** degrades a failed handshake to a PTY TUI (marking the
  in-flight degrade in `chat_switching` so attached chat sockets report
  `degraded`, not `exited`, and stamping a `ModeSwitch` in the journal on
  success), keeps a handshake failure with no recipe visible-and-Errored (like
  `ProtocolError`), or retires a clean exit — EXCEPT during a deliberate view
  switch (`chat_switching`), which it leaves intact for the respawn. Startup
  failures are already journaled by the driver harness before this runs.
- **`switch_view` / `rewind_session`** stop the current process and respawn the
  SAME chimaera session id in the other surface / at a fork point. A term→chat
  switch seeds native or previous-Chimaera history **before** appending its
  `ModeSwitch` marker: the journal seeder is create-new/never-clobber, so
  reversing that order produces a marker-only transcript. A resurrected TUI
  may not have emitted a fresh transcript hook yet, so its durable
  `AgentRecord.resumed_from` is the resume-handle fallback.
- **`fork_session`** snapshots a source journal prefix without stopping it and
  creates a distinct session. Same-agent exact boundaries use native history;
  cross-agent and unrepresentable boundaries seed the normalized prefix and
  prime a fresh driver with a bounded portable transcript.

### The state maps you must keep coherent

A chat session's truth is spread across several `AppState` locks. When you touch
the lifecycle, keep them consistent:

| Lock | Holds | Watch out for |
|---|---|---|
| `state.chat` (the `ChatManager`) | the live driver registry | dead `ProtocolError` entries are kept visible — presence ≠ alive. |
| `state.agents` | `AgentRecord` (state, title, files, `custom_title`) | survives a view switch; the identity that both surfaces share. |
| `state.chat_recipes` | respawn recipe per id | must be removed when the session ends or toggles (else it leaks). |
| `state.chat_switching` | ids mid view-switch (or mid auto-degrade) | serialize entry (one switch per id); the exit path keys on it; the degrade inserts "term" around its respawn window. |
| `state.session_workspaces` | id → workspace | resolve the workspace root from here. |

## Invariants / gotchas

- **Auth every new route.** Chat WS uses first-frame token auth
  (`chat_authenticate`), same token as the rest. Don't add an unauthenticated
  endpoint.
- **Kill-then-respawn is not atomic.** Resolve every respawn precondition
  (binary and regenerable runtime settings/MCP files) *before* killing the old
  process — a post-kill
  failure leaves the session in no registry and the watcher retires it. Serialize
  concurrent switches on `chat_switching`.
- **Chat sessions survive disconnect AND a daemon restart.** They are
  daemon-owned (a closed laptop / window doesn't kill them — the WS handler just
  exits on client disconnect), and a daemon restart now resurrects them like
  PTYs: `ledger::snapshot` records the chat (surface + native id + model),
  `ledger::restore` resurrects the resumable ones live under the same id via
  `chat::resurrect_chat` (regenerate settings/mcp, `--resume`/thread, reuse the
  journal) and retires the rest into Recents (`ui=Chat`). The graceful-shutdown
  path must **not** retire chats (that drops their workspace mapping and the
  reconciler would lose them) — the snapshot carries them.
- **`close-all` / `shutdown` must stop chat drivers too** (`kill_all` only
  covers PTYs).
- **Resource discipline is a review criterion.** ~150 MB RSS, no unbounded
  buffers, no blocking fs on the reactor (wrap journal reads/copies in
  `spawn_blocking`). See the repo-root rules.
- Bumping an agent CLI or editing a driver → `just chat-smoke` (in the sibling
  `chimaera-agent`), then note it in the PR.
