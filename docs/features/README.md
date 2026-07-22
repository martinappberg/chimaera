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

A future agent must treat the Intent section as constraints, not suggestions — but read the
*grade* of each one. Intent distinguishes **core bets** (a handful of load-bearing product
decisions — workspace-first, daemon-owns-everything / nothing-dies, never-silently-kill,
server-side terminal state, no-root ssh deployment — that are genuinely don't-change) from
**additions to the core** (git, previews, chat specifics, linked terminals, the native-app
teardown UX, …) which are *deliberate for now but improvable*. Reserve "must not change" for the
core; an addition can change when there's a clear improvement. Don't be too strict about additions.

## The pages

| Page | What it covers |
|---|---|
| [workbench.md](workbench.md) | Workspaces, home screen, the pane/tab/split workbench, drag-and-drop, zoom, focus mode, quick-open, folder picker, layout persistence, keybindings |
| [dashboard.md](dashboard.md) | The workspace dashboard (landing surface): the attention lane with inline permission answering, density-adaptive agent cards with provenance tiers, subagent drop-down, changed-file attribution, recents & git summary |
| [terminals.md](terminals.md) | Persistent daemon-owned terminals, reconnect/resize/resync, clickable path links, clipboard & provenance, live theming, the exec engine, the command journal |
| [agents.md](agents.md) | Launching coding agents (real TUI + structured chat), the launcher, managed install/update, agent detection, the session rail & attention state, rename/kill, recents & resume |
| [chat-mode.md](chat-mode.md) | Structured chat mode (Tier B): the composer, model/effort/mode/thinking/ultracode controls, tool cards, permission & question prompts, rewind, MCP panel, usage, inline artifacts, the seq journal & gap-replay, view-switch |
| [files-and-previews.md](files-and-previews.md) | The file tree with git decorations, file previews (code, markdown, CSV/TSV incl. gzip, PDF, image, sandboxed HTML, binary, Finder), lightweight editing, raw tickets |
| [board.md](board.md) | Board — the `.board.json` visual composition surface: the chimaera-board engine, `board show/new/render/describe/lint`, the render/describe/edit routes, the BoardView pane, the bidirectional gesture loop |
| [drag-drop-and-uploads.md](drag-drop-and-uploads.md) | Drag a file/folder from the tree to reference it in a session, OS-desktop file drops + screenshot paste that stream to the session's owning host (remote-transparent), the size-capped session-scoped upload route, the native-shell drop handler |
| [git.md](git.md) | Source-control panel (status/diff), worktree create/remove, the session-scoped changes view, git-binary remediation |
| [linked-terminals.md](linked-terminals.md) | Granting an agent access to specific terminals (the "leash") and the daemon's MCP server (`list_terminals` / `run_in_terminal` / `read_terminal`) |
| [remote-connect.md](remote-connect.md) | `chimaera connect` — SSH orchestration, daemon auto-deploy, tunnels, in-app SSH/2FA auth, remote host management |
| [native-app.md](native-app.md) | The Tauri shell: real OS windows, window restore, the signed app+daemon self-updater, the update toast |
| [lifecycle-and-persistence.md](lifecycle-and-persistence.md) | "Close the laptop, nothing dies" — daemon-owned sessions, the session ledger + restart handoff, graceful shutdown, update awareness |
| [environment.md](environment.md) | Environment preludes — per-host/workspace/launch startup commands (`module load`, `conda activate`) run once per session before the shell or agent |
| [compute.md](compute.md) | Slurm awareness — daemon-side scheduler detection, the user's queue snapshot, the rail compute chip + popover (hidden off-cluster) |
| [settings.md](settings.md) | The dotted-key `settings.json` model (hand-edit-aware), the settings UI, theme palettes |
| [cli.md](cli.md) | The `chimaera` binary: `serve`, `connect`, `status`, `kill`, `doctor`, `shell-integration` |

## Not in this catalog (on purpose)

Cross-cutting *infrastructure* — not user features — lives with the code, not here:

- **Auth** (bearer / WS first-frame / `/raw` ticket / key-in-URL) → [rules/daemon.md](../../.claude/rules/daemon.md) + [chimaera-server/AGENTS.md](../../crates/chimaera-server/AGENTS.md).
- **The daemon↔UI wire contract** (`SessionInfo`, `SessionEvent`, `ExecOutcome`, chat `SeqEvent`/`AgentCommand`) → the same two places. Feature pages name the routes/WS channels a feature uses; the wire *invariants* are a rule, not a feature.
- **How it's built and why** → the [architecture guide](../agent-guides/architecture.md) and [DESIGN.md](../../DESIGN.md).

## Honest gaps

Some behavior can't be documented from code alone — it needs product intent — and a few
capabilities are half-built. Rather than guess, the pages flag these inline
(**Intent: pending** for the former; **Status: partial** for the latter). Known
half-built items today: Codex chat drops the create-time model; Gemini/Antigravity aren't
first-class agents yet (see [agents.md](agents.md)).

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
