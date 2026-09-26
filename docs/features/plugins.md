# Plugins, skills & agent notes

One **Plugins** tab for two kinds of add-on, different in who runs them: **workbench plugins**
run in Chimaera (each a WebAssembly component the daemon runs in a sandbox, opt-in per
workspace, saying in words what it adds) and **agent plugins** run inside claude / codex (read
from the agents themselves, installed only through their own plugin managers). Beside them: a
**Skills** view of every skill each agent can use here, in-app **codex hook trust**, and
**Agent notes** — agents talking, itself a workbench plugin. Design: the WASM host, versions and
updates in [docs/plugin-system-plan.md](../plugin-system-plan.md); the seam, the tab and notes
in [docs/timeline-knowledge-plugins-plan.md](../timeline-knowledge-plugins-plan.md) §6–§7.
Writing a plugin: [docs/agent-guides/plugins.md](../agent-guides/plugins.md).

**Where it lives (shared):** daemon `crates/chimaera-server/src/plugins/` — `mod.rs` (the
manifest, the catalog and its gates, detect, the workspace routes), `runtime.rs` (the wasmtime
host), `hostfns.rs` (every host function, bounded), `tools.rs` (plugin MCP tools through the
runtime), `installed.rs` (the installed directory; install, update, rollback, remove),
`releases.rs` (the release checker) — plus `agent_probe.rs` and `notes.rs`; the interface
`crates/chimaera-plugin-api` (the WIT world and its Rust bindings,
[map](../../crates/chimaera-plugin-api/AGENTS.md)); the first-party plugin crates in the
`plugins/` cargo workspace ([map](../../plugins/AGENTS.md)), built by `scripts/build-plugins.sh`
into `plugins/dist`; the CLI `crates/chimaera/src/plugin.rs`; UI `web-ui/src/lib/plugins/`
([map](../../web-ui/src/lib/plugins/AGENTS.md)) — the `plugins` singleton tab (`{v:"plugins"}` in
`web-ui/src/lib/layout/layout.ts`) and the attach sheet, hosted once in `web-ui/src/App.svelte`.
Wire (all under `/api/v1`, bearer-authed): `GET /plugins` (the catalog) ·
`GET /workspaces/{id}/plugins` (the same entries plus `on` / `detected` / `active` +
requirements) · `PUT /workspaces/{id}/plugins/{pid} {on}` ·
`POST /plugins/install {github, version?}` · `POST /plugins/{pid}/update` ·
`POST /plugins/{pid}/rollback` · `POST /plugins/{pid}/check` · `DELETE /plugins/{pid}` ·
`POST /workspaces/{id}/plugins/{pid}/install {agent}` · `POST …/{pid}/setup {agent}` ·
`POST …/{pid}/trust-hooks {hooks:[{key,hash}]}` · `GET /workspaces/{id}/agent-plugins?refresh=` ·
`GET /workspaces/{id}/skills?refresh=` · `POST /workspaces/{id}/timeline/{seq}/deliver`; plugin
tools ride the per-session MCP endpoint ([linked-terminals.md](linked-terminals.md#the-mcp-server));
a plugin's `emit` reaches `/ws/events` as a `{"type":"plugin","plugin":…,"workspace":…}` frame
(no UI reads one yet).

## Workbench plugins

- **What & when.** Opt-in capabilities that change what the UI or the agents get, off by
  default. Two ship, both WASM plugins embedded in the binary: **Mycelium** (fills Knowledge and
  "Where things stand"; adds `knowledge_search` · `knowledge_get` for every agent here) and
  **Agent notes** (below).
- **How it's used.** Open Plugins from the rail's `plugins` row or quick-open ("Plugins").
  **Installed** shows a card per plugin — name · its own version and where it came from
  ([below](#versions-installs--updates)) · summary · a switch meaning *on in this workspace* · a
  **Here** line · an **Adds** line ("Would add" while off) · a **Needs** line with per-agent
  requirement chips (Install, "Review & trust →") — then what each agent CLI reports (below). A
  host chip names the host, since installs are per host. Mycelium's **attach sheet** ("Use
  mycelium for Knowledge", also reached from Knowledge's card, the dashboard's "Where things
  stand", and the Mastermind panel's quiet line) runs three live-checked steps: 1 install for
  your agents (a visible terminal running `<agent> plugin marketplace add
  arjunrajlaboratory/mycelium`, then `claude plugin install mycelium@mycelium` or `codex plugin
  add mycelium@mycelium`) · 2 trust codex's hooks (see below) · 3 set up this workspace (the
  plugin's own prompt, "Set up Mycelium in this repository.", sent to a new chat session of the
  agent the user picks — their click, their billing). Completing also switches the plugin on
  here.
- **Where it lives.** `plugins/mod.rs` (`Manifest`, `Catalog` / `resolve` / `gate`, `active`,
  `spawn_allow`, `manifest_json`, `workspace_plugins`, `put_workspace_plugin`,
  `install_requirement`, `setup_workspace`), `plugins/tools.rs` (`owner` / `offered` / `call`),
  the switch on `Workspace.plugins_on` (`workspaces.rs`). UI `store.ts`, `PluginsView.svelte`,
  `InstalledView.svelte`, `AttachSheet.svelte`. Tests:
  `crates/chimaera-server/src/tests/plugins.rs`.
- **Key behaviors.**
  - **Active = switched on here AND the footprint is present AND the plugin passes its gates**
    (`detect.any`, workspace-relative; no path component may be a symlink; empty = always
    present, as for Agent notes). The switch persists in `workspaces.json` — durable or refused,
    since a forgotten toggle would silently change what agents see — and flipping it drops the
    plugin's instance there and clears a fault. Detection is a few `stat`s off the reactor,
    cached 30 s per workspace, re-run on every switch and before any answer that decides what an
    agent sees.
  - **Core never changes what agents see; a plugin changes it only where active.** With none
    active, MCP `tools/list`, the `initialize` instructions, generated claude settings and codex
    argv are byte-identical — pinned by `crates/chimaera-server/src/tests/agent_view.rs`. Where
    active, a plugin's tools join `tools/list`, pass the call gate (elsewhere a call is refused,
    JSON-RPC -32602, "… isn't switched on in this workspace …"), and its instruction paragraph
    joins `initialize`, in catalog (id) order. A plugin that can't answer (refused, faulted)
    adds nothing.
  - **Pre-allowed at spawn:** claude — `mcp__chimaera__<tool>` in the generated settings'
    `permissions.allow` (chat and TUI); codex chat — the driver's `mcp_auto_approve`; codex
    TUI — `-c mcp_servers.chimaera.url=…` + `bearer_token_env_var="CHIMAERA_MCP_KEY"` (the key
    in the PTY env, never argv) + per-tool `approval_mode="approve"` (live: a pre-approved
    tool runs with no prompt, a linked-terminal tool still asks — PROTOCOL.md Pass 35). **A
    codex TUI gets the chimaera MCP server at all only while a plugin with tools is active in its
    workspace, or a Mastermind is appointed there** — with neither, its argv is unchanged.
    Pre-allows are baked at spawn: a claude session or codex chat started before the switch
    sees the tools (tools/list is per call) but its agent asks per call, and a codex TUI
    started before it has no chimaera server until respawned; switching off gates calls at
    once.
  - **A plugin is a manifest plus a sandboxed component.** The manifest (`plugin.toml`) is TOML
    parsed `deny_unknown_fields`; a test fails a shipped one that doesn't say what it adds. The
    behaviour is the plugin's `plugin.wasm`, never daemon code ([the host](#the-plugin-host)).
    Built manifest points: `detect`, `requires.agent_plugins`, `requires.chimaera`,
    `setup.prompt`, `provides.knowledge`, `provides.mcp_tools`, `provides.events`,
    `[release] github`; `provides.views` parses and rides the wire but nothing renders it, and
    `settings` / `commands` are specified in the plan only.
  - **Agent-plugin installs are the agent's own CLI** in a visible `install <plugin> for
    <agent>` terminal (ids and marketplace sources charset-gated, never flag-shaped); 409 when
    the agent binary is missing; the probe cache is invalidated when that terminal ends. Setup
    is a fresh chat plus one Send (502 if the prompt didn't land — the session still exists).
    Chimaera never installs or updates anything on its own.
  - **Status: partial.** Built and proven live on an isolated daemon (2026-09-26, a stand-in
    agent, nothing billed): the WASM host, both plugins as components, versions, installs,
    updates, Use previous and Remove ([plan status](../plugin-system-plan.md#status-2026-09-26)).
    Later: Browse renders disabled, "later"; adding a third-party plugin is the CLI or the route
    only (no card field for it); `plugins/plugins.lock` (first-party plugins pinned from their
    own repositories); the `switched-on` / `switched-off` events (declarable, never delivered);
    the UI-facing `query` route (the export exists, no route calls it); the `exec` and `watch`
    host imports (WIT 0.2), which the LaTeX and Typst plugins' `build` point waits for
    ([plan](../latex-reports-plan.md#the-plugin-shape)).

## The plugin host

- **What & when.** A workbench plugin is a Rust crate compiled for `wasm32-wasip2` into one
  portable `plugin.wasm` (the same file on a Mac, an x86 login node or an ARM box) beside its
  `plugin.toml`, run by the daemon's host through the pinned WIT world `chimaera:plugin@0.1.0`.
  It exports `tools`, `instructions`, `call-tool`, `knowledge`, `query` and `on-event`, and
  reaches nothing but the host's imports. First-party plugins are embedded from `plugins/dist`
  (rust-embed, like `web-ui/dist`; a debug daemon reads the folder once, when its catalog loads,
  so a rebuilt plugin needs a daemon restart); third-party ones install under
  `~/.chimaera/plugins/<id>/<version>/` ([below](#versions-installs--updates)).
- **Where it lives.** `plugins/runtime.rs` (engine, instances, deadlines, faults; the entry
  points `offer` / `call_tool` / `knowledge` / `on_event`, `hook`, `session_ended`),
  `plugins/hostfns.rs` (the WIT `host` interface), `crates/chimaera-plugin-api/wit/chimaera.wit`
  (the world; the daemon's `bindgen!` reads the same file). Tests:
  `crates/chimaera-server/src/tests/plugin_host.rs`, against `plugins/test-fixture` (embedded
  only by test builds, never shipped).
- **Key behaviors** (every limit is the host's, so no plugin can forget one):
  - **One engine per process** (wasmtime, Cranelift, epoch interruption), built on first use: a
    daemon whose user never switches a plugin on never builds it. Each build compiles once, off
    the reactor, and stays in memory (the last two per plugin, keyed by SHA-256, so a moved
    `current` never runs old code).
  - **One instance per (plugin, workspace)**, created lazily, one call at a time, at most 64
    daemon-wide (least recently used goes), dropped after 10 min idle or when the switch flips.
  - **Per-call deadlines:** 5 s (30 s for `knowledge`), enforced on a 100 ms epoch tick, plus an
    outer timeout for a host call stuck on a slow filesystem. **Memory:** 64 MiB of linear
    memory and a 64 MiB virtual reservation per instance, not wasmtime's 4 GiB (login nodes run
    under `ulimit -v`). **WASI grants nothing:** no files, env, args or network; the guest's
    stderr is kept (4 KiB) only to explain a trap.
  - **A trap costs the instance, never the daemon**; five in a minute mark the plugin *faulted*
    in that workspace (the card says why) until the user switches it off and on. Traps ride Unix
    signal handlers on macOS too (`macos_use_mach_ports(false)`): wasmtime's Mach-port thread
    aborted the whole process when a caught SIGCHLD from an ending PTY shell interrupted it.
  - **Host functions, each bounded** (`hostfns.rs`): workspace-relative `read` / `stat` /
    `list` walked with `O_NOFOLLOW` on every component (`..`, absolute paths and symlinks
    refused), reads ≤ 8 MiB and listings ≤ 4,096 entries, off the reactor behind the filesystem
    semaphore; `state` — 64 KiB per (plugin, workspace), in memory (a restart clears it);
    `sessions`; `timeline-append` of `note` entries only (≤ 2 KiB, from a session's call, under
    the per-session posts-per-minute window `tell_mastermind` shares) and `timeline-recent`;
    `emit` (one JSON object ≤ 16 KiB); `now-ms`; `log` (≤ 64 lines a call). The host serves the
    workspace and session of the call in flight, never the context a guest passes back.
  - **What a plugin hands back is capped too** — a tool result at 256 KiB, the instruction
    paragraph at 8 KiB, a hook line at 1 KiB — and a component whose `tools()` names differ
    from its manifest's `provides.mcp_tools` is refused everywhere (the card's Adds line and the
    call gate come from the manifest).
  - **Events reach only the plugins that declare them** (`provides.events`): a hook never
    instantiates a plugin that ignores hooks.
  - **Measured** (2026-09-26, macOS arm64): the release binary 26.8 → 38.4 MB (Cranelift); a
    release-grade compile of Agent notes (135 KB) 55–82 ms and of Mycelium (305 KB) about
    200 ms, once per daemon lifetime; release daemon RSS 5.8 MB idle, 10.2 MB with a workspace
    and a session, 28.8 MB after the first plugin call (compile + instantiate, 70 ms end to end)
    and 28.9 MB after 50 more; a warm tool call through the MCP endpoint 1–2 ms (P1's
    measurement), under 10 ms in the live proof.

## Versions, installs & updates

- **What & when.** Every plugin carries its own version and the WIT version it targets, and
  the card says which version runs and where it came from. An installed plugin updates, rolls
  back and is removed on its own cadence — always the user's click, always a checksum-verified
  download.
- **How it's used.** The card's first line: name · version · a source chip — "ships with
  chimaera x.y.z" (embedded), "installed", or "installed · x ships with chimaera" when both
  copies exist — a **stale** chip when an installed copy is older than the embedded one, and an
  **Update to x.y.z** chip when a check found a newer release. An installed copy adds a line
  with **Use previous (x)** · **Check now** · **Remove** (a dialog naming the versions it
  deletes); each change reports one outcome line (an update shows the verified `plugin.wasm`
  sha256). A plugin this daemon can't run shows why ("needs chimaera ≥ x (this is y)", "needs a
  newer chimaera: …", "needs a newer plugin: …") and stays off. On the daemon's host:
  `chimaera plugin list` · `add <owner/repo> [--version x]` · `update <id>` · `remove <id>` —
  the same routes, printing the versions and the checksums the daemon verified.
- **Where it lives.** `plugins/mod.rs` (`resolve`, `gate`, `Catalog`, `manifest_json`),
  `plugins/installed.rs` (`scan`, `install_route` / `update_route` / `rollback_route` /
  `remove_route`), `plugins/releases.rs` (`check`, `check_all`, `check_route`; run from
  `update.rs::run_checker`), `crates/chimaera/src/plugin.rs`; UI `InstalledView.svelte`,
  `store.ts` (`changeWorkbenchPlugin`, `daemonVersion`). Wire: each catalog entry gains
  `version`, `api`, `source` (`embedded` | `installed`), `stale`, and — only when set — `path`,
  `embedded_version`, `installed_version`, `previous`, `update` (`{version, url, checked_ms}`)
  and `fault`. Tests: `crates/chimaera-server/src/tests/plugin_updates.rs`, against a fake
  releases server.
- **Key behaviors.**
  - **Layout:** `~/.chimaera/plugins/<id>/<version>/{plugin.toml,plugin.wasm}` (the daemon's
    data dir) behind `current` and `previous` links swapped atomically (a symlink under a fresh
    name, renamed over the old). At most two versions stay on disk; **Use previous** swaps the
    two links, so it is reversible; **Remove** deletes the id's directory (409 for a plugin that
    only ships with chimaera — it updates with chimaera).
  - **Precedence, never silent:** the same id embedded and installed → the higher version loads
    and the card names both; equal → the embedded copy; an older installed copy is flagged
    stale and the embedded one runs.
  - **Gates before anything loads:** `api` must be a WIT version this host serves (`0.1`) and
    `requires.chimaera` must match this daemon (a dev build matches every requirement). A plugin
    failing one stays listed, off, with its reason; its switch refuses on (409), and a switch
    already on is kept for when the gate passes again.
  - **Install and update are one path:** the release's `SHA256SUMS` and `plugin.toml` first;
    the manifest's id, the tag's version and the gates checked; then `plugin.wasm` streamed to a
    temp dir under a 16 MiB cap, verified, renamed into `<version>/`, links swapped. Any failure
    leaves the old version current. One change at a time daemon-wide, an audit log line each;
    after it the catalog reloads and the plugin's instances go, so a running session's next
    `tools/list` carries the new version's tools.
  - **The checker never downloads.** For each installed copy whose manifest names
    `[release] github`, it asks the GitHub releases API once after boot and then daily (riding
    the daemon's own update loop, off with `update.autoCheck`; a dev build skips it unless
    `CHIMAERA_PLUGIN_RELEASES_API` is set) and on **Check now**, reads only the release's
    `plugin.toml`, and offers a version only when it is strictly newer and passes the gates.
    Offers live in memory. Embedded plugins update with chimaera itself.
  - A release is a tag `v<version>` with three assets: `plugin.wasm`, `plugin.toml` (that
    version's manifest) and `SHA256SUMS`. A plugin's state and its per-workspace switch follow
    its id across versions.

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
  `skillsModel.ts`. Wire facts: [PROTOCOL.md Pass 35](../../crates/chimaera-agent/PROTOCOL.md).
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
  Timeline — heads-ups, questions, "I'm changing the loader API". (Reaching the Mastermind
  needs no plugin: every worker in a workspace with one has `tell_mastermind` —
  [dashboard.md](dashboard.md#the-mastermind-panel).) **Mail, not phone:** posting
  never starts a turn anywhere, so no ping-pong loops, no surprise bills, no chain a poisoned
  note could set off. Talking isn't commanding: every reader gets notes framed as information.
- **How it's used.** Switch "Agent notes" on for the workspace (it has no footprint, so on =
  active). Tools: `post_note {text, to?}` — `to` a session id in this workspace, `"mastermind"`,
  or omitted for everyone — and `read_notes {all?}` — unread notes for you (and for everyone),
  oldest first, quoted, advancing your read cursor. A note reaches its recipient when it
  (a) reads; (b) is a claude session — a one-line "N unread notes … read_notes shows them" hint
  rides the `SessionStart` / `UserPromptSubmit` hook responses claude already fires; (c) the
  user clicks "deliver to <name>" on the Timeline row of a note addressed to one session — a
  real, attributed, quoted message they chose to send; (d) is the Mastermind — a "N new messages
  from agents" chip in the Mastermind panel, where one click is one turn that quotes up to 20 of
  them into the prompt ([dashboard.md](dashboard.md#the-mastermind-panel)).
- **Coverage.** claude chat / TUI — read, post, hinted, addressable; codex chat — read, post,
  pull-only; codex TUI — read, post, pull-only (it gets the MCP server because this plugin has
  tools); shells — none. Any session in the workspace can be a `to`; **deliver** needs a
  running chat session (a terminal agent gets 409 — "open it and paste"; chimaera never types
  into a TUI).
- **Where it lives.** The WASM plugin `plugins/agent-notes` — `src/lib.rs` (the exports:
  `post_note`, `read_notes`, the hook line, read cursors in host state), `src/notes.rs` (the
  addressing rule, unread, the texts agents read; unit-tested natively), `plugin.toml`
  (declares the `hook` and `session-ended` events). What stays in core, `notes.rs`: `deliver`,
  `tell_mastermind`, `take_post_slot` (the posts-per-minute window the host's Timeline appends
  share), `append_note`, `age`; the hint reaches claude through `plugins::runtime::hook` from
  `agents.rs::ingest`. UI `TimelineRow.svelte` / `TimelineView.svelte` (deliver) and
  `MastermindDock.svelte` (the inbox chip). Tests: `crates/chimaera-server/src/tests/plugins.rs`
  pins the tool definitions, the paragraph and the texts byte for byte.
- **Key behaviors.** ≤10 posts per session per minute and ≤2 KiB a note (the host's caps, not
  the plugin's); notes never cross workspaces; a note for everyone has no single recipient to
  deliver to. Read cursors (host state, per workspace) and rate windows live in daemon memory
  (a restart resets them, so old notes can read as unread again); switching the plugin off and
  on keeps the cursors, and a session that ends drops its own. `read_notes` looks through the
  newest 200 Timeline entries and returns ≤30. The dock's inbox keeps its own per-browser
  cursor (localStorage).

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

### WASM plugins, versions & updates — why it exists
_Intent for the WASM plugin system: pending capture._
