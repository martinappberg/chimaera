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
| `ws.rs` | WebSockets: `/ws/sessions/{id}` (PTY byte pipe), **`/ws/chat/{id}`** (structured events), `/ws/events` (the session-list bus). |
| `chat.rs` | **The chat-mode glue** (see below). |
| `launcher.rs` | argv assembly (`build_agent_command`, `build_chat_command`, `build_agent_resume_command` — the degrade/toggle-to-TUI argv), binary `detect`, login-shell wrapping, per-agent binary resolution. Unit-tested — argv logic lives HERE, not in drivers or `chat.rs`. |
| `agent_state.rs` | The pure state core: `AgentKind`/`AgentState`/`AgentRecord` + the hook→state / title helpers. A leaf (no transport/fs/`AppState`) — this is what lets `chat.rs` depend on it without the old agents↔chat cycle. |
| `agents.rs` | The agent glue over `agent_state`: hook ingest, settings/mcp writers, the transcript watcher. |
| `spawn.rs` | PTY session spawn (the Tier-A TUI path), theme injection. |
| `git/` | Read-only git, split into `resolve`/`parse`/`service`/`worktree`/`http`: porcelain-v2 status, side-by-side diff, worktree orchestration (confined to a managed root), login-shell git resolution gated at >=2.15. |
| `fs.rs` | The filesystem service AND the file previews (markdown, CSV/TSV incl. a gzip tier, ranged raw reads, atomic writes, create/rename/delete for the file-manager menus, `/raw` tickets — tickets may name directories for downloads). There is no separate `previews` module — previews live here. |
| `download.rs` | `GET /download/{ticket}` — browser downloads via the ticket pattern: files stream as attachments, folders stream a zip built on the fly (bounded memory, symlinks never followed). |
| `upload.rs` | `POST /sessions/{id}/upload?name=` — the landing pad for OS-desktop drops + pasted screenshots. STREAMS the body to `uploads_root/<session-id>/` (hidden-tmp-then-rename), capped 32 MB/file + 256 MB/session + 256 files, strict basename sanitize (no traversal), bearer-authed; the per-session dir is pruned on session delete/retire/shutdown/boot-sweep. |
| `update.rs` | The self-update reporter (`GET /update`; test knobs `CHIMAERA_RELEASES_API`/`UPDATE_CURRENT`). |
| `environment.rs` | Environment preludes: the `env-profiles.json` store + `GET/PUT /environment`, per-session prelude materialization (host ⊕ workspace ⊕ launch → `CHIMAERA_PRELUDE`). Injection rides `api::session_env`/`spawn_env_remove` — keep those two lists disjoint (the PTY and chat transports apply env/env_remove in opposite orders). |
| `compute.rs` | Slurm awareness: login-shell detection (cached; `CHIMAERA_SLURM_BINDIR` test knob) + `GET /compute` — the user's queue + partitions via capped/timeout-fenced `squeue`/`sinfo`, 30s single-flight snapshot cache. Never a 500: a failed `squeue` CALL carries the previous jobs forward tagged `degraded` (distinct from an empty queue); everything else degrades to an empty snapshot. Also `agent_context` — the compute-session context a compute-node daemon (`SLURM_JOB_ID` at boot) injects into its claude sessions via the hook response (`agents::ingest`); baked once per daemon lifetime with an absolute walltime end. |
| `compute_jobs.rs` | Mode 2 — chimaera daemons AS Slurm jobs: `POST/GET/DELETE /compute/sessions` (sbatch render with charset-gated directives, stateless squeue⋈manifest⋈record listing + dismissable "ended" tombstones from orphaned records, scancel + record marking). Launch seeds the job home's `workspaces.json` with the host's whole registry over the shared FS. |
| `workspaces` / `links`+`mcp` / `settings` / `quickopen` / `recents` / `naming` / `view_state` | The rest of the workbench: roots, linked terminals, settings, palette, history, per-window view-state. |

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
- **`spawn_chat_session`** is the one spawn recipe (create, view-switch, rewind
  all route through it). It assembles argv via `launcher`, seeds the journal
  from a previous life on resume, and hands a `SpawnSpec` to `state.chat.spawn`.
- **`handle_chat_exit`** degrades a failed handshake to a PTY TUI (marking the
  in-flight degrade in `chat_switching` so attached chat sockets report
  `degraded`, not `exited`, and stamping a `ModeSwitch` in the journal on
  success), keeps a handshake failure with no recipe visible-and-Errored (like
  `ProtocolError`), or retires a clean exit — EXCEPT during a deliberate view
  switch (`chat_switching`), which it leaves intact for the respawn. Startup
  failures are already journaled by the driver harness before this runs.
- **`switch_view` / `rewind_session`** stop the current process and respawn the
  SAME chimaera session id in the other surface / at a fork point.

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
  (binary, settings files) *before* killing the old process — a post-kill
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
