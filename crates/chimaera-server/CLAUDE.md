# chimaera-server — the daemon's HTTP/WS surface + business logic

Orientation for coding agents. This crate is the daemon: every HTTP route, every
WebSocket, and the logic behind them. It embeds `web-ui/dist` and serves it.
Parent map: repo-root [CLAUDE.md](../../CLAUDE.md). Architecture + rationale:
[DESIGN.md](../../DESIGN.md) `## Architecture`.

This file is a **module map + the seams that bite**. It is not exhaustive — open
the module you need and read its header doc.

## Module map

| Module | What it owns |
|---|---|
| `lib.rs` | `AppState` (every shared handle + lock), `app()` route table, the daemon `run()` lifecycle. |
| `ledger.rs` | The session ledger: snapshot/restore for restart handoff + resurrection. (Note: chat sessions are not yet in the snapshot — see the chat-mode seam.) |
| `api.rs` | REST: session create/delete, `session_env`, the agent/model catalog surface, view-switch entry, shutdown. |
| `ws.rs` | WebSockets: `/ws/sessions/{id}` (PTY byte pipe), **`/ws/chat/{id}`** (structured events), `/ws/events` (the session-list bus). |
| `chat.rs` | **The chat-mode glue** (see below). |
| `launcher.rs` | argv assembly (`build_agent_command`, `build_chat_command`), binary `detect`, login-shell wrapping, per-agent binary resolution. Unit-tested — argv logic lives HERE, not in drivers. |
| `agents.rs` | `AgentKind`, `AgentRecord`, `AgentState`, title/file tracking, the agent watcher. |
| `spawn.rs` | PTY session spawn (the Tier-A TUI path), theme injection. |
| `workspaces` / `fs` / `previews` / `links`+`mcp` / `settings` / `quickopen` / `recents` / `naming` / `view_state` | The workbench: roots, file previews, linked terminals, settings, palette, history. |

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
- **`handle_chat_exit`** degrades a failed handshake to a PTY TUI, or retires the
  session — EXCEPT during a deliberate view switch (`chat_switching`), which it
  leaves intact for the respawn.
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
| `state.chat_switching` | ids mid view-switch | serialize entry (one switch per id); the exit path keys on it. |
| `state.session_workspaces` | id → workspace | resolve the workspace root from here. |

## Invariants / gotchas

- **Auth every new route.** Chat WS uses first-frame token auth
  (`chat_authenticate`), same token as the rest. Don't add an unauthenticated
  endpoint.
- **Kill-then-respawn is not atomic.** Resolve every respawn precondition
  (binary, settings files) *before* killing the old process — a post-kill
  failure leaves the session in no registry and the watcher retires it. Serialize
  concurrent switches on `chat_switching`.
- **Chat sessions survive disconnect, die with the daemon.** By design they are
  daemon-owned (a closed laptop doesn't kill them) but they are NOT (yet)
  resurrected across a daemon restart — the journal + native-id index preserve
  the conversation for a manual resume. (Extending the ledger to resurrect them
  is a known follow-up.)
- **`close-all` / `shutdown` must stop chat drivers too** (`kill_all` only
  covers PTYs).
- **Resource discipline is a review criterion.** ~150 MB RSS, no unbounded
  buffers, no blocking fs on the reactor (wrap journal reads/copies in
  `spawn_blocking`). See the repo-root rules.
- Bumping an agent CLI or editing a driver → `just chat-smoke` (in the sibling
  `chimaera-agent`), then note it in the PR.
