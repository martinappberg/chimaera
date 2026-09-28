# Plugins, skills & agent notes

One **Extensions** tab, in three segments — **Plugins** · **Skills** · **Browse** — for two kinds
of add-on, different in who runs them: **workbench plugins** run in Chimaera (each a WebAssembly
component the daemon runs in a sandbox, installed per host, opt-in per workspace, saying in words
what it adds) and **agent plugins** run inside claude / codex (read from the agents themselves,
installed only through their own plugin managers). Beside them: the **Skills** segment (every
skill each agent can use here), in-app **codex hook trust**, and **Agent notes** — agents
talking, itself a workbench plugin. **Browse** (searching the marketplaces the agents already
have) renders disabled, "later". Design: the WASM host, versions and updates in
[docs/plugin-system-plan.md](../plugin-system-plan.md); the seam, the tab and notes in
[docs/timeline-knowledge-plugins-plan.md](../timeline-knowledge-plugins-plan.md) §6–§7. Writing a
plugin: [docs/agent-guides/plugins.md](../agent-guides/plugins.md).

**Where it lives (shared):** daemon `crates/chimaera-server/src/plugins/` — `mod.rs` (the
manifest, the embedded lock, the catalog and its gates, detect, the workspace routes),
`runtime.rs` (the wasmtime host), `hostfns.rs` (every host function, bounded), `tools.rs` (plugin
MCP tools through the runtime), `installed.rs` (the installed directory; install, update,
rollback, remove), `releases.rs` (the release checker) — plus `agent_probe.rs` and `notes.rs`;
the interface `crates/chimaera-plugin-api` (the WIT world and its Rust bindings,
[map](../../crates/chimaera-plugin-api/AGENTS.md)); the first-party plugins in their own
repositories, [chimaera-plugin-agent-notes](https://github.com/martinappberg/chimaera-plugin-agent-notes) and [chimaera-plugin-mycelium](https://github.com/martinappberg/chimaera-plugin-mycelium), whose releases
`plugins/plugins.lock` pins — the lock is all the daemon carries of them
([map](../../plugins/AGENTS.md)); the CLI `crates/chimaera/src/plugin.rs`; UI
`web-ui/src/lib/plugins/` ([map](../../web-ui/src/lib/plugins/AGENTS.md)) — the `plugins`
singleton tab (`{v:"plugins"}` in `web-ui/src/lib/layout/layout.ts`; the user sees
**Extensions**, while ids, the wire, the store and the file names stay "plugins") and the attach
sheet, hosted once in `web-ui/src/App.svelte`. Wire (all under `/api/v1`, bearer-authed):
`GET /plugins` (the catalog) · `GET /workspaces/{id}/plugins` (the same entries plus `on` /
`detected` / `active` + `requires` / `recommends`) · `PUT /workspaces/{id}/plugins/{pid} {on}` ·
`POST /plugins/install {github, version?}` or `{path}` · `POST /plugins/{pid}/install` ·
`POST /plugins/{pid}/update` · `POST /plugins/{pid}/rollback` · `POST /plugins/{pid}/check` ·
`DELETE /plugins/{pid}` · `GET /plugins/{pid}/details` · `POST /plugins/preview {github}` ·
`POST /workspaces/{id}/plugins/{pid}/install {agent}` ·
`POST …/{pid}/setup {agent}` · `POST …/{pid}/trust-hooks {hooks:[{key,hash}]}` ·
`GET /workspaces/{id}/agent-plugins?refresh=` · `GET /workspaces/{id}/skills?refresh=` ·
`POST /workspaces/{id}/timeline/{seq}/deliver`; plugin tools ride the per-session MCP endpoint
([linked-terminals.md](linked-terminals.md#the-mcp-server)); a plugin's `emit` reaches
`/ws/events` as a `{"type":"plugin","plugin":…,"workspace":…}` frame (no UI reads one yet).
The additive `{"type":"agent_plugins","epoch":…}` frame invalidates agent reports after
installation or hook trust and on reconnect; it carries no plugin payload.

## Workbench plugins

- **What & when.** Opt-in capabilities that change what the UI or the agents get, off by
  default. Two are first-party, both WASM plugins in their own repositories at the releases
  `plugins/plugins.lock` pins: **Mycelium** (fills Knowledge and "Where things stand"; adds
  `knowledge_search` · `knowledge_get` for every agent here) and **Agent notes** (below). The
  daemon carries neither: each is listed as *available* until the user installs it on this
  host ([below](#versions-installs--updates)).
- **How it's used.** Open Extensions from the rail's **Extensions** row or quick-open
  ("Extensions"; "plugins" and "skills" find it too). Its header stays put across segments —
  "Extensions" and a chip naming the host ("Plugins are installed per host") on the left, the
  segments in the far corner (under the title when the tab is narrow) — and each segment
  scrolls on its own below it, keeping its place when you switch away and back. The **Plugins**
  segment opens with "Plugins add tools for your agents and views for you. Install one, then
  switch it on per workspace." and shows, under **Chimaera plugins** (Chimaera's own first), a
  card per plugin with one primary control. Its head: a two-letter tile · the name (a link to
  the plugin's `homepage`, opened in the system browser) · the check badge · version
  ([below](#versions-installs--updates)) · on the right **Install** for a plugin not on this
  host, else the switch meaning *on in this workspace* with its state in words beside it
  ("active here", "on · not set up here yet", "off") · and a quiet **…** menu (click or
  keyboard) holding everything secondary: Check for updates, Use previous version (x), Set up
  in this workspace…, Open on GitHub, Remove…. Under it: the summary, then the author's
  `description` clamped to two lines with a quiet "more"; a small definition list — **For
  you** and **For agents** (from its `[adds]`) and **Here**, only when it means something
  ("using .living/ in this workspace · 4 findings · 4 decisions", "found .living/ in this
  workspace — switch it on to use it", "not set up in this workspace yet — Set it up"); the
  **Agent-side plugin** box (below); an update callout; a fault callout; one outcome line.
  A plugin not installed yet shows its head and summary, and opens in place — a click anywhere
  on its top, or Enter / Space on its chevron — to that same body, read from its release before
  anything is installed ([below](#versions-installs--updates)). Then a small form, **Install
  from a repository** (with **Preview**), and under **Agent plugins** what each agent CLI
  reports (below). Mycelium's **attach sheet** ("Use Mycelium for Knowledge", reached from
  the card's menu, Knowledge's card and the Mastermind panel's quiet line) runs three
  live-checked steps: 1 install for your agents (a visible terminal running
  `<agent> plugin marketplace add arjunrajlaboratory/mycelium`, then `claude plugin install
  mycelium@mycelium` or `codex plugin add mycelium@mycelium`) · 2 trust codex's hooks (see
  below) · 3 set up this workspace (the plugin's own prompt, "Set up Mycelium in this
  repository.", sent to a new chat session of the agent the user picks — their click, their
  billing). Completing also switches the plugin on here.
- **Where it lives.** `plugins/mod.rs` (`Manifest`, the lock — `Locked` / `lock_entries` —,
  `Catalog` / `resolve` / `gate`, `listing`, `active`, `spawn_allow`, `manifest_json` /
  `available_json`, `workspace_plugins`, `put_workspace_plugin`, `install_requirement`,
  `setup_workspace`), `plugins/tools.rs` (`owner` / `offered` / `call`), the switch on
  `Workspace.plugins_on` (`workspaces.rs`). UI `store.ts`, `PluginsView.svelte`,
  `InstalledView.svelte` (the list, the repository form, the agent plugins), `PluginCard.svelte`
  (one card), `installCopy.ts` (the card's words), `requirementsModel.ts` (the agent-side box),
  `AttachSheet.svelte`. Tests:
  `crates/chimaera-server/src/tests/plugins.rs`.
- **Key behaviors.**
  - Release manifests must name the repository they were fetched from in `[release] github`.
    Installing the same id from a different publisher is refused until the existing copy is
    removed. Manifests over 64 KiB are refused before activation.
  - **Active = installed AND switched on here AND the footprint is present AND the plugin
    passes its gates** (`detect.any`, workspace-relative; no path component may be a symlink;
    empty = always present, as for Agent notes). An available plugin is never active: switching
    it on answers 409 "<name> isn't installed — install it first". The switch persists in
    `workspaces.json` by plugin id — durable or refused, since a forgotten toggle would silently
    change what agents see — so reinstalling a plugin that was on here makes it active again;
    flipping it drops the plugin's instance there and clears a fault. Detection is a few
    `stat`s off the reactor, cached 30 s per workspace, re-run on every switch and before any
    answer that decides what an agent sees.
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
    parsed `deny_unknown_fields`; a test fails a locked release that doesn't say what it adds.
    The behaviour is the plugin's `plugin.wasm`, never daemon code
    ([the host](#the-plugin-host)). Built manifest points: `detect`, `requires.agent_plugins`,
    `recommends.agent_plugins`, `requires.summary` / `recommends.summary` (the author's one
    sentence about the agent-side plugin), `requires.chimaera`, `description` (a few plain
    sentences from the author, shown under the summary), `setup.prompt`, `provides.knowledge`,
    `provides.mcp_tools`, `provides.events`, `[release] github`; `provides.views` parses and
    rides the wire but nothing renders it, and `settings` / `commands` are specified in the plan
    only.
  - **Requires and recommends** name agent-side plugins in
    the same shape, `[requires.agent_plugins.<agent>]` / `[recommends.agent_plugins.<agent>]`
    `{id, marketplace}`, and both feed the agent-plugin install route, the attach sheet's
    step 1 and codex hook trust. On the card each is a small titled box, **Agent-side plugin**
    (**· needed** for a requirement), shown only when an agent that could use it is installed on
    this host (or, for a requirement, to say none is): the author's sentence
    (`requires.summary` / `recommends.summary`, or a plain fallback), a link to the agent
    plugin's page ("mycelium on GitHub", from a marketplace that names `owner/repo`), then one
    line per installed agent with its state in words and at most one action — "claude ·
    installed 0.7.2", "codex · not installed [Install]", "codex · 2 hooks not trusted
    [Review]", "claude · installed, disabled"; "asking claude and codex…" while the agents are
    asked. `requires` is a genuine hard requirement (no plugin has one today; "It needs claude
    or codex, and neither is installed on this host." when none is). `recommends` helps the
    agents the user runs but is never needed — mycelium's `mycelium@mycelium` is the example:
    the Knowledge reader works with no agent plugin at all. Both ride
    `GET /workspaces/{id}/plugins` as lists of `{agent, id, marketplace}`, beside
    `requires_summary` / `recommends_summary`.
  - **Agent-plugin installs are the agent's own CLI** in a visible `install <plugin> for
    <agent>` terminal (ids and marketplace sources charset-gated, never flag-shaped); 409 when
    the agent binary is missing. `plugins/install-agent.sh` receives metadata as arguments,
    never shell source. It inherits the host + workspace **Settings → Environment** commands
    so module loads and PATH setup apply. A failed Claude marketplace fetch explains missing
    Git or lack of `--shallow-submodules` support; an existing cached marketplace can still
    install. Codex's fetcher is independent of this Git requirement.
    Success and failure output stay visible until **Enter** closes the
    terminal; the original exit status is preserved. The probe cache is invalidated when the
    command finishes, even while its result remains open, and visible Extensions tabs
    automatically recheck via `/ws/events`. The card offers **continue setup**
    after an agent install is opened, including one launched from the attach sheet. Returning
    to Extensions during this flow and opening the sheet request a fresh agent report. Once
    detected, the card says **Installed for <agent> — continue setup**. The
    sheet resumes the existing install → hook review → workspace setup steps; installation
    alone does not trust hooks or initialize a repository. Newly started agent sessions load
    the installed plugin. Setup is a fresh chat plus one Send (502 if the prompt didn't land —
    the session still exists).
    Chimaera never installs or updates anything on its own.
  - **Status: partial.** Built and proven live on an isolated daemon (2026-09-26, a stand-in
    agent, nothing billed): the WASM host, both plugins as components, versions, installs,
    updates, Use previous and Remove ([plan status](../plugin-system-plan.md#status-2026-09-27)).
    Since then both plugins moved to their own repositories, and (2026-09-27) the daemon stopped
    carrying plugin bytes: the lock is the curated list, and first-party plugins install from
    their releases like any plugin. Later: Browse renders disabled, "later"; the `switched-on` /
    `switched-off` events (declarable, never delivered); the UI-facing `query` route (the export
    exists, no route calls it); the `exec` and `watch` host imports (WIT 0.2), which the LaTeX
    and Typst plugins' `build` point waits for ([plan](../latex-reports-plan.md#the-plugin-shape)).

## The plugin host

- **What & when.** A workbench plugin is a Rust crate compiled for `wasm32-wasip2` into one
  portable `plugin.wasm` (the same file on a Mac, an x86 login node or an ARM box) beside its
  `plugin.toml`, run by the daemon's host through the pinned WIT world `chimaera:plugin@0.1.0`.
  It exports `tools`, `instructions`, `call-tool`, `knowledge`, `query` and `on-event`, and
  reaches nothing but the host's imports. Every plugin, a first-party one included, runs from
  an installed copy under `~/.chimaera/plugins/<id>/<version>/`
  ([below](#versions-installs--updates)); the daemon binary carries no plugin bytes, so a daemon
  with nothing installed has nothing to load.
- **Where it lives.** `plugins/runtime.rs` (engine, instances, deadlines, faults; the entry
  points `offer` / `call_tool` / `knowledge` / `on_event`, `hook`, `session_ended`),
  `plugins/hostfns.rs` (the WIT `host` interface), `crates/chimaera-plugin-api/wit/chimaera.wit`
  (the world; the daemon's `bindgen!` reads the same file). Tests:
  `crates/chimaera-server/src/tests/plugin_host.rs`, against `plugins/test-fixture` (embedded
  only by test builds, from `plugins/dist-test`, never shipped).
- **Key behaviors** (every limit is the host's, so no plugin can forget one):
  - **One engine per process** (wasmtime, Cranelift, epoch interruption), built on first use: a
    daemon whose user never switches a plugin on never builds it. Each build compiles once, off
    the reactor, and stays in memory (the last two per plugin across the 32 most recently used plugin ids, keyed by SHA-256, so a moved
    `current` never runs old code).
  - **One instance per (plugin, workspace)**, created lazily, one call at a time, at most 64
    daemon-wide (the least recently used idle slot goes), dropped after 10 min idle or when the
    switch flips. In-flight and queued calls retain their slot; when every slot is busy, new
    instances are refused until one is free. A reset instance still counts until its call ends.
  - **Per-call deadlines:** 5 s (30 s for `knowledge`), enforced on a 100 ms epoch tick, plus an
    outer timeout for a host call stuck on a slow filesystem. **Memory:** 64 MiB of linear
    memory across all memories in an instance, 65,536 table elements across its tables,
    and at most 16 memories, 16 tables and 64 core instances. Each memory reserves 64 MiB
    of virtual address space instead of wasmtime's 4 GiB (login nodes run under `ulimit -v`). **WASI grants nothing:** no files, env, args or network; the guest's
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
    call gate come from the manifest). Tool names are unique within a manifest, 1–64 ASCII
    letters, digits, underscores, dots or dashes; built-in tool names are reserved. If active
    plugins share a tool name, the first in catalog order both advertises and handles it.
  - **Events reach only the plugins that declare them** (`provides.events`): a hook never
    instantiates a plugin that ignores hooks.
  - **Measured** (2026-09-26, macOS arm64): the release binary 26.8 → 38.4 MB (Cranelift); a
    release-grade compile of Agent notes (135 KB) 55–82 ms and of Mycelium (305 KB) about
    200 ms, once per daemon lifetime; release daemon RSS 5.8 MB idle, 10.2 MB with a workspace
    and a session, 28.8 MB after the first plugin call (compile + instantiate, 70 ms end to end)
    and 28.9 MB after 50 more; a warm tool call through the MCP endpoint 1–2 ms (P1's
    measurement), under 10 ms in the live proof.

## Versions, installs & updates

- **What & when.** The daemon ships no plugin bytes. It embeds `plugins/plugins.lock`, the
  curated list of first-party plugins (per plugin: `id`, `name`, `summary`, the pinned
  `version`, `repo` and that release's two sha256s), and every plugin — first-party or not — is
  installed per host into `~/.chimaera/plugins`: a first-party one from its own repository at
  the version the lock pins, a third-party one from its latest release (or a tag), a local build
  from a directory. Each copy carries its own version and the WIT version it targets, and the
  card says which version runs and whether the Chimaera maintainers approved the plugin (it is
  in the curated lock). An installed copy updates from its repository on its own cadence, goes
  back to its previous version and is removed — always the user's click. The safety mechanism
  is the daemon's, not the card's: every download is checked against its release's
  `SHA256SUMS` (and a first-party one against the lock too), and every installed copy again
  each time the catalog loads it; a copy that fails is never loaded, and only that failure
  reaches the card, as its fault line.
- **How it's used.** The card's head: name · a small check badge when the plugin is in
  Chimaera's curated list (tooltip "Verified by the Chimaera maintainers"; a plugin from any
  other repository has none) · version (tooltip "chimaera pins x" when a first-party copy runs
  another) · a quiet "local build" tag for a copy installed from a directory. A first-party
  plugin with nothing installed shows **Install** (it installs the version shown, the one
  chimaera pins) where the switch would be; its tooltip says what happens: "Downloads it from
  github.com/<repo> into this host's ~/.chimaera/plugins. It does nothing until you switch it
  on in a workspace." Its whole top is one button (a small chevron right of the summary shows
  it; `aria-expanded`, Enter / Space; Install and **…** keep their own clicks) that opens the
  card in place — several can stay open, and they stay open while the page lives: a quiet
  "loading…", then everything an installed card shows, from the release chimaera pins — the
  author's description in full, **For you** / **For agents**, the **Agent-side plugin** box with
  each agent's state (no actions until the plugin is installed) — then "Installing downloads it
  from github.com/<repo> (≈ 305 KB) into this host's ~/.chimaera/plugins. It does nothing until
  you switch it on in a workspace." (the size when GitHub gives it) and a second **Install**, so
  the reader needn't scroll back up. When GitHub can't be reached the body is one line: "couldn't
  reach github.com — the summary above is all we know for now". Then the rest of the card
  ([above](#workbench-plugins)). When a check
  found a newer release, a calm callout inside the card says "0.1.2 is available" with a
  "what changed" link (the release page) and a small **Update**. The **…** menu holds **Check
  for updates** (afterwards a muted "No newer version · checked just now" line, which ages —
  "checked 2 hours ago"), **Use previous version (0.1.1)**, **Open on GitHub** (the repository
  it installs from) and **Remove…** (a dialog naming the versions it deletes). Each change
  reports one outcome line, never a checksum ("installed Agent notes 0.1.2", "updated to
  0.1.2", "back to 0.1.1 — Use previous returns to 0.1.2"). A plugin this daemon can't run
  shows why in a callout ("needs chimaera ≥ x (this is y)", "needs a newer chimaera: …",
  "needs a newer plugin: …", "The downloaded files don't match what the release published —
  reinstall it", with **Reinstall** for that one) and stays off, its switch disabled; a plugin
  that failed five times in a minute here adds "Switching it off and on starts it again."
  Under the cards, the small **Install from a repository** form (label, an `owner/repo` field,
  **Preview** and **Install**, both enabled once something is typed, and "The latest release of a
  plugin's GitHub repository: Preview shows what it adds, Install puts it on this host. It does
  nothing until you switch it on in a workspace.") takes `owner/repo` (or its
  `https://github.com/owner/repo` URL). **Preview** shows that release as a card under the form —
  the opened body above, the check badge only for a first-party repository, "not installed"
  where the switch would be, a close button, and its own **Install** — without installing
  anything; **Install** (the form's or the preview's) fetches the latest release (a first-party
  plugin's repository: the pinned version), reporting the plugin and its version — or the
  daemon's refusal, as a failed Preview does — in one outcome line, and a preview installed from
  gives way to the new card; like any plugin, it runs only where it is switched on. On the
  daemon's host: `chimaera plugin list` · `add <id>` · `add <owner/repo> [--version x]` ·
  `add --path <dir>` · `update <id>` · `remove <id>` — the same routes, one line per change
  ("installed agent-notes 0.1.2") and a leading ✓ in the list for Chimaera's own plugins
  ([cli.md](cli.md)).
- **Where it lives.** `plugins/mod.rs` (the lock — `Locked`, `parse_lock`, `lock_entries` —,
  `resolve`, `gate`, `Catalog`, `listing`, `manifest_json` / `available_json`),
  `plugins/installed.rs` (`scan`, which reads each copy and checks its files; `install_route`
  — a release or `{path}` —, `pinned_install_route`, `update_route`, `rollback_route`,
  `remove_route`), `plugins/releases.rs` (`check`, `check_all`,
  `check_route`; run from `update.rs::run_checker`), `plugins/preview.rs` (`details_route`,
  `preview_route`, the `Previews` caches), `crates/chimaera/src/plugin.rs`; UI
  `PluginCard.svelte`, `InstalledView.svelte`, `store.ts`. Tests: `crates/chimaera-server/src/tests/plugin_updates.rs`,
  against a fake releases server and the locked releases in `plugins/dist-test`. The wire — one
  entry shape (`manifest_json`) on `GET /plugins`, `GET /workspaces/{id}/plugins` and in the
  `plugin` field of every change answer:
  - **An installed entry:** `id`, `name`, `summary`, `description` (the author's prose, null
    when none), `homepage`, `adds {ui, agents}`, `provides {knowledge, mcp_tools, views}`,
    `setup`, `detect`, `requires_summary` / `recommends_summary` (the author's sentence about
    the agent-side plugin, null when none), then `version` (the installed
    copy's), `api` (the WIT version it targets, `"0.1"`), `source: "installed"`,
    `installed: true`, `first_party`, `verified`, `sha256_wasm` (the loaded copy's
    `plugin.wasm`), `path` (the version directory), and only when they hold something: `repo`
    (the `owner/repo` it updates from, its `[release] github`), `pinned_version` (first-party
    only: the lock's version), `local_path` (a local install's source directory), `previous`
    (what Use previous goes back to), `update` (`{version, url, checked_ms}`, when a check found
    a newer release) and `fault` (why it can't run, in the card's words: a gate, files that
    don't match their release, or the runtime's fault in a workspace).
  - **An available entry** (a lock id with nothing installed): `id`, `name`, `summary` from the
    lock; `description: null`, `homepage: null`, `adds {ui: [], agents: []}`,
    `provides {knowledge: null, mcp_tools: [], views: []}`, `setup: null`, `detect: []`,
    `requires_summary: null`, `recommends_summary: null`;
    `version` (the pinned one), `api: null`, `source: "available"`, `installed: false`,
    `first_party: true`, `verified: false`, `repo` and `pinned_version` (the lock's).
  - `GET /workspaces/{id}/plugins` adds per entry `on`, `detected`, `active` (all false for an
    available entry), `requires` and `recommends` (lists of `{agent, id, marketplace}`).
  - **Before an install** — `GET /plugins/{pid}/details` (an installed plugin: its entry plus
    `requires` / `recommends`; 404 for an id that is neither installed nor in the lock) and
    `POST /plugins/preview {github}` answer a release's manifest in the installed entry's shape —
    `description`, `homepage`, `adds`, `provides`, `setup`, `detect`, `requires_summary` /
    `recommends_summary`, `requires` / `recommends`, `version`, `api` — with `source:
    "available"`, `installed: false`, `verified: false`, `first_party` (a lock entry, or the
    lock's repository), `repo`, `pinned_version` (first-party only), `release_url` (the release
    page), `download: {wasm_bytes}` when the releases API gives the component's size, and `fault`
    when this daemon's gates would refuse it. Refusals: 400 (not `owner/repo`), 422 (a
    `plugin.toml` that isn't what the lock or the release's `SHA256SUMS` lists — "<name>'s
    description on GitHub isn't the one chimaera approved" — or doesn't parse), 502 ("couldn't
    reach github.com — the summary above is all we know for now"; a preview: "couldn't read
    <repo>'s releases on github.com — check the name, or try again later").
  - **Change answers:** install (all three kinds) and update →
    `{id, version, previous, sha256: {"plugin.wasm", "plugin.toml"}, plugin}`; rollback →
    `{id, version, previous, plugin}`; remove → `{id, removed: true, plugin}` (the available
    entry for a lock id, else null); check → `{id, update, plugin}`. Refusals are `{error}` with
    400 / 404 / 409 / 422 / 502.
- **Key behaviors.**
  - **Seeing a plugin before installing it writes nothing.** `/details` of a first-party plugin
    with nothing installed reads its pinned release's `plugin.toml` (the direct download Install
    uses; its sha256 must be the lock's `sha256_toml`) and, alongside, the release's asset list
    for the component's size; the answer is kept in memory per (id, version) for the daemon's
    life, so a second open fetches nothing. `/preview` asks the repository's latest release,
    checks its `plugin.toml` against that release's `SHA256SUMS`, and keeps the last 32
    repositories per (repo, tag) — the releases API is still asked each time, since the latest
    tag can move; the lock's own repository previews its pinned release, which is what Install
    installs from it. Nothing is fetched at boot or by the daily checker, a failure is never
    kept, and each fetch rides the same 10 s / 1 MiB fence (the manifest capped again at 64 KiB).
  - **The catalog is the lock's entries plus the installed copies.** A lock entry with no copy
    installed is listed `available`; everything else is an installed copy under
    `<data dir>/plugins/<id>/<version>/`. Only an installed copy loads, can be switched on,
    offers tools or is active. There is no embedded copy, so no precedence between two copies of
    one id.
  - **`first_party`** (the card's check badge, "Verified by the Chimaera maintainers"): the id
    is in the lock — the maintainers' curated list — AND the installed copy's
    `[release] github` matches the lock's `repo` (case-insensitive), with either exact pinned
    bytes or a `source-github` marker recorded by the host when installing from that repository.
    A local build cannot grant itself the badge through its manifest or copied marker. Available
    lock entries are first-party by definition; updates from the official repository keep it.
  - **`verified`** — the integrity check, never shown on the card (only a failure is, as the
    fault line) — checked every time the catalog loads a copy (off the reactor): both files are
    re-hashed against the `SHA256SUMS` kept beside them. A match → `verified`; no `SHA256SUMS`
    (a local build without one, or a copy installed before chimaera kept the file) → unverified,
    no error; a mismatch, or a `SHA256SUMS` that doesn't list both files → listed with the fault
    "the downloaded files don't match what the release published — reinstall it" (the log line
    names the file): it never loads, its switch refuses on, and Use previous to that version is
    refused. **Reinstall** — installing that same version again from its repository — replaces
    such a copy in place (an intact copy at that version still answers 409). A first-party copy at the lock's pinned version
    is `verified` only when its bytes also equal the lock's two sha256s, so a local build at the
    pinned version is unverified, without a fault.
  - **Three install kinds:**
    - **First-party** — `POST /plugins/{pid}/install` for a lock id, or
      `POST /plugins/install {github}` naming the lock's repository (with no `version`, or the
      pinned one): the lock's version, fetched from
      `https://github.com/<repo>/releases/download/v<version>/{SHA256SUMS,plugin.toml,plugin.wasm}`
      (direct download URLs, no API call). Both files must match the release's `SHA256SUMS` AND
      the lock's sha256s (a mismatch with the lock is a 422, "<name> <version> on GitHub isn't
      the release chimaera approved, so it wasn't installed", with the hashes in the log); a
      download that doesn't match its release's list is a 422, "the downloaded files don't
      match what the release published — try again"; the manifest's id must be the lock's, its version the pinned one and its
      `[release] github` the lock's repo, and it must pass the gates. The copy is `verified` and
      `first_party`. 409 when that version is already installed.
    - **Third-party** — `POST /plugins/install {github, version?}`: the latest release, or the
      tag `v<version>`, found through the GitHub releases API and verified against its
      `SHA256SUMS`.
    - **A local build** — `POST /plugins/install {path}` (an absolute directory) or
      `chimaera plugin add --path <dir>`: copies `plugin.toml`, `plugin.wasm` and, when present,
      `SHA256SUMS` (both files must then match it, else 422) into `<id>/<version>/` and records
      the source directory in `local-path`. The manifest is parsed, validated and gated as
      usual. Re-installing the same version from a path replaces it — the development loop:
      rebuild, `chimaera plugin add --path <dir>`, and the next `tools/list` is the new build.
      It is also how a host with no network installs. `verified` only with a matching
      `SHA256SUMS` (and, at a first-party id's pinned version, the lock's sha256s too); either
      way the card notes "local build" (`local_path` is set).
  - **Layout:** `~/.chimaera/plugins/<id>/<version>/{plugin.toml,plugin.wasm,SHA256SUMS[,local-path]}`
    (the daemon's data dir) behind `current` and `previous` links swapped atomically (a symlink
    under a fresh name, renamed over the old). At most two versions stay on disk; **Use
    previous** swaps the two links, so it is reversible; **Remove** deletes the id's directory,
    and a first-party plugin then shows as available again.
  - **Gates before anything loads:** `api` must be a WIT version this host serves (`0.1`) and
    `requires.chimaera` must match this daemon (a dev build matches every requirement). A plugin
    failing one stays listed, off, with its reason; its switch refuses on (409), and a switch
    already on is kept for when the gate passes again.
  - **A release install and an update are one path:** the release's `SHA256SUMS` and
    `plugin.toml` first; the manifest's id, the tag's version and the gates checked; then
    `plugin.wasm` streamed to a temp dir under a 16 MiB cap, verified, renamed into
    `<version>/` with the release's `SHA256SUMS` beside it, links swapped. Any failure leaves the
    old version current. One change at a time daemon-wide, an audit log line each; after it the
    catalog reloads and the plugin's instances go, so a running session's next `tools/list`
    carries the new version's tools.
  - **Update, rollback, check and remove need an installed copy:** on a plugin that isn't
    installed they answer 409 "<name> isn't installed — install it first" (a lock id) or 404
    "unknown plugin".
  - **The checker never downloads.** For each installed plugin whose manifest names
    `[release] github`, it asks the GitHub releases API once after boot and then daily (riding
    the daemon's own update loop, off with `update.autoCheck`; a dev build skips it unless
    `CHIMAERA_PLUGIN_RELEASES_API` is set) and on **Check for updates**, reads only the release's
    `plugin.toml`, and offers a version only when it is strictly newer than the one that runs
    and passes the gates. Offers live in memory. **Update** installs it through the release path
    (its `SHA256SUMS` kept). A first-party plugin updated past the pin keeps its check badge
    (and stays `verified`); the card names the pin ("chimaera pins 0.1.0") only in a tooltip. What the lock
    pins changes only with a chimaera release: a bump is a reviewed change to the lock.
  - **Knobs** (tests and live proofs): `CHIMAERA_PLUGIN_RELEASES_API` replaces the GitHub API
    base the checker and third-party installs ask; `CHIMAERA_PLUGIN_DOWNLOADS` replaces
    `https://github.com` in the first-party download URLs.
  - A release is a tag `v<version>` with three assets: `plugin.wasm`, `plugin.toml` (that
    version's manifest) and `SHA256SUMS`. A plugin's state and its per-workspace switch follow
    its id across versions, and the switch survives a remove and a reinstall.

## Agent plugins & the Skills view

- **What & when.** "What can my agents do here?" — answered by asking each agent CLI, never by
  re-deriving its discovery rules.
- **How it's used.** The Plugins segment's agent section lists each CLI's plugins: claude from
  `claude plugin list --json` (id, version, scope, enabled) plus `claude plugin details` totals
  (skills, hooks, always-on tokens — for the first 12); codex from a short-lived
  `codex app-server` (`skills/list`, `hooks/list` for the workspace cwd — no thread, no model
  call), a codex plugin being whatever `pluginId` codex attributes skills and hooks to. The
  **Skills** segment lists every skill once per name, grouped by origin — this project
  (`<root>/.claude/skills`; codex `repo` scope) · from plugins (enabled claude plugins'
  `skills/`; codex `pluginId`) · yours (`~/.claude/skills`; codex `user`) · built into the agent
  (claude's catalog from a *live* claude chat session's handshake in this workspace — present
  only while one runs, `_`-prefixed internals dropped) — split as **Built into claude** (`/name`
  chips; without a running claude chat a muted "claude lists its built-in skills only while a
  claude chat runs.") and **Built into codex** (codex's system skills as `$name` chips), each
  chip copying its invocation. Each row carries one chip per agent —
  available ✓ · off ◌ ("disabled in codex's config") · absent, with the reason in words ("codex
  is not installed here", "the plugin is not installed for claude") — and the invocation in each
  agent's own syntax (`/name` claude, `$name` codex). Codex's skill load errors are listed.
  The controls follow Settings' recipes — the segmented All · claude · codex, a search field,
  the counts — and a row opens in place into a small definition list: **Use it** (each
  agent's invocation, copyable), **File** (open SKILL.md), **Problems** (load errors).
- **Where it lives.** `agent_probe.rs` (`claude_state`, `codex_raw` / `codex_state`,
  `CodexRpc`, `agent_plugins`, `skills`, `scan_skills`); UI `SkillsView.svelte`,
  `skillsModel.ts`. Wire facts: [PROTOCOL.md Pass 35](../../crates/chimaera-agent/PROTOCOL.md).
- **Key behaviors.** Every child is login-shell wrapped, time-boxed (20 s CLI, 15 s per RPC),
  output-capped, `kill_on_drop`; **one probe runs daemon-wide at a time** (a semaphore — three
  windows on the tab never spawn three app-servers); answers are cached 60 s; `?refresh=true`,
  an install ending and a trust write invalidate. Only writes broadcast a changed epoch;
  explicit refreshes do not. Probes started before invalidation cannot refill the cache with
  stale results. Claude has no skills-list API, so its side is
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
- **Where it lives.** The WASM plugin in its own repository, [chimaera-plugin-agent-notes](https://github.com/martinappberg/chimaera-plugin-agent-notes) — `src/lib.rs` (the exports:
  `post_note`, `read_notes`, the hook line, read cursors in host state), `src/notes.rs` (the
  addressing rule, unread, the texts agents read; unit-tested natively), `plugin.toml`
  (declares the `hook` and `session-ended` events). What stays in core, `notes.rs`: `deliver`,
  `tell_mastermind`, `take_post_slot` (the posts-per-minute window the host's Timeline appends
  share), `append_note`, `age`; the hint reaches claude through `plugins::runtime::hook` from
  `agents.rs::ingest`. UI `TimelineRow.svelte` / `TimelineView.svelte` (deliver) and
  `MastermindDock.svelte` (the inbox chip). Tests: `crates/chimaera-server/src/tests/plugins.rs`
  pins the tool definitions and the texts byte for byte; the instruction paragraph is the
  plugin's own wording (0.1.2 rewrote it), so the test checks it names `post_note` and
  `read_notes`.
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
