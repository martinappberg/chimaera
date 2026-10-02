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
> warns (never blocks) when a feature's entry points change but its page here didn't.

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
| [agents.md](agents.md) | Claude Code, Codex, Antigravity and Grok Build (real TUI + structured chat), the launcher, managed install/update, agent detection, the session rail & attention state, rename/kill, recents & resume |
| [chat-mode.md](chat-mode.md) | Structured chat for Claude, Codex, Antigravity and Grok: composer and queues, provider-specific controls, tool cards, permissions and questions, inline artifacts, journal and gap-replay, restart recovery; Claude/Codex also support view-switch and rewind |
| [files-and-previews.md](files-and-previews.md) | File tree and Finder, code/text and live Markdown editing, CSV/TSV/gzip, spreadsheets, PDF/images, Word/PowerPoint, Parquet, notebooks, media, diagrams and boards, sandboxed HTML, bounded reads and draft/conflict recovery, pointing an agent at a file selection |
| [browser-pane.md](browser-pane.md) | Live web apps (Jupyter, marimo, Streamlit) as panes — the daemon's ticketed reverse proxy (HTTP+WS, remote-transparent, compute-node second hop), terminal URL detection, the iframe pane |
| [drag-drop-and-uploads.md](drag-drop-and-uploads.md) | Drag a file/folder from the tree to reference it in a session, OS-desktop file drops + screenshot paste that stream to the session's owning host (remote-transparent), the size-capped session-scoped upload route, the native-shell drop handler |
| [git.md](git.md) | Source-control panel (status/diff), several repositories per workspace, history (log, commits, file history, changes on a branch), sessions' branches, worktree create/remove/lock, the session-scoped changes view, git-binary remediation |
| [linked-terminals.md](linked-terminals.md) | Granting an agent access to specific terminals (the "leash") and the daemon's MCP server (`list_terminals` / `run_in_terminal` / `read_terminal`; `notify` lives in [notifications.md](notifications.md)) |
| [remote-connect.md](remote-connect.md) | `chimaera connect` — SSH orchestration, daemon auto-deploy, tunnels, in-app SSH/2FA auth, remote host management |
| [notifications.md](notifications.md) | The notice feed (agent finished / awaiting approval / error / agent `notify`), native OS + browser notifications with click-to-session, the approval-only counts and the unread mark |
| [native-app.md](native-app.md) | The Tauri shell: real OS windows, window restore, the signed app+daemon self-updater, the update toast |
| [lifecycle-and-persistence.md](lifecycle-and-persistence.md) | Daemon-owned session lifetime, live reconnect, conversation resume and shell respawn after restart, graceful shutdown, update awareness |
| [environment.md](environment.md) | Environment preludes — per-host/workspace/launch startup commands (`module load`, `conda activate`) run once per session before the shell or agent |
| [compute.md](compute.md) | Clusters (HPC + Slurm) — nothing on the login node; you start Slurm jobs from the app's cluster page or `chimaera compute` and open workspaces inside them (several per job, moving between jobs with their chats); discovery, start sheet, folder picker, continue in a new job, notifications, what agents in a job are told, the passive rail chip |
| [settings.md](settings.md) | The dotted-key `settings.json` model (hand-edit-aware), the settings UI, theme palettes |
| [cli.md](cli.md) | The `chimaera` binary: `serve`, `connect`, `status`, `kill`, `doctor`, `shell-integration`, `compute`, `plugin` |

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
   feature's code entry points change but its page here wasn't touched.
