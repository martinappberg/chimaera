# Feature catalog

What Chimaera actually **does**, feature by feature — the reference the rest of the
agent layer lacked. The nested `AGENTS.md` maps tell you how the code is *structured*;
this catalog tells you what a user or an agent can *do* and how each capability is
wired end to end, so you can locate and extend a feature without re-exploring the tree.

> **Deep-doc, not always-on.** Only this index is pointed at from the root
> [AGENTS.md](../../AGENTS.md). Read the one page for the feature you're touching —
> don't front-load the set. Each page is on-demand reference material.

> **Docs drift — verify before you trust.** Same rule as everywhere in this repo:
> confirm a path/route/behavior against the code before relying on it, and fix a page
> you find wrong **in the same change**. The [doc-drift hook](../../.claude/hooks/doc-drift.sh)
> warns (never blocks) when a feature's entry points, listed in [registry.json](registry.json),
> change but its page here didn't.

## How to read a page

Every page separates two kinds of knowledge, and the split is load-bearing:

- **Derived — What / How it's used / Where it lives / Key behaviors.** Facts read from
  the code, the UI, and the daemon's routes. An agent (or the
  [document-feature](../../.claude/skills/document-feature/SKILL.md) skill) may
  regenerate or extend these. If they disagree with the code, the code wins and the
  page is wrong — fix it.
- **Intent — why it exists, what's intentional vs incidental.** Human ground truth,
  captured from the people who built the feature via the
  [capture-feature-intent](../../.claude/skills/capture-feature-intent/SKILL.md) skill.
  **Never auto-generated or inferred from code.** Each page carries an `## Intent`
  section at the end; until a `feat:` in that area records it, entries read *pending*.

Read each Intent entry with its capture date and later addenda: it records what the
maintainer decided then, not a list of shipped capabilities. When an older capture predates
current behavior, preserve it and describe present behavior in the derived section; never
infer a new maintainer decision from code. Treat still-applicable product decisions as
constraints, with the *grade* of each one in mind. Intent distinguishes **core bets** (a handful of load-bearing product
decisions — workspace-first, daemon-owned live sessions, never-silently-kill,
server-side terminal state, no-root ssh deployment — that are genuinely don't-change) from
**additions to the core** (git, previews, chat specifics, linked terminals, the native-app
teardown UX, …) which are *deliberate for now but improvable*. Reserve "must not change" for the
core; an addition can change when there's a clear improvement. Don't be too strict about additions.

## The pages

| Page | What it covers |
|---|---|
| [workbench.md](workbench.md) | Workspaces, home screen, the pane/tab/split workbench, drag-and-drop, zoom, focus mode, quick-open, folder picker, layout persistence, keybindings |
| [dashboard.md](dashboard.md) | The workspace dashboard (landing surface), re-centred on questions: the attention lane with inline permission answering, Since you left, Where things stand, the one-line Now (or density-adaptive agent cards with provenance tiers), and the Mastermind dock with Brief me |
| [session-history.md](session-history.md) | A lasting record per agent session (who started it, files written, tokens and time, transcript pointer, the Mastermind's acts), All sessions from Recents, archiving Recents, what a session changed with or without git, the same-file notice and hook line, the Activity page and the dashboard's activity line |
| [agent-communication.md](agent-communication.md) | Agents seeing and messaging each other (`workspace_agents`, `read_agent`, `message_agent`, `read_messages`), provider-specific chat and terminal delivery, including carriers that do not start a turn, wakes and the Needs-you wake requests, the Mastermind as the coordinator inside it, the on/off switch |
| [timeline-and-knowledge.md](timeline-and-knowledge.md) | The per-workspace Timeline the daemon writes (agent turns, notable commands, ended Slurm jobs, crashes, knowledge changes, messages between agents) and the read-only Knowledge view over what agents recorded (mycelium's `.living/`, guidance files, claude memory) |
| [plugins.md](plugins.md) | The Extensions tab (Plugins · Connections · Skills · Browse): opt-in WASM workbench plugins, permissions and trust, plugin screens and file views, programs and tools, per-workspace setup, install/update/rollback (`chimaera plugin`), configured MCP services and hosted connectors with in-app setup, agent plugins and skills, Codex hook trust; Browse is disabled |
| [terminals.md](terminals.md) | Persistent daemon-owned terminals, reconnect/resize/resync, clickable path links, clipboard & provenance, live theming, the exec engine, the command journal |
| [agents.md](agents.md) | Claude Code, Codex, Antigravity, Grok Build and Cursor CLI (real TUI + structured chat), sign-in from the app, the launcher, managed install/update, agent detection, the session rail & attention state, rename/kill, recents & resume |
| [chat-mode.md](chat-mode.md) | Structured chat for Claude, Codex, Antigravity, Grok and Cursor: composer and queues, provider-specific controls, tool cards, permissions and questions, inline artifacts, journal and gap-replay, restart recovery; Claude/Codex also support view-switch and rewind |
| [loops-and-goals.md](loops-and-goals.md) | Work that resumes an agent on its own: Claude's `/loop` (wakeups, interval jobs) and `/goal`, Codex thread goals — the Ahead card, the folded work strip, woken-turn and goal lines, the rail/dashboard "not finished" state, the held done notice, restart carryover |
| [claude-mods.md](claude-mods.md) | Claude Mod panes, native controls and live render hooks in structured chat; transient UI transport and isolated client modules |
| [files-and-previews.md](files-and-previews.md) | File tree and Finder, code/text and live Markdown editing, CSV/TSV/gzip, spreadsheets, PDF/images, Word/PowerPoint, Parquet, notebooks, media, diagrams and boards, sandboxed HTML, bounded reads and draft/conflict recovery, pointing an agent at a file selection |
| [browser-pane.md](browser-pane.md) | Live web apps (Jupyter, marimo, Streamlit) as panes — the daemon's ticketed reverse proxy (HTTP+WS, remote-transparent, compute-node second hop), terminal URL detection, the iframe pane, agent-opened panes (`open_browser`) |
| [drag-drop-and-uploads.md](drag-drop-and-uploads.md) | Drag a file/folder from the tree to reference it in a session, OS-desktop file drops + screenshot paste that stream to the session's owning host (remote-transparent), the size-capped session-scoped upload route, the native-shell drop handler |
| [git.md](git.md) | Source-control panel (status/diff), several repositories per workspace, history (log, commits, file history, changes on a branch), sessions' branches, worktree create/remove/lock, new agents in managed worktrees, the session-scoped changes view, git-binary remediation |
| [linked-terminals.md](linked-terminals.md) | Granting an agent access to specific terminals (the "leash") and the daemon's MCP server (`list_terminals` / `run_in_terminal` / `read_terminal`; `notify` lives in [notifications.md](notifications.md)) |
| [remote-connect.md](remote-connect.md) | `chimaera connect` — SSH orchestration, daemon auto-deploy, tunnels, in-app SSH/2FA auth, remote host management |
| [notifications.md](notifications.md) | The notice feed (agent finished / awaiting approval / error / agent `notify`), native OS + browser notifications with click-to-session, the approval-only counts and the unread mark |
| [native-app.md](native-app.md) | The Tauri shell: real OS windows, window restore, the signed app+daemon self-updater, the update toast |
| [lifecycle-and-persistence.md](lifecycle-and-persistence.md) | Daemon-owned session lifetime, live reconnect, conversation resume and shell respawn after restart, graceful shutdown, update awareness |
| [environment.md](environment.md) | Environment preludes — per-host/workspace/launch startup commands (`module load`, `conda activate`) run once per session before the shell or agent |
| [compute.md](compute.md) | Clusters (HPC + Slurm) — nothing on the login node; you start Slurm jobs from the app's cluster page or `chimaera compute` and open workspaces inside them (several per job, moving between jobs with their chats); discovery, start sheet, folder picker, continue in a new job, notifications, what agents in a job are told, the passive rail chip |
| [settings.md](settings.md) | The dotted-key `settings.json` model (hand-edit-aware), the settings UI, theme palettes |
| [cli.md](cli.md) | The `chimaera` binary: `serve`, `connect`, `status`, `kill`, `doctor`, `shell-integration`, `compute`, `plugin` |

## Where each feature is wired

<!-- registry:start -->
Generated from [registry.json](registry.json) by `node scripts/check-feature-registry.mjs --write`; do not hand-edit it. Code globs are the entry points whose change usually wants the page updated.

| Feature | Code | Maps | Rules | Checks |
|---|---|---|---|---|
| [Agent communication](agent-communication.md) | `crates/chimaera-server/src/comms.rs`, `web-ui/src/lib/workspace/comms.svelte.ts`, `web-ui/src/lib/chat/agentMessages.ts`, `web-ui/src/lib/chat/AgentMessageCards.svelte`, `web-ui/src/lib/dashboard/WakeRequestCard.svelte` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [chat-ui](../../web-ui/src/lib/chat/AGENTS.md), [dashboard-ui](../../web-ui/src/lib/dashboard/AGENTS.md), [workspace-ui](../../web-ui/src/lib/workspace/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Agents — launch, lifecycle & runtimes](agents.md) | `crates/chimaera-server/src/launcher.rs`, `crates/chimaera-server/src/runtimes.rs`, `crates/chimaera-server/src/doc_check.rs`, `crates/chimaera-server/src/agent_docs.rs`, `crates/chimaera-server/src/agents.rs`, `crates/chimaera-server/src/agent_setup.rs`, `crates/chimaera-server/src/recents.rs`, `web-ui/src/lib/workspace/Launcher.svelte`, `web-ui/src/lib/workspace/launcher.ts`, `web-ui/src/lib/shared/SessionGlyph.svelte` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [workspace-ui](../../web-ui/src/lib/workspace/AGENTS.md), [shared-ui](../../web-ui/src/lib/shared/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Browser pane (reverse-proxied web apps)](browser-pane.md) | `crates/chimaera-server/src/proxy.rs`, `crates/chimaera-server/src/browser_open.rs`, `web-ui/src/lib/browser/**`, `web-ui/src/lib/terminal/urlLinks.ts` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [browser-ui](../../web-ui/src/lib/browser/AGENTS.md), [terminal-ui](../../web-ui/src/lib/terminal/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Structured chat mode (Tier B)](chat-mode.md) | `crates/chimaera-agent/src/**`, `web-ui/src/lib/chat/**`, `web-ui/src/lib/shared/TaskListGlyph.svelte`, `crates/chimaera-server/src/chat.rs`, `web-ui/src/lib/shared/ActivityGlyph.svelte`, `web-ui/src/lib/shared/WorkTray.svelte` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [chimaera-agent](../../crates/chimaera-agent/AGENTS.md), [chat-ui](../../web-ui/src/lib/chat/AGENTS.md), [shared-ui](../../web-ui/src/lib/shared/AGENTS.md) | [agent-protocol](../../.claude/rules/agent-protocol.md), [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `cargo test -p chimaera-agent`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test`, `just chat-smoke` |
| [Claude Mods in structured chat](claude-mods.md) | `crates/chimaera-agent/src/native_ui.rs`, `web-ui/src/lib/chat/[Mm]od*`, `web-ui/src/lib/chat/nativeUi*`, `crates/chimaera-agent/tests/native_ui_live.rs` | [chimaera-agent](../../crates/chimaera-agent/AGENTS.md), [chat-ui](../../web-ui/src/lib/chat/AGENTS.md) | [agent-protocol](../../.claude/rules/agent-protocol.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-agent`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test`, `just chat-smoke`, `just chat-mods-smoke` |
| [CLI — the `chimaera` binary](cli.md) | `crates/chimaera/src/**` | [chimaera-cli](../../crates/chimaera/AGENTS.md) | [daemon](../../.claude/rules/daemon.md) | `cargo test -p chimaera` |
| [Clusters (HPC + Slurm)](compute.md) | `crates/chimaera-server/src/compute.rs`, `crates/chimaera-remote/src/cluster.rs`, `crates/chimaera-server/src/job_host.rs`, `crates/chimaera/src/compute.rs`, `crates/chimaera-core/src/slurm.rs`, `crates/chimaera-core/src/cluster.rs`, `crates/chimaera-app/src/shell/cluster.rs` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [chimaera-app](../../crates/chimaera-app/AGENTS.md), [chimaera-core](../../crates/chimaera-core/AGENTS.md), [chimaera-cli](../../crates/chimaera/AGENTS.md), [chimaera-remote](../../crates/chimaera-remote/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [native-app](../../.claude/rules/native-app.md) | `cargo test -p chimaera-server`, `just app-check`, `cargo test -p chimaera-core`, `cargo test -p chimaera`, `cargo test -p chimaera-remote` |
| [The workspace dashboard](dashboard.md) | `web-ui/src/lib/dashboard/**` | [dashboard-ui](../../web-ui/src/lib/dashboard/AGENTS.md) | [web-ui](../../.claude/rules/web-ui.md) | `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [File dragging, references & desktop uploads](drag-drop-and-uploads.md) | `crates/chimaera-server/src/upload.rs`, `web-ui/src/lib/net/uploads.ts`, `web-ui/src/lib/chat/images.ts`, `web-ui/src/lib/shared/reference.ts` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [chat-ui](../../web-ui/src/lib/chat/AGENTS.md), [net-ui](../../web-ui/src/lib/net/AGENTS.md), [shared-ui](../../web-ui/src/lib/shared/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Environment preludes](environment.md) | `crates/chimaera-server/src/environment.rs`, `web-ui/src/lib/settings/EnvironmentSettings.svelte`, `web-ui/src/lib/settings/environment.ts` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [settings-ui](../../web-ui/src/lib/settings/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Files & previews](files-and-previews.md) | `web-ui/src/lib/previews/**`, `crates/chimaera-server/src/fs.rs`, `crates/chimaera-server/src/notebook.rs`, `web-ui/src/lib/workspace/FileTree.svelte` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [previews-ui](../../web-ui/src/lib/previews/AGENTS.md), [workspace-ui](../../web-ui/src/lib/workspace/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Git & source control](git.md) | `crates/chimaera-server/src/git/**`, `web-ui/src/lib/workspace/Git*`, `web-ui/src/lib/workspace/git*.ts`, `web-ui/src/lib/workspace/SessionChangesView.svelte` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [workspace-ui](../../web-ui/src/lib/workspace/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Lifecycle & persistence](lifecycle-and-persistence.md) | `crates/chimaera-server/src/ledger.rs`, `crates/chimaera-server/src/lifecycle.rs`, `crates/chimaera-server/src/persist.rs`, `crates/chimaera-server/src/update.rs`, `crates/chimaera-server/src/api/shutdown.rs`, `crates/chimaera/src/kill.rs` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [chimaera-cli](../../crates/chimaera/AGENTS.md) | [daemon](../../.claude/rules/daemon.md) | `cargo test -p chimaera-server`, `cargo test -p chimaera` |
| [Linked terminals (the "leash") & the MCP server](linked-terminals.md) | `crates/chimaera-server/src/mcp.rs`, `crates/chimaera-server/src/links.rs`, `web-ui/src/lib/workspace/agentLinks.ts` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [workspace-ui](../../web-ui/src/lib/workspace/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Loops and goals](loops-and-goals.md) | `crates/chimaera-agent/src/autonomy.rs`, `crates/chimaera-agent/src/cron.rs`, `crates/chimaera-server/src/goal_outcome.rs`, `web-ui/src/lib/chat/AheadCard*`, `web-ui/src/lib/chat/[Ww]orkStrip*`, `web-ui/src/lib/shared/CountdownRing.svelte`, `web-ui/src/lib/chat/ahead*` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [chimaera-agent](../../crates/chimaera-agent/AGENTS.md), [chat-ui](../../web-ui/src/lib/chat/AGENTS.md) | [agent-protocol](../../.claude/rules/agent-protocol.md), [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `cargo test -p chimaera-agent`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test`, `just chat-smoke` |
| [Native app (the Tauri shell)](native-app.md) | `scripts/build-macos-release.sh`, `scripts/build-macos-release.test.mjs`, `scripts/notarize-macos-dmg.sh`, `crates/chimaera-app/src/**`, `web-ui/src/lib/net/native.ts`, `web-ui/src/lib/workspace/UpdateToast.svelte`, `web-ui/src/lib/workspace/update.svelte.ts` | [chimaera-app](../../crates/chimaera-app/AGENTS.md), [net-ui](../../web-ui/src/lib/net/AGENTS.md), [workspace-ui](../../web-ui/src/lib/workspace/AGENTS.md) | [native-app](../../.claude/rules/native-app.md), [web-ui](../../.claude/rules/web-ui.md) | `just app-check`, `just app-build-signed`, `node --test scripts/build-macos-release.test.mjs`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Notifications & attention](notifications.md) | `crates/chimaera-server/src/notices.rs`, `crates/chimaera-app/src/shell/notices.rs`, `crates/chimaera-app/src/notify.rs`, `web-ui/src/lib/workspace/notices.ts`, `web-ui/src/lib/settings/NotificationStatus.svelte` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [chimaera-app](../../crates/chimaera-app/AGENTS.md), [settings-ui](../../web-ui/src/lib/settings/AGENTS.md), [workspace-ui](../../web-ui/src/lib/workspace/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [native-app](../../.claude/rules/native-app.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `just app-check`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Plugins & skills](plugins.md) | `crates/chimaera-server/src/plugins/**`, `crates/chimaera-server/src/agent_probe.rs`, `plugins/plugins.lock`, `web-ui/src/lib/plugins/**`, `crates/chimaera-plugin-api/**`, `crates/chimaera/src/plugin.rs` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [chimaera-cli](../../crates/chimaera/AGENTS.md), [chimaera-plugin-api](../../crates/chimaera-plugin-api/AGENTS.md), [plugin-api](../agent-guides/plugins.md), [plugins-dir](../../plugins/AGENTS.md), [plugins-ui](../../web-ui/src/lib/plugins/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `cargo test -p chimaera`, `cargo test -p chimaera-plugin-api`, `bash scripts/build-plugins.sh`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Remote connect (SSH orchestration)](remote-connect.md) | `crates/chimaera-remote/src/**`, `crates/chimaera/src/connect.rs`, `crates/chimaera-app/src/shell/connect.rs`, `crates/chimaera-app/src/askpass.rs`, `web-ui/src/lib/workspace/AskpassModal.svelte`, `web-ui/src/lib/workspace/ReauthOverlay.svelte` | [chimaera-app](../../crates/chimaera-app/AGENTS.md), [chimaera-cli](../../crates/chimaera/AGENTS.md), [chimaera-remote](../../crates/chimaera-remote/AGENTS.md), [workspace-ui](../../web-ui/src/lib/workspace/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [native-app](../../.claude/rules/native-app.md), [web-ui](../../.claude/rules/web-ui.md) | `just app-check`, `cargo test -p chimaera`, `cargo test -p chimaera-remote`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Session history & activity](session-history.md) | `crates/chimaera-server/src/history/**`, `crates/chimaera-server/src/recents_archive.rs`, `web-ui/src/lib/workspace/SessionsView.svelte`, `web-ui/src/lib/workspace/SessionEdits.svelte`, `web-ui/src/lib/workspace/SameFileNotice.svelte`, `web-ui/src/lib/workspace/sameFile.svelte.ts`, `web-ui/src/lib/workspace/history.ts` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [workspace-ui](../../web-ui/src/lib/workspace/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Settings](settings.md) | `web-ui/src/lib/settings/**`, `crates/chimaera-server/src/settings.rs` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [settings-ui](../../web-ui/src/lib/settings/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Terminals](terminals.md) | `crates/chimaera-pty/src/**`, `web-ui/src/lib/terminal/**`, `crates/chimaera-server/src/exec.rs`, `crates/chimaera-server/src/api/exec.rs`, `web-ui/src/lib/net/reconnect.ts` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [chimaera-pty](../../crates/chimaera-pty/AGENTS.md), [net-ui](../../web-ui/src/lib/net/AGENTS.md), [terminal-ui](../../web-ui/src/lib/terminal/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [pty](../../.claude/rules/pty.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `cargo test -p chimaera-pty`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Timeline & Knowledge](timeline-and-knowledge.md) | `crates/chimaera-server/src/timeline.rs`, `crates/chimaera-server/src/episodes.rs`, `crates/chimaera-server/src/knowledge.rs`, `plugins/plugins.lock`, `web-ui/src/lib/knowledge/**`, `web-ui/src/lib/workspace/[Tt]imeline*`, `web-ui/src/lib/workspace/knowledge.ts` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [plugins-dir](../../plugins/AGENTS.md), [knowledge-ui](../../web-ui/src/lib/knowledge/AGENTS.md), [workspace-ui](../../web-ui/src/lib/workspace/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `bash scripts/build-plugins.sh`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
| [Workbench — panes, tabs, workspaces & navigation](workbench.md) | `web-ui/src/lib/layout/**`, `crates/chimaera-server/src/workspaces.rs`, `crates/chimaera-server/src/view_state.rs`, `crates/chimaera-server/src/quickopen.rs`, `web-ui/src/lib/shared/keys.ts` | [chimaera-server](../../crates/chimaera-server/AGENTS.md), [layout-ui](../../web-ui/src/lib/layout/AGENTS.md), [shared-ui](../../web-ui/src/lib/shared/AGENTS.md) | [daemon](../../.claude/rules/daemon.md), [web-ui](../../.claude/rules/web-ui.md) | `cargo test -p chimaera-server`, `npm --prefix web-ui run check`, `npm --prefix web-ui run test` |
<!-- registry:end -->

## Not in this catalog (on purpose)

Cross-cutting *infrastructure* — not user features — lives with the code, not here:

- **Auth** (bearer / WS first-frame / `/raw` ticket / key-in-URL) → [rules/daemon.md](../../.claude/rules/daemon.md) + [chimaera-server/AGENTS.md](../../crates/chimaera-server/AGENTS.md).
- **The daemon↔UI wire contract** (`SessionInfo`, `SessionEvent`, `ExecOutcome`, chat `SeqEvent`/`AgentCommand`) → the same two places. Feature pages name the routes/WS channels a feature uses; the wire *invariants* are a rule, not a feature.
- **How it's built and why** → the [architecture guide](../agent-guides/architecture.md) and [docs/design/README.md](../design/README.md).

## Honest gaps

Some behavior can't be documented from code alone — it needs product intent — and a few
capabilities are half-built. Rather than guess, the pages flag these inline
(**Intent: pending** for the former; **Status: partial** for the latter). Current limits include
Antigravity/Grok's unavailable same-session terminal/chat switch and native rewind, and
third-party harness registration that is still planned ([agents.md](agents.md)); the disabled
Extensions **Browse** segment and Connections' lack of add/remove configuration
([plugins.md](plugins.md)); and Timeline turn records that are unavailable for hook-less TUIs
([timeline-and-knowledge.md](timeline-and-knowledge.md)). Knowledge needs an enabled provider
plugin. Gemini CLI is retired from new launches, while old records remain readable.
Codex's launch model is applied, and saved chats resume after daemon restarts; see
[chat-mode.md](chat-mode.md) and [lifecycle-and-persistence.md](lifecycle-and-persistence.md)
for recovery behavior and limits.

## Keeping this catalog current

This only stays true if updating it is part of shipping, not a separate chore:

1. A `feat:` (new user-facing capability — the same bar as a minor version bump, defined
   once in [`scripts/version-bump.sh`](../../scripts/version-bump.sh)) **must** carry its
   feature-page update. The [ship-pr](../../.claude/skills/ship-pr/SKILL.md) flow enforces
   this; the [document-feature](../../.claude/skills/document-feature/SKILL.md) skill is how
   you do it.
2. For a `feat:`, the [capture-feature-intent](../../.claude/skills/capture-feature-intent/SKILL.md)
   skill runs a short questionnaire with the human and writes the answers into the page's
   Intent section. `fix:` / `refactor:` / `chore:` / `docs:` never trigger it.
3. The [doc-drift hook](../../.claude/hooks/doc-drift.sh) warns (never blocks) when a
   feature's entry points, listed in [registry.json](registry.json), change but its page
   here wasn't touched.
4. A feature's owning paths live in [registry.json](registry.json), and
   `node scripts/check-feature-registry.mjs` fails when a page has no entry or an entry
   points at nothing.
