# Plugins, skills & agent notes

One **Plugins** tab for two kinds of add-on, different in who runs them: **workbench plugins**
run in Chimaera (opt-in per workspace, each saying in words what it adds) and **agent plugins**
run inside claude / codex (read from the agents themselves, installed only through their own
plugin managers). Beside them: a **Skills** view of every skill each agent can use here, in-app
**codex hook trust**, and **Agent notes** — agents talking, itself a workbench plugin. Design:
[docs/timeline-knowledge-plugins-plan.md](../timeline-knowledge-plugins-plan.md) §6–§7;
authoring recipe for a new plugin: [docs/agent-guides/plugins.md](../agent-guides/plugins.md).

**Where it lives (shared):** daemon `crates/chimaera-server/src/plugins/` (`mod.rs` — catalog,
detect, routes; `tools.rs` — the MCP tools plugins add; `manifests/*.toml`), `agent_probe.rs`,
`notes.rs`; UI `web-ui/src/lib/plugins/` ([map](../../web-ui/src/lib/plugins/AGENTS.md)) — the
`plugins` singleton tab (`{v:"plugins"}` in `web-ui/src/lib/layout/layout.ts`) and the attach
sheet, hosted once in `web-ui/src/App.svelte`. Wire (all under `/api/v1`, bearer-authed):
`GET /plugins` (the catalog) · `GET /workspaces/{id}/plugins` (per plugin: `on` / `detected` /
`active` + requirements) · `PUT /workspaces/{id}/plugins/{pid} {on}` ·
`POST /workspaces/{id}/plugins/{pid}/install {agent}` · `POST …/{pid}/setup {agent}` ·
`POST …/{pid}/trust-hooks {hooks:[{key,hash}]}` · `GET /workspaces/{id}/agent-plugins?refresh=` ·
`GET /workspaces/{id}/skills?refresh=` · `POST /workspaces/{id}/timeline/{seq}/deliver`; plugin
tools ride the per-session MCP endpoint ([linked-terminals.md](linked-terminals.md#the-mcp-server)).

## Workbench plugins

- **What & when.** Opt-in capabilities that change what the UI or the agents get, off by
  default. Two ship: **Mycelium** (fills Knowledge and "Where things stand"; adds
  `knowledge_search` · `knowledge_get` for every agent here) and **Agent notes** (below).
- **How it's used.** Open Plugins from quick-open ("Plugins"). **Installed** shows a card per
  plugin — name · summary · a switch meaning *on in this workspace* · a **Here** line · an
  **Adds** line ("Would add" while off) · a **Needs** line with per-agent requirement chips
  (Install, "Review & trust →") — then what each agent CLI reports (below). A host chip names
  the host, since installs are per host. Mycelium's **attach sheet** ("Use mycelium for
  Knowledge", also reached from Knowledge's card, the dashboard's "Where things stand", and the
  Mastermind dock's quiet line) runs three live-checked steps: 1 install for your agents (a
  visible terminal running `<agent> plugin marketplace add arjunrajlaboratory/mycelium`, then
  `claude plugin install mycelium@mycelium` or `codex plugin add mycelium@mycelium`) · 2 trust
  codex's hooks (see below) · 3 set up this workspace (the plugin's own prompt, "Set up
  Mycelium in this repository.", sent to a new chat session of the agent the user picks —
  their click, their billing). Completing also switches the plugin on here.
- **Where it lives.** `plugins/mod.rs` (`Manifest`, `catalog`, `active`, `spawn_allow`,
  `workspace_plugins`, `put_workspace_plugin`, `install_requirement`, `setup_workspace`),
  `plugins/tools.rs` (`defs` / `instructions` / `call`), the switch on
  `Workspace.plugins_on` (`workspaces.rs`). UI `store.ts`, `PluginsView.svelte`,
  `InstalledView.svelte`, `AttachSheet.svelte`.
- **Key behaviors.**
  - **Active = switched on here AND the footprint is present** (`detect.any`, workspace-
    relative; no path component may be a symlink; empty = always present, as for Agent notes).
    The switch persists in `workspaces.json` — durable or refused, since a forgotten toggle
    would silently change what agents see. Detection is a few `stat`s off the reactor, cached
    30 s per workspace, re-run on every switch and before any answer that decides what an agent
    sees.
  - **Core never changes what agents see; a plugin changes it only where active.** With none
    active, MCP `tools/list`, the `initialize` instructions, generated claude settings and codex
    argv are byte-identical — pinned by `crates/chimaera-server/src/tests/agent_view.rs`. Where
    active, a plugin's tools join `tools/list`, pass the call gate (elsewhere a call is refused
    with "switch it on" text), and its instruction paragraph joins `initialize`.
  - **Pre-allowed at spawn:** claude — `mcp__chimaera__<tool>` in the generated settings'
    `permissions.allow` (chat and TUI); codex chat — the driver's `mcp_auto_approve`; codex
    TUI — `-c mcp_servers.chimaera.url=…` + `bearer_token_env_var="CHIMAERA_MCP_KEY"` (the key
    in the PTY env, never argv) + per-tool `approval_mode="approve"` (live: a pre-approved
    tool runs with no prompt, a linked-terminal tool still asks — PROTOCOL.md Pass 33). **A codex TUI gets the chimaera MCP server at all only while a
    plugin with tools is active in its workspace** — with none on, its argv is unchanged.
    Pre-allows are baked at spawn: a claude session or codex chat started before the switch
    sees the tools (tools/list is per call) but its agent asks per call, and a codex TUI
    started before it has no chimaera server until respawned; switching off gates calls at
    once.
  - **Manifests are data**: TOML embedded with `include_str!`, parsed `deny_unknown_fields`; a
    test fails one that doesn't say what it adds. No dynamic loading, no third-party code.
    Built contribution points: `detect`, `requires.agent_plugins`, `setup.prompt`,
    `provides.knowledge`, `provides.mcp_tools`; `views` / `settings` / `commands` are specified
    in the authoring guide but not built.
  - **Installs are the agent's own CLI** in a visible `install <plugin> for <agent>` terminal
    (ids and marketplace sources charset-gated, never flag-shaped); 409 when the agent binary is
    missing; the probe cache is invalidated when that terminal ends. Setup is a fresh chat plus
    one Send (502 if the prompt didn't land — the session still exists). Chimaera never
    auto-installs or auto-updates.
  - **Status: partial.** Browse (a view over the agents' own marketplaces) renders disabled,
    "later". The Plugins tab itself opens only from quick-open (`DashCtx.onOpenPlugins` exists
    but nothing calls it); the attach sheet is a modal reached from the doors above. LaTeX is
    the next planned plugin; third-party (declarative-only) manifests are later.

## Agent plugins & the Skills view

- **What & when.** "What can my agents do here?" — answered by asking each agent CLI, never by
  re-deriving its discovery rules.
- **How it's used.** Installed's agent section lists each CLI's plugins: claude from
  `claude plugin list --json` (id, version, scope, enabled) plus `claude plugin details` totals
  (skills, hooks, always-on tokens — for the first 12); codex from a short-lived
  `codex app-server` (`skills/list`, `hooks/list` for the workspace cwd — no thread, no model
  call), a codex plugin being whatever `pluginId` codex attributes skills and hooks to. The
  **Skills** view lists every skill once per name, grouped by origin — this project
  (`<root>/.claude/skills`; codex `repo` scope) · from plugins (enabled claude plugins'
  `skills/`; codex `pluginId`) · yours (`~/.claude/skills`; codex `user`) · built into the agent
  (claude's catalog from a *live* claude chat session's handshake in this workspace — present
  only while one runs, `_`-prefixed internals dropped). Each row carries one chip per agent —
  available ✓ · off ◌ ("disabled in codex's config") · absent, with the reason in words ("codex
  is not installed here", "the plugin is not installed for claude") — and the invocation in each
  agent's own syntax (`/name` claude, `$name` codex). Codex's skill load errors are listed.
  Filter chips (All · claude · codex · only one agent), search, a detail aside.
- **Where it lives.** `agent_probe.rs` (`claude_state`, `codex_raw` / `codex_state`,
  `CodexRpc`, `agent_plugins`, `skills`, `scan_skills`); UI `SkillsView.svelte`,
  `skillsModel.ts`. Wire facts: [PROTOCOL.md Pass 33](../../crates/chimaera-agent/PROTOCOL.md).
- **Key behaviors.** Every child is login-shell wrapped, time-boxed (20 s CLI, 15 s per RPC),
  output-capped, `kill_on_drop`; **one probe runs daemon-wide at a time** (a semaphore — three
  windows on the tab never spawn three app-servers); answers are cached 60 s; `?refresh=true`,
  an install ending and a trust write invalidate. Claude has no skills-list API, so its side is
  a bounded directory scan (200 skills per dir, 8 KiB of each `SKILL.md`) joined with the live
  catalog.

## Codex hook trust

- **What & when.** Codex runs no plugin hook until the user trusts it — a real security
  boundary. Chimaera removes the trip to codex's `/hooks`, never the decision.
- **How it's used.** The attach sheet's step 2 lists each of the plugin's codex hooks in plain
  words — event, matcher, what it runs (the script from the plugin's own `hooks.json`; a
  dispatcher is named by its handler), with a warning on mycelium's Stop hook ("can keep a turn
  going until .living/ is updated"). Trusting posts exactly the `{key, hash}` pairs shown.
- **Where it lives.** `agent_probe::trust_hooks` → `POST /workspaces/{id}/plugins/{pid}/trust-hooks`.
- **Key behaviors.** Re-lists `hooks/list` first and writes only hooks whose `pluginId` is this
  manifest's codex plugin, whose status is `untrusted` or `modified`, and whose `currentHash`
  still equals the hash the user reviewed — everything else comes back in `skipped` with a
  reason. The write is codex's own record (`hooks.state."<key>".trusted_hash`) through the
  app-server's `config/batchWrite` with `mergeStrategy: "upsert"` (other trust records survive)
  and `reloadUserConfig` — never an edit of `config.toml`. Hash-pinned: a hook that changes on
  upgrade returns as `modified`. Each write logs a `tracing` audit line; nothing is automated.

## Agent notes

- **What & when.** Agents leave short notes for each other and for the Mastermind, on the
  Timeline — heads-ups, questions, "I'm changing the loader API". **Mail, not phone:** posting
  never starts a turn anywhere, so no ping-pong loops, no surprise bills, no chain a poisoned
  note could set off. Talking isn't commanding: every reader gets notes framed as information.
- **How it's used.** Switch "Agent notes" on for the workspace (it has no footprint, so on =
  active). Tools: `post_note {text, to?}` — `to` a session id in this workspace, `"mastermind"`,
  or omitted for everyone — and `read_notes {all?}` — unread notes for you (and for everyone),
  oldest first, quoted, advancing your read cursor. A note reaches its recipient when it
  (a) reads; (b) is a claude session — a one-line "N unread notes … read_notes shows them" hint
  rides the `SessionStart` / `UserPromptSubmit` hook responses claude already fires; (c) the
  user clicks "deliver to <name>" on the Timeline row of a note addressed to one session — a
  real, attributed, quoted message they chose to send; (d) is the Mastermind — a "N new notes
  from agents" chip on the dock, where one click is one turn asking it to `read_notes`
  ([dashboard.md](dashboard.md#the-mastermind-dock)).
- **Coverage.** claude chat / TUI — read, post, hinted, addressable; codex chat — read, post,
  pull-only; codex TUI — read, post, pull-only (it gets the MCP server because this plugin has
  tools); shells — none. Any session in the workspace can be a `to`; **deliver** needs a
  running chat session (a terminal agent gets 409 — "open it and paste"; chimaera never types
  into a TUI).
- **Where it lives.** `notes.rs` (`post`, `read`, `unread_count`, `deliver`), the hint in
  `agents.rs::ingest`, `tools.rs`; the `agent-notes.toml` manifest; UI `TimelineRow.svelte` /
  `TimelineView.svelte` (deliver) and `MastermindDock.svelte` (the inbox chip).
- **Key behaviors.** ≤10 posts per session per minute; a note is ≤2 KiB; notes never cross
  workspaces; a note for everyone has no single recipient to deliver to. Read cursors and rate
  windows live in daemon memory (a restart resets them, so old notes can read as unread
  again); `read_notes` looks through the newest 200 Timeline entries and returns ≤30. The
  dock's inbox keeps its own per-browser cursor (localStorage).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Plugins, Skills, hook trust & Agent notes — why it exists
_Captured 2026-09-25 (from the maintainer, via capture-feature-intent)._

- **Problem it solves:** all four of the maintainer's triggers — the Mastermind was "sometimes superfluous, not really always doing anything" and the dashboard "doesn't say much and feels redundant"; Chimaera should know what is going on in each project — fully mycelium-compatible with good UI on top, yet still working a little without it; agents across vendors (claude ↔ codex, even subagents) should be able to talk to each other; and attaching mycelium should be quick and plugin-like, generic enough for LaTeX and other harnesses later.
- **How settled it is (intended vs provisional):** only the *why* is settled. The manifest shape, contribution points, the Installed/Skills layout, the attach sheet and how notes are delivered are how it works for now.
- **Deliberately open / where it may go:** all left open for later, not ruled out: a Browse/marketplace for skills and plugins; more workbench plugins (LaTeX, built by another agent; the specified-but-unbuilt views/settings/commands contribution points); a Chimaera-side knowledge store (today, without mycelium, Knowledge shows guidance files and Claude memory only); and agents directing each other beyond informational notes (subagents addressable).
- **Do not change (or: open to change):** open to change — an addition to the core, not a core bet. Offered four candidates to freeze (nothing changes for agents unless a plugin is on; Chimaera never curates knowledge; notes never start a turn; hook trust is never silent), the maintainer answered "all can change". They are how it was built today, not locked contracts.

The design's maintainer decisions (2026-09-25) are in the
[plan](../timeline-knowledge-plugins-plan.md#decisions-maintainer-2026-09-25).
