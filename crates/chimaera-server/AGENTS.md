# chimaera-server — the daemon's HTTP/WS surface + business logic

Orientation for coding agents. This crate is the daemon: every HTTP route, every WebSocket, and the logic behind them. It embeds `web-ui/dist` and serves it. Not here: chat drivers and journal (`chimaera-agent`), PTYs (`chimaera-pty`), SSH (`chimaera-remote`). Parent map: [AGENTS.md](../../AGENTS.md). Design: [architecture guide](../../docs/agent-guides/architecture.md). Rules: [daemon.md](../../.claude/rules/daemon.md). Detail: [REFERENCE.md](REFERENCE.md), a section per module, then each module's header doc.

## Module map

| Module | What it owns |
|---|---|
| `lib.rs`, `state.rs`, `router.rs`, `assets.rs` | Wiring, `AppState` + `lock()`, the route table, embedded UI files. |
| `lifecycle.rs`, `job_host.rs`, `ledger.rs` | Daemon `run()`, the cluster lease, the job's host process, restart snapshot/restore. |
| `api/`, `exec.rs` | REST by resource; `run_exec`, the shared type-a-command helper. |
| `persist.rs`, `session_view.rs` | Atomic small-JSON writes; the session-row JSON builders (`pty_row` / `switching_row` / `finish_row`; their tests write `session-rows.json`, [reference](REFERENCE.md#session_viewrs)). |
| `ws.rs`, `ws/frames.rs` | PTY, chat, events sockets; **every daemon→UI frame is built in `frames.rs`**, whose tests write the wire fixtures ([reference](REFERENCE.md#wsframesrs)); `native_ui` frames for Claude Mods. |
| `chat.rs` | Chat-mode glue: which driver runs, what surrounds its lifecycle. |
| `claude_store.rs`, `goal_outcome.rs` | Where a Claude transcript lives; how a `/goal` ended. |
| `operation.rs`, `agent_setup.rs`, `agent_auth.rs` | Per-agent bounded jobs: installs, sign-in. |
| `launcher.rs` | argv assembly, binary detection and resolution, role prompts. |
| `agent_state.rs`, `agents.rs`, `spawn.rs` | Pure agent state; hook ingest and writers (the per-session Claude `--settings` file also carries `autoCompactEnabled`: the reopened conversation's pick, else `agents.autoCompact` On/Off, and `env.CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` from `agents.autoCompactAt`, via `chat::claude_compaction` → `agents::Compaction`; `refresh_settings_compaction` brings a reused file up to date); PTY spawn (a Codex TUI gets the threshold as `-c model_post_turn_compact_threshold_percent`, `launcher::push_codex_compact_at`). |
| `git/` | Read-only git: repositories, session tracker, diffs, history. |
| `fs.rs`, `embed.rs`, `notebook.rs` | Filesystem service and previews; link targets; notebook pages. |
| `fs_watch.rs`, `doc_check.rs`, `drafts.rs`, `agent_docs.rs` | Disk monitor; dialect checker; draft mirror; "teach agents" docs. |
| `proxy.rs`, `browser_open.rs` | Browser-pane reverse proxy; the agent `open_browser` tool. |
| `download.rs`, `upload.rs`, `voice/` | Ticketed downloads; streamed uploads; dictation relay. |
| `notices.rs`, `update.rs` | The notice feed; the self-update reporter. |
| `runtimes.rs`, `runtime_retention.rs`, `agent_updates.rs` | Managed agent runtimes, their cleanup, release awareness. |
| `environment.rs`, `compute.rs` | Environment preludes; Slurm awareness and `/compute`. |
| `timeline.rs`, `episodes.rs`, `history/` | The Timeline, what writes it, per-session history. |
| `subagents.rs`, `recents_archive.rs`, `knowledge.rs` | A subagent's transcript; archived Recents; the Knowledge snapshot. |
| `plugins/` | The plugin host: lock, catalog, trust, runtime, platform, jobs. |
| `agent_probe.rs` + `agent_probe/` | Ask agents: plugins, skills, connections, extensions, usage. |
| `comms.rs`, `mcp.rs` | Agent communication; the per-session MCP endpoint and tiers. Chat instructions require a final task-tool update before replying; no user refresh action or inferred completion. |
| `workspaces`, `links`, `settings`, `quickopen`, `recents`, `naming`, `view_state` | Roots, linked terminals, settings, palette, Recents, view-state. |

Cross-module topics: [status feed](REFERENCE.md#the-status-feed-v02), [Mastermind](REFERENCE.md#the-mastermind--the-coordinator-inside-agent-communication), [Codex MCP injection](REFERENCE.md#codex-mcp-injection), [chat-mode seam](REFERENCE.md#chatrs--the-chat-mode-seam), [agent integration](REFERENCE.md#agent-integration-and-view-persistence).

## State owners

A chat session's truth spans several `AppState` locks; keep them coherent ([→](REFERENCE.md#state-maps-you-must-keep-coherent)).

| Lock | Holds |
|---|---|
| `state.chat` | Live driver registry; dead entries stay visible, so presence ≠ alive. |
| `state.agents` | `AgentRecord`: the identity both surfaces share. |
| `state.chat_recipes` | Respawn recipe per id; removed when the session ends, except a PTY successor keeps it one round trip. |
| `state.chat_switching` | Ids mid view-switch or startup cleanup; one switch per id; the exit path keys on it. |
| `state.session_workspaces` | Session id to workspace. |
| `state.worktree_setups` | `StartGate` of a chat opened before its worktree exists; view switch, rewind and fork answer 409 until the checkout ends. |

## Invariants / gotchas

- **Extension reports whitelist fields; never turn an unknown slash command into a discovery prompt** ([→](REFERENCE.md#native-extension-adapters)).
- **Connector reports carry names and statuses only; sign-in is a bounded in-memory job, never a terminal session** ([→](REFERENCE.md#connector-inventory)).
- **Agent-plugin installs pass metadata as arguments, never spliced into shell source; the exit trap, not the terminal title, reports the outcome** ([→](REFERENCE.md#agent-plugin-installs)).
- **An agent-plugin change clears the probe cache and advances the `agent_plugins` epoch; an explicit refresh never broadcasts** ([→](REFERENCE.md#agent-plugin-change-pushes)).
- **Auth every new route** ([→](REFERENCE.md#auth-every-new-route)).
- **Kill-then-respawn is not atomic** ([→](REFERENCE.md#kill-then-respawn)).
- **Chat sessions survive disconnect AND a daemon restart** ([→](REFERENCE.md#chat-sessions-survive-disconnect-and-restart)).
- **`close-all` / `shutdown` must stop chat drivers too** ([→](REFERENCE.md#close-all-and-shutdown)).
- **~150 MB RSS, no unbounded buffers, no blocking fs on the reactor (`spawn_blocking`)** ([→](REFERENCE.md#resource-discipline)).
- **In chat mode the protocol is authoritative for the whole lifecycle** ([→](REFERENCE.md#chatrs--the-chat-mode-seam)).

## Checks

```sh
just check
just chat-smoke       # CLI bump or driver edit (billed); -acp for ACP
```

Note smoke runs in the PR ([gate](REFERENCE.md#chat-smoke-gate)). Verify runtime changes live (**verify-app**).

## Neighbours

[chimaera-agent](../chimaera-agent/AGENTS.md) + [PROTOCOL.md](../chimaera-agent/PROTOCOL.md) · [chimaera-remote](../chimaera-remote/AGENTS.md) · [chimaera-pty](../chimaera-pty/AGENTS.md) · [chimaera-core](../chimaera-core/AGENTS.md) · [plugin-api](../chimaera-plugin-api/AGENTS.md) · [web-ui chat](../../web-ui/src/lib/chat/AGENTS.md). Features: [catalog](../../docs/features/README.md), [chat-mode](../../docs/features/chat-mode.md), [plugins](../../docs/features/plugins.md).
