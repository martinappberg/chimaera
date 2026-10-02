# Plugins & skills

One **Extensions** tab, in four segments — **Plugins** · **Connections** · **Skills** · **Browse** — for two kinds
of add-on, different in who runs them: **workbench plugins** run in Chimaera (each a WebAssembly
component the daemon runs in a sandbox, installed per host, opt-in per workspace, saying in words
what it adds) and **agent plugins** run inside claude / codex (read from the agents themselves,
installed only through their own plugin managers). Beside them: the **Skills** segment (every
skill each agent can use here) and in-app **codex hook trust**. Agents talking to each other is
built in now, no plugin: Agent communication (Settings → Agents). **Browse** (searching the
marketplaces the agents already have) renders disabled, "later". Design: the WASM host, versions
and updates in [docs/design/plugin-system-plan.md](../design/plugin-system-plan.md); trust — what a plugin can
do and who approved it — in [docs/design/plugin-platform-plan.md](../design/plugin-platform-plan.md) §1–§2
(its phase P6, [below](#trust-what-a-plugin-can-do-and-who-approved-it)); the seam and the tab in
[docs/design/timeline-knowledge-plugins-plan.md](../design/timeline-knowledge-plugins-plan.md) §6–§7. Writing a
plugin: [docs/agent-guides/plugins.md](../agent-guides/plugins.md).

**Where it lives (shared):** daemon `crates/chimaera-server/src/plugins/` — `mod.rs` (the
manifest, the embedded lock, the catalog and its gates, detect, the workspace routes),
`runtime.rs` (the wasmtime host), `hostfns.rs` (every host function, bounded), `tools.rs` (plugin
MCP tools through the runtime), `installed.rs` (the installed directory; install, update,
rollback, remove), `releases.rs` (the release checker), `capabilities.rs` (what a manifest can
do: `[access]`, the atoms, the digest, the tier, the Can list), `trust.rs` (standing, trust
records, admission, the admin policy, holds), `revoke.rs` (the kill switch), `activity.rs` (the
activity log), `retired.rs` (plugins whose job moved into Chimaera,
[below](#agent-notes)) — plus `agent_probe.rs` and `comms.rs` (the post window plugins' Timeline appends share);
the interface `crates/chimaera-plugin-api` (the WIT world and its Rust bindings,
[map](../../crates/chimaera-plugin-api/AGENTS.md)); the first-party plugins in their own
repositories, [chimaera-plugin-mycelium](https://github.com/martinappberg/chimaera-plugin-mycelium), [chimaera-plugin-latex](https://github.com/martinappberg/chimaera-plugin-latex) and [chimaera-plugin-typst](https://github.com/martinappberg/chimaera-plugin-typst), whose releases
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
`POST /plugins/{pid}/trust {caps}` or `{allow_block: true}` · `DELETE /plugins/{pid}/trust` ·
`POST /plugins/{pid}/skip {version}` · `GET /plugins/{pid}/activity` ·
`POST /workspaces/{id}/plugins/{pid}/install {agent, agent_plugin_id?}` ·
`POST …/{pid}/setup {agent}` · `POST …/{pid}/trust-hooks {hooks:[{key,hash}]}` ·
`GET /workspaces/{id}/agent-plugins?refresh=` · `GET /workspaces/{id}/skills?refresh=` ·
`POST /workspaces/{id}/timeline/{seq}/deliver`; plugin tools ride the per-session MCP endpoint
([linked-terminals.md](linked-terminals.md#the-mcp-server)); a plugin's `emit` reaches
`/ws/events` as a `{"type":"plugin","plugin":…,"workspace":…}` frame, sent only to windows
showing that workspace (no UI reads one yet).
The additive `{"type":"agent_plugins","epoch":…}` frame invalidates agent reports after
installation or hook trust and on reconnect; it carries no plugin payload.

## Connections

- **What & when.** The second segment lists configured MCP servers and installed hosted
  connectors for Claude Code, Codex, Antigravity and Grok Build in the current workspace on this host.
  Plugins remains the default. Agent account login is outside this flow.
- **How it's used.** Open Extensions → Connections. **Check again** refreshes the agents'
  reports. **Sign in** opens a dialog with a browser authorization link, progress,
  cancel and retry. Hosted Claude connectors offer **Set up**, with a dialog linking to
  Claude's connector settings and a **Check connection** action after setup in the browser;
  local MCP sign-ins accept the full callback URL from the browser when needed. The agent
  still owns OAuth and credential storage. Hosted authorization is managed by the vendor,
  not stored by chimaera on this host. Hosted connectors also offer **Manage** when
  no setup is needed.
- **Where it lives.** UI `plugins/ConnectionsView.svelte` and `plugins/connections.ts`;
  daemon `agent_probe/connections.rs`. Bearer-authenticated routes:
  `GET /workspaces/{id}/connections?refresh=true` and
  `POST /workspaces/{id}/connections/login {agent,name}`; job status/cancel at
  `GET/DELETE …/login/{attempt}`, callback input at `POST …/{attempt}/callback {url}`,
  and hosted verification at `POST …/{attempt}/check`. Auth jobs live in
  `agent_probe/connections/auth.rs`; the UI is `plugins/ConnectionDialog.svelte`.
- **Key behaviors.** Claude's `mcp list` reports connection health. Codex's `mcp list --json`
  reports configuration and credential status; **Configured** and **Signed in** do not
  promise a live connection. Its `app/installed` snapshot reports effective availability
  of ChatGPT apps; policy restrictions are not treated as expired authentication. Missing
  or unsupported reports show an explicit error. No agent turn is started to list services.
  Probes use the host + workspace Environment, share the daemon's single-flight gate,
  cache for 60 seconds, cap output and rows, and time out. Endpoints, environment values,
  credentials, and raw CLI errors never enter the report. Sign-in revalidates the selected
  server and passes names as literal arguments. Its child process is cancelled on close,
  timeout or daemon shutdown. Claude's terminal requirements use an internal PTY without
  a terminal pane or saved output; Codex accepts piped input.
  Auth URLs exist only in memory during a bounded job; callback
  input is never persisted or echoed in status. A fresh agent report must confirm completion before
  the dialog says Connected. Existing sessions may need reconnection or reopening.
  Claude's hosted "needs authentication" can also cover configuration errors or a connector
  that hasn't been activated. The UI says **Setup needed in Claude** and opens its settings,
  avoiding a forced OAuth flow for public connectors. Browser errors are only visible in
  the vendor's page; the dialog explains where to review them and retains a settings action
  after expiry or failure.
  This first pass does not add/remove server configurations or implement a connector catalog.

## Workbench plugins

- **What & when.** Opt-in capabilities that change what the UI or the agents get, off by
  default. The first-party ones are WASM plugins in their own repositories at the releases
  `plugins/plugins.lock` pins: **Mycelium** (fills Knowledge and "Where things stand"; adds
  `knowledge_search` · `knowledge_get` for every agent here), and **LaTeX** and **Typst**
  ([below](#programs-and-tools-the-privileged-tier)). The daemon carries none of them: each is
  listed as *available* until the user installs it on this host
  ([below](#versions-installs--updates)).
- **How it's used.** Open Extensions from the rail's **Extensions** row or quick-open
  ("Extensions"; "plugins" and "skills" find it too). Its header stays put across segments —
  "Extensions" and a chip naming the host ("Extensions and connections on this host") on the left, the
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
    empty = always present, as for LaTeX). An available plugin is never active: switching
    it on answers 409 "<name> isn't installed — install it first". The switch persists in
    `workspaces.json` by plugin id — durable or refused, since a forgotten toggle would silently
    change what agents see (written off the workspaces lock, taken back if the write fails) —
    so reinstalling a first-party plugin that was on here makes it active again. A third-party
    plugin's switches go when it is removed, and a plugin new to this daemon that isn't
    first-party starts off everywhere: a switch kept under its id never binds another
    publisher's plugin. Switching off is accepted for any id, installed or not. Flipping the
    switch drops the plugin's instance there and clears a fault. Detection is a few
    `stat`s off the reactor, cached 30 s per workspace, re-run on every switch and before any
    answer that decides what an agent sees.
  - **A plugin changes what agents see only where it is active.** With none active, the
    plugin integration adds nothing to MCP `tools/list`, `initialize` instructions, generated
    Claude settings or Codex argv; the baseline respects Chimaera's built-in communication
    settings and tools — pinned by `crates/chimaera-server/src/tests/agent_view.rs`. Where
    active, a plugin's tools join `tools/list`, pass the call gate (elsewhere a call is refused,
    JSON-RPC -32602, "… isn't switched on in this workspace …"), and its instruction paragraph
    joins `initialize`, in catalog (id) order. A plugin that can't answer (refused, faulted)
    adds nothing.
  - **Pre-allowed at spawn:** claude — `mcp__chimaera__<tool>` in the generated settings'
    `permissions.allow` (chat and TUI); codex chat — the driver's `mcp_auto_approve`; codex
    TUI — `-c mcp_servers.chimaera.url=…` + `bearer_token_env_var="CHIMAERA_MCP_KEY"` (the key
    in the PTY env, never argv) + per-tool `approval_mode="approve"` (live: a pre-approved
    tool runs with no prompt, a linked-terminal tool still asks — PROTOCOL.md Pass 35). **A
    Codex TUI gets the chimaera MCP server when agent communication is on (default), a
    workbench plugin with tools is active, or a Mastermind is appointed there**. ACP chats
    receive the same tools but retain their native approval requests.
    Pre-allows are baked at spawn: a claude session or codex chat started before the switch
    sees the tools (tools/list is per call) but its agent asks per call, and a codex TUI
    started without any MCP injection needs a respawn to gain it; switching off gates calls at
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
    asked. `requires` is a hard requirement ("It needs claude or codex, and neither is installed
    on this host." when none is). `recommends` helps the
    agents the user runs but is never needed — mycelium's `mycelium@mycelium` is the example:
    the Knowledge reader works with no agent plugin at all. Both ride
    `GET /workspaces/{id}/plugins` as lists of `{agent, id, marketplace}`, beside
    `requires_summary` / `recommends_summary`.
  - **Agent-plugin installs are the agent's own CLI** in a visible `install <plugin> for
    <agent>` terminal (ids and marketplace sources charset-gated, never flag-shaped); 409 when
    the agent binary is missing. `plugins/install-agent.sh` receives metadata as arguments,
    never shell source. It inherits the host + workspace **Settings → Environment** commands
    so module loads and PATH setup apply, and the git **Settings → Git binary path** names
    goes first on its PATH when it is new enough (≥ 2.15) — a login node's stock
    `/usr/bin/git` 1.8.3 can't do Claude's marketplace clone. A failed
    Claude marketplace fetch explains missing Git or lack of `--shallow-submodules` support;
    an existing cached marketplace can still install. Codex's fetcher is independent of this
    Git requirement.
    Success and failure output stay visible until **Enter** closes the
    terminal; the original exit status is preserved. The probe cache is invalidated when the
    command finishes, even while its result remains open, and visible Extensions tabs
    automatically recheck via `/ws/events`. The card offers **continue setup**
    after an agent install is opened, including one launched from the attach sheet. Returning
    to Extensions during this flow and opening the sheet request a fresh agent report. Once
    the specific requested add-on is detected (even disabled), the card says
    **Installed for <agent> — continue setup**. Card and sheet select that add-on by
    its manifest id, so a required and recommended plugin for one agent stay distinct.
    Full marketplace ids take precedence over short names. If a short name could refer
    to different marketplaces in the manifest or report, the row says the marketplace
    is unclear and does not confirm installation or infer hook ownership.
    The setup sheet repeats this warning and blocks completion until resolved;
    setup runs only through an agent with known enabled add-ons and all its requirements.
    Omitting the optional id preserves required-first selection for older clients. The
    sheet resumes the existing install → hook review → workspace setup steps; installation
    alone does not trust hooks or initialize a repository. Newly started agent sessions load
    the installed plugin. Setup is a fresh chat plus one Send (502 if the prompt didn't land —
    the session still exists).
    Chimaera never installs or updates anything on its own.
  - **Current platform.** The initial isolated live proof (2026-09-26, stand-in agent,
    nothing billed) covered the WASM host, the two components then present, versions,
    installs, updates, Use previous and Remove
    ([dated plan status](../design/plugin-system-plan.md#status-2026-09-27)). Plugins now live
    in separate repositories and install from locked releases; the daemon carries no plugin
    bytes. Capability trust, both WIT worlds, plugin screens and file views, query routes,
    switch events, program jobs and tool downloads are implemented (sections below).
    LaTeX and Typst use the 0.2 platform. **Browse** remains disabled, "later".

## The plugin host

- **What & when.** A workbench plugin is a WebAssembly component (first-party plugins use
  Rust compiled for `wasm32-wasip2`) in one portable `plugin.wasm` (the same file on a Mac, an x86 login node or an ARM box) beside its
  `plugin.toml`, run by the daemon's host through the pinned WIT worlds `chimaera:plugin@0.1.0` and `@0.2.0`.
  Both export `tools`, `instructions`, `call-tool`, `knowledge`, `query` and `on-event`.
  A component reaches nothing but the host's imports. The 0.2 world adds platform imports and screen/action
  exports, including declared program jobs; 0.1 remains supported unchanged. Every plugin, a first-party one included, runs from
  an installed copy under `~/.chimaera/plugins/<id>/<version>/`
  ([below](#versions-installs--updates)); the daemon binary carries no plugin bytes, so a daemon
  with nothing installed has nothing to load.
- **Where it lives.** `plugins/runtime.rs` (engine, instances, deadlines, faults; the entry
  points `offer` / `call_tool` / `knowledge` / `on_event`, `hook`, `session_ended`),
  `plugins/hostfns.rs` (the WIT `host` interface), `crates/chimaera-plugin-api/wit/chimaera.wit`
  (the 0.2 world; `wit-0.1/chimaera.wit` retains 0.1; the daemon binds both). Tests:
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
    in that workspace (the card says why) until the user switches it off and on. A build the
    host can't run at all (it doesn't compile, or its imports don't link) is its fault on the
    card too — never recompiled per call, tried again when the switch flips or the plugin's
    version changes. Traps ride Unix
    signal handlers on macOS too (`macos_use_mach_ports(false)`): wasmtime's Mach-port thread
    aborted the whole process when a caught SIGCHLD from an ending PTY shell interrupted it.
  - **Host functions, each bounded** (`hostfns.rs`): workspace-relative `read` / `stat` /
    `list` walked with `O_NOFOLLOW` on every component (`..`, absolute paths and symlinks
    refused), reads ≤ 8 MiB and listings ≤ 4,096 entries, off the reactor behind the filesystem
    semaphore; `state` — 64 KiB per (plugin, workspace), in memory (a restart clears it);
    `sessions`; `timeline-append` of `note` entries only (≤ 2 KiB, from a session's call, under
    the per-session posts-per-minute window agent messages share; a plugin's note is shown on the Timeline, never delivered into an agent) and `timeline-recent`;
    `emit` (one JSON object ≤ 16 KiB); `now-ms`; `log` (≤ 64 lines a call). The host serves the
    workspace and session of the call in flight, never the context a guest passes back.
  - **What a plugin hands back is capped too** — a tool result at 256 KiB, the instruction
    paragraph at 8 KiB, a hook line at 1 KiB, a tool description at 2 KiB; a tool input
    schema must be a JSON object schema (≤ 16 KiB) — and a component whose `tools()` names differ
    from its manifest's `provides.mcp_tools` is refused everywhere (the card's Adds line and the
    call gate come from the manifest). Tool names are unique within a manifest, 1–64 ASCII
    letters, digits, underscores or dashes (dots are refused to keep Codex approval keys literal); built-in tool names are reserved. If active
    plugins share a tool name, the first in catalog order both advertises and handles it.
  - **Events reach only the plugins that declare them** (`provides.events`): a hook never
    instantiates a plugin that ignores hooks.
  - **Measured** (2026-09-26, macOS arm64): the release binary 26.8 → 38.4 MB (Cranelift); a
    release-grade compile of a 135 KB plugin 55–82 ms and of Mycelium (305 KB) about
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
  reports one outcome line, never a checksum ("installed Mycelium 0.2.1", "updated to
  0.1.2", "back to 0.1.1 — Use previous returns to 0.1.2"). A plugin this daemon can't run
  shows why in a callout ("needs chimaera ≥ x (this is y)", "needs a newer chimaera: …",
  "needs a newer plugin: …", "The downloaded files don't match what the release published —
  reinstall it", with **Reinstall** for that one; a retired plugin's "Built into Chimaera now:
  …", [below](#agent-notes)) and stays off, its switch disabled; a plugin
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
  ("installed mycelium 0.2.1") and a leading ✓ in the list for Chimaera's own plugins
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
    copy's), `api` (the WIT version it targets, `"0.1"` or `"0.2"`), `source: "installed"`,
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
    This origin test cannot be self-granted by a local build's manifest or copied marker.
    On the wire, `first_party` also requires **Verified** trust standing: an update that grows
    capabilities, or an unpinned privileged build, loses the badge and requires trust.
    Available lock entries carry the badge; eligible official updates keep it.
  - **`verified`** — the integrity check, never shown on the card (only a failure is, as the
    fault line) — checked every time the catalog loads a copy (off the reactor): both files are
    re-hashed against the `SHA256SUMS` kept beside them. A match → `verified`; no `SHA256SUMS`
    (a local build without one, or a copy installed before chimaera kept the file) → unverified,
    no error; a mismatch, or a `SHA256SUMS` that doesn't list both files → listed with the fault
    "the downloaded files don't match what the release published — reinstall it" (the log line
    names the file): it never loads, its switch refuses on, and Use previous to that version is
    refused. **Reinstall** — installing that same version again from its repository — replaces
    such a copy in place (an intact copy at that version still answers 409) — the old copy is
    set aside until the new one is in place, and put back if the move fails. Every change runs
    to its end even when the window that asked for it closes, so the catalog always follows
    the files. A first-party copy at the lock's pinned version
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
  - **Gates before anything loads:** `api` must be a WIT version this host serves (`0.1` or `0.2`) and
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
    (its `SHA256SUMS` kept). An update retains checksum integrity (`verified`); a first-party
    update keeps its check badge only while it retains Verified trust standing
    (sandboxed with the pinned capabilities, or the exact pinned privileged release); the card names the pin ("chimaera pins 0.1.0") only in a tooltip. What the lock
    pins changes only with a chimaera release, and that follows a plugin release on its own: the
    hourly `plugin-lock` workflow checks the newer release, bumps the lock in a `fix:` PR with
    auto-merge, CI installs it, and the merge cuts a patch release (setup and how to turn a
    version down: `plugins/AGENTS.md`).
  - **Knobs** (tests and live proofs): `CHIMAERA_PLUGIN_RELEASES_API` replaces the GitHub API
    base the checker and third-party installs ask; `CHIMAERA_PLUGIN_DOWNLOADS` replaces
    `https://github.com` in the first-party download URLs.
  - A release is a tag `v<version>` with three assets: `plugin.wasm`, `plugin.toml` (that
    version's manifest) and `SHA256SUMS`. A plugin's state and its per-workspace switch follow
    its id across versions; a first-party plugin's switch survives a remove and a reinstall
    (a third-party plugin's goes with it — see above).

## Trust: what a plugin can do, and who approved it

- **What & when.** Every plugin says what it can do in one list, derived by the daemon from its
  manifest — the card's **Can** list, the trust prompt and what the host enforces are that same
  list. A plugin the Chimaera maintainers verified installs with a click; anything else asks the
  user to trust exactly that list first. An update that asks for more waits for the user, and
  the version that runs keeps running meanwhile. Chimaera can block a bad build everywhere
  (the kill switch), and a host's admin can allow only verified plugins. Shipped as the
  platform plan's phase P6; nothing changed for Mycelium (verified, asking for nothing new).
- **How it's used.**
  - **The Can list** is a row of every card's facts ("Can"), in plain words: "Reads files in this
    workspace" · "Reads the Timeline and posts notes to it" · "Sees this workspace's sessions" ·
    "Gives agents 2 tools: knowledge_search · knowledge_get" · "Fills the Knowledge view" · "Offers an
    agent-side plugin for claude: …" · "Has a setup prompt it sends to an agent you choose". A
    plugin that runs programs ([below](#programs-and-tools-the-privileged-tier)) carries a "runs
    programs" tag and its program lines in the warning tone.
  - **The trust prompt** (`TrustDialog.svelte`) opens when the daemon refuses an install, an
    update, Use previous, a switch or Trust with 409 and what the plugin can do: who asks and
    from where ("github.com/owner/repo", "a local build in /dir"), for an update "It would also"
    (what it asks beyond the version that runs), then everything it can do. **Trust and install**
    / **Allow update** / **Trust it** send the same request again with the capability digest
    shown; an update also offers **Skip this version** (not offered again). A plugin that runs
    programs is confirmed by typing its name. `chimaera plugin add` and `update` print the same
    list and ask on the terminal (`--trust` answers yes for scripts; without a terminal the
    answer is no).
  - **A card that can't run here** says why in a callout: "X waits for your trust …" with
    **Review and trust**; "Chimaera turned X off: … You can switch it back on anyway." with
    **Use anyway** (a soft block); "Chimaera blocked X: … Update or remove it." (a hard block,
    no override); "Off on this host: this host only allows verified plugins." (the policy). Its
    switch words read "waiting for your trust" or "off on this host".
  - **The card's "…" menu** adds **Activity…** (the log, newest first: installs, updates, Use
    previous, trust given and withdrawn, blocks, skips — kept after a Remove) and, for a
    plugin the user trusted, **Withdraw trust…** (it goes off everywhere at once and stays
    installed). CLI: `chimaera plugin trust|untrust|activity <id>`, and `chimaera plugin caps
    <plugin.toml>` prints a manifest's tier, digest and Can list with no daemon.
  - **Settings → Extensions → Unverified Plugins** (`plugins.allowUnverified`, on by default):
    off, only verified plugins install and run on this host. An admin's
    `/etc/chimaera/policy.json` (`{"plugins": {"allowUnverified", "allowPrivileged": "all" |
    "verified" | "none", "blocked": [ids]}}`) can only tighten it; a file that doesn't parse
    fails closed (no unverified and no program-running plugin) and the install form says so.
- **Where it lives.** `plugins/capabilities.rs` (`Access`, `Caps`: the atoms, `digest`, `tier`,
  `covers` / `beyond`, `lines`), `plugins/trust.rs` (`Guard` on `AppState` — trust records in
  `<data dir>/plugins/trust.json`, the policy file, the revocations —; `standing`, `hold`,
  `admit` / `after_admit`, `needs_trust`, the trust / untrust / skip routes), `plugins/revoke.rs`
  (the lists, signature checks, `refresh` on the daily checker, `apply`), `plugins/activity.rs`
  (`<data dir>/plugins/.activity/<id>.jsonl`), `hostfns.rs` (`[access]` on every call), the
  lock's `tier` / `caps`, `plugins/revoked.json` + `plugins/revocation-keys.txt` (embedded) and
  `scripts/revocations.mjs` (make a key, sign a list); UI `TrustDialog.svelte`,
  `ActivityDialog.svelte`, `store.ts` (`TrustNeeded`, `trustChange`), `installCopy.ts`
  (`holdWords`, `trustWords`, `activityWords`); tests `tests/plugin_trust.rs`.
- **Key behaviors.**
  - **Capabilities are atoms** (`["access","files","read"]`, `["agent-tool","knowledge_search"]`, …);
    the **digest** is the SHA-256 of the sorted atoms under `chimaera-caps/1`, so a kind added
    later changes no digest of a plugin that doesn't use it. "Asks for more" is set difference;
    "covered" is subset. A 0.1 manifest without `[access]` is read as exactly what 0.1 allowed
    (files, the Timeline with notes, sessions); the host refuses each read a build's
    `[access]` doesn't allow (a file read errors, `timeline-recent` answers nothing,
    `timeline-append` errors, `sessions` answers nothing).
  - **Standing.** *Verified*: the lock covers it — the pinned release byte for byte, or a
    sandboxed first-party update whose digest is the lock's `caps` (the check badge follows
    this, so a first-party update that grew loses the badge and is the user's to trust).
    *Trusted*: a trust record for this id, source (`github:<repo>` or `path:<dir>`) and digest.
    *Untrusted*: installed and off everywhere until trusted. Records cover a digest, not a
    version: an update that asks for nothing new asks nothing. The first daemon with no
    `trust.json` trusts every copy already installed (the user's own installs); one that
    doesn't parse trusts nothing (fail closed) until the user trusts again. Remove forgets the
    records (a later install is a new question) and the plugin's kept state.
  - **Admission** (install, update, Use previous, local build): blocked → 422 before any
    download; the policy → 403; verified, trusted, or asking for no more than the covered build
    it replaces (a record is written, `how: "subset"`) → proceeds; the caller's `trust` equal to
    the digest → proceeds (`how: "prompt"`); else 409 with `trust` (`id`, `name`, `version`,
    `source`, `tier`, `caps`, `can`, `grown`, `from_version`, `confirm`). All of it before the
    component is fetched. A local build asks once per id and digest (the rebuild loop asks
    nothing).
  - **Holds** — why a loaded plugin may not run here — are checked by `active`, so a held plugin
    offers agents nothing: blocked (kill switch, or the policy's list), refused by the policy,
    or untrusted. The card wire carries `standing`, `hold` (`{kind, level?, reason?}`) and
    `skipped_version`; every entry carries `tier`, `caps` and `can`; `GET /plugins` and the
    workspace list carry `policy`.
  - **The kill switch.** `plugins/revoked.json` (entries: id, versions and/or `plugin.wasm`
    sha256s — neither is every version —, `hard` or `soft`, a reason) is embedded in every
    build; the live copy on the repository's main branch is fetched with the daily plugin
    check and counts only with Ed25519 signatures (`revoked.sig`) from `threshold` of the keys
    in `plugins/revocation-keys.txt` (none yet: until a maintainer adds one, only the embedded
    list counts). Each list carries a `serial` raised with every change, and a host refuses a
    list older than the one it holds, so an old signed list can't lift a later block. A list
    that newly blocks a loaded build drops its instances and cancels its jobs at once and logs
    a `blocked` entry; a soft block the user allowed (by the build's sha256) runs.
  - **Lock bumps.** The lock records each first-party plugin's `tier` and `caps`; the
    `plugin-lock` workflow auto-merges a bump only when the plugin is sandboxed and its release's
    capability lines match the pinned release's — otherwise the PR waits for a maintainer, and
    CI (which checks both against the release) stays red until they are set.

## Plugin screens, files and settings (the 0.2 platform)

- **What & when.** A plugin built for plugin API 0.2 can add to the app itself, not only to
  agents: screens drawn in Chimaera's own format (a tab, a dashboard panel, a file's view, a
  status chip, a section of its Extensions card), the kinds of files it opens (a `.tex` file
  opens in the LaTeX plugin's view, **Text** always one click away), items in a file's bar
  ("Export PDF"), published data core draws (problems in a file, a build's result), its own
  settings in Settings → Plugins, a folder for what it makes, and state that survives a
  restart. Shipped as the platform plan's phase P7; 0.1 plugins (Mycelium) run unchanged
  beside it.
- **How to use.** Switch the plugin on in a workspace. Its card gains **Open <view>** for each
  tab it offers, its card sections, and **Settings**. A file it claims opens in its view; the
  bar above has **Text** (and, when two plugins claim it, each one — the choice is remembered
  per workspace), its status chips and file actions. Opening such a file where the plugin is
  installed but off offers **Turn on** in that bar (**Not now** is remembered per workspace;
  one waiting for trust is left to its card). The dashboard shows its panels after
  Chimaera's own. Settings → **Plugins** lists every installed plugin's settings (host-wide or
  per workspace), with what its output folder uses and **Clear**.
- **Where it's wired.**
  - **Daemon** (`crates/chimaera-server/src/plugins/`): `runtime.rs` binds both WIT worlds
    (`v1`, `v2`) in one linker and picks by the manifest's `api`; `hostfns.rs` serves 0.1's
    `host` through 0.2's and the new `platform` imports; `platform.rs` (the manifest tables,
    their checks, the file patterns, the per-daemon `Platform`), `screens.rs` (the `ui/1`
    check, render / action / file-action / query routes, invalidation at ≤ 4 a second),
    `surfaces.rs` (`diagnostics/1`, `output/1`, `sourcemap/1`, `knowledge/1`, `references/1`), `output.rs`
    (output folders under the cache dir, the 1 GiB quota, Save to workspace), `pdata.rs`
    (durable state and setting values, capped JSON under `<data>/plugins/.data/`), `files.rs`
    (file events from every write the daemon knows of via `git::mark_path_dirty`, the save
    mark, per-file debounce, the watch sweep while a view is open, `settings-changed`).
    `switched-on` / `switched-off` are delivered from the switch route. `GET /git/diff?rev=`
    and `GET /git/log?path=` give the `diff` node its bases.
  - **Routes** (bearer-authed): `GET /workspaces/{id}/plugins/{pid}/views/{view}`,
    `POST …/views/{view}/actions`, `POST …/file-actions/{action}`, `GET …/query/{name}`,
    `GET …/output`, `POST …/output/save`, `GET /workspaces/{id}/surfaces/{kind}/{version}`,
    `GET`/`DELETE /plugins/{pid}/output`, `GET`/`PUT /plugins/{pid}/settings`. `/ws/events`
    carries `view`, `surface` and `plugin` frames, each only to windows on that workspace.
  - **UI** (`web-ui/src/lib/plugins/`): `platform.ts` (the wire, the pure matching, the
    fetchers, the frame bus), `ui/UiNode.svelte` (every node), `ui/PluginScreen.svelte` (one
    view: render, actions, built-in actions, re-render on `view` frames), `ui/PluginTab.svelte`
    (the `plugin` tab kind, `layout.ts`), `ui/PluginFileGate.svelte` (inside `FileView`),
    `PluginSettings.svelte` (the card and `settings/PluginsSettings.svelte`),
    `dashboard/PluginPanels.svelte`, `editorMarks.ts` (a plugin's `diagnostics/1` as gutter marks
    and underlines in the editor, errors and warnings only).
- **Rules.**
  - A screen is data: semantic props only (tone, size, icon names), so light, dark and the
    brand hold; markdown goes through chat's sanitizer; links open outside; images and files
    come from the workspace or the plugin's output folder only.
  - A tree the daemon's check refuses (size, node count, a missing label or alt) is not drawn:
    the view says so and lists each problem with its JSON path.
  - A plugin never writes into the workspace itself: **Save to workspace** is the user's click.
  - Nothing polls: a screen renders on open, on an action and on the plugin's `invalidate`; the
    watch sweep runs only while one of its views was open in the last 10 minutes.
  - Claiming a file kind is on the **Can** list, so a release that claims a new one asks again.
  - The author's guide has every node, prop, surface and limit:
    [docs/agent-guides/plugins.md](../agent-guides/plugins.md#the-platform-api-02).

## Programs and tools (the privileged tier)

- **What & when.** A 0.2 plugin may run programs on your computer (a LaTeX build, a
  formatter) and download the ones it needs. It names each one in its manifest; nothing else
  can run. The host runs them as **jobs** under fixed limits, and downloads a **tool** only
  when you click Install. The locked first-party LaTeX and Typst plugins use this tier
  for document compilation, diagnostics, and agent compilation tools.
- **How to use.** Such a plugin's card says **runs programs** and lists every program under
  **Can** ("Runs latexmk"; a shell gets "Runs sh: this plugin can run any command on this
  host"; a program that reaches the network says so: "tlmgr uses the network: CTAN mirrors"). Before install, **Downloads** says what its tools would fetch and from where ("TeX
  Live 2026.09 · 152 MB from github.com"). Once installed, its **Tools** section shows each tool
  with **Install**, **Update** (the plugin names a newer version) and **Remove**. A program you
  already have wins over the plugin's copy unless the plugin offers a setting to prefer its own.
- **Where it's wired.**
  - **Daemon** (`crates/chimaera-server/src/plugins/`): `jobs.rs` (the queue, the limits, the
    POSIX `sh` preamble that sets them, the captured login environment, process-group kills,
    logs in the output folder, `job-finished`, the `job` frame, an agent tool's wait) and
    `toolchain.rs` (the https download with its streamed sha256 and size cap, the safe
    unpacker, setup steps, the `current` link, Install / Update / Remove); `platform.rs`
    validates `[[programs]]` and `[[tools]]`; `capabilities.rs` makes them `program` and
    `download` atoms (privileged); `runtime.rs` holds a tool call that answered `wait` and asks
    `tool_resume`; switching the plugin off or a block cancels its jobs.
  - **Routes** (bearer-authed): `GET /plugins/{pid}/tools`, `POST
    /plugins/{pid}/tools/{tool}/install`, `DELETE /plugins/{pid}/tools/{tool}`, `GET` / `DELETE
    /workspaces/{id}/jobs/{job}`. `/ws/events` carries `job` frames to that workspace's windows.
  - **UI**: `PluginTools.svelte` (the Tools section, with a progress bar while one
    installs), the card's Downloads line (`platform.ts`'s `downloadWords`), and a
    screen's `install-tool` button (the same install, its progress at the top of the
    screen).
- **Rules.**
  - **Only declared programs, by name.** No path to a binary, no shell between the plugin and
    the program; a URL that moves (`/latest/`) doesn't validate.
  - **Limits for every job:** 2 running on the host, 1 per plugin, 8 waiting; 60 s by default,
    600 s at most, then the whole process group is stopped; 4 GiB of memory, 256 MiB per
    written file, low CPU and I/O priority; stdout and stderr to 16 MiB logs in the output
    folder, never in the daemon's memory; the host's own variables (`PATH`, `HOME`, `LD_*`, …)
    can't be set.
  - **A tool is checked before it counts:** https only, the declared size and sha256 while it
    streams; unpacked into an empty folder that refuses `..`, absolute paths, hard links,
    devices and links that leave it; nothing outside `~/.chimaera/tools/<plugin>/<tool>/`
    changes (no PATH or rc edits). Two versions stay. Removing the plugin removes its tools.
  - **Where tools go:** `~/.chimaera/tools/` unless Settings → Extensions → **Plugin Tools
    Folder** (`plugins.toolsDir`, `~` and `$VARIABLES` expanded) names another — on a cluster
    whose home is a small quota, `$SCRATCH` or a group folder. An install first checks the
    folder has room for the download and about three times it unpacked, with 1 GB to spare,
    and otherwise says so and points at the setting (a cluster home at 880 MB free would
    otherwise have taken a 415 MB TeX Live). Tools already installed stay where they were.
  - **The lock covers downloads too:** the lock pins the manifest and the manifest pins each
    download's sha256, and the lock bump fetches every download and compares.
  - **The honest limit:** a program can do whatever its arguments allow. That is why programs
    make a plugin privileged and why its card names each one.
  - A switched-on plugin is compiled in the background when the daemon starts, when
    it is installed or updated, and when it is switched on, so opening a file never
    waits on it.
  - Every job and install is in the plugin's **Activity** log. The author's side:
    [docs/agent-guides/plugins.md](../agent-guides/plugins.md#programs-jobs-and-tools).

## Agent plugins & the Skills view

- **What & when.** "What can my agents do here?" — answered by asking each agent CLI, never by
  re-deriving its discovery rules.
- **How it's used.** The Plugins segment's agent section lists each CLI's plugins: claude from
  `claude plugin list --json` (id, version, scope, enabled) plus `claude plugin details` totals
  (skills, hooks, always-on tokens — for the first 12); codex from a short-lived
  `codex app-server` (`skills/list`, `hooks/list` for the workspace cwd — no thread, no model
  call), a codex plugin being whatever `pluginId` codex attributes skills and hooks to. The
  **Skills** segment keeps different source files separate, including equal skill names, grouped by origin — this project
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
  The controls use a compact agent selector (including All agents), a search field,
  and a count of the matching skills — and a row opens in place into a small definition list: **Use it** (each
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

### Antigravity and Grok Build

Both are core agents in Plugins, Connections and Skills. Agent identity and action lists are
open-ended on these reports; the UI never assumes an unknown agent is Codex. This does not
add third-party agent registration to the current Wasm plugin API.

- **Grok:** discovery uses `grok inspect --json`, including Claude-compatible add-ons that
  `grok plugin list` and `grok mcp list` omit. Project trust remains Grok's decision. Its
  `/plugins` and `/mcps` menus open through **Manage in Grok Build**. Installation runs the
  native CLI in a visible terminal, asking **Install and trust?** before passing `--trust`.
  Project/compatible plugins do not receive installation-only action buttons.
- **Antigravity:** `agy plugin list` reports imported packages. The native zero-model-turn
  `/skills` print command supplies loaded skills and their source/plugin identity (CLI
  1.2.14 or newer; unknown/older versions are not prompted during discovery). A package's
  presence alone does not prove enablement: the UI says **installed** until a loaded skill
  confirms it. User and project copies remain separate. Install/enable/disable use native
  CLI commands. **Command help** opens CLI help: `/plugins` is *not* a native menu in 1.2.14
  and must never be sent as a management prompt.
- **Connections:** Antigravity's list covers CLI-managed connections, not every project or
  plugin server; the page says so. Grok combines its native MCP list with effective inspect
  results. Neither adapter fabricates OAuth status or a `mcp login` flow; Grok sign-in uses
  its native menu. Reports expose presentation fields, never endpoints, headers or secrets.
- **Skill use:** invocation comes from the provider's report. Non-invocable skills have no
  copy-command action; a skill's `allowed-tools` declaration is not translated into a common
  permission grant. Hooks and trust remain provider-owned, not converted between formats.

The adapters live in `agent_probe/extensions.rs`; fixed-argument user actions live in
`agent_probe/actions.rs`. A failed skill inventory offers **Try again**, which clears
cached inventories and rechecks any missing CLI version without changing the chosen
installation. Native command arguments are defined in
`launcher::extension_action`. The bearer-authenticated
`POST /workspaces/{id}/agent-extensions/action {agent,action,target?}` opens a retained PTY,
keeps the native command's outcome visible, and invalidates discovery on completion or exit.
The shared gate permits one probe process at a time; parallel report requests share a
60-second deadline including queue time. Per-command output and row caps remain in force.

Workbench manifests can require or recommend an agent-side package with an additive
`source` (local folder or repository accepted by that CLI) for `agy` or `grok`; the existing
Claude/Codex `marketplace` contract is unchanged. The server advertises whether installation
is supported. Unknown enablement blocks setup rather than claiming the package is ready.


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

- **What & when.** Agent notes was a first-party workbench plugin (`agent-notes`) for agents
  leaving each other notes. Its job is built into Chimaera now: Agent communication (Settings →
  Agents). It left `plugins/plugins.lock`, so nothing offers it, and its id is on the daemon's
  retired list.
- **How it's used.** Nothing to do but remove it. A copy an older daemon installed stays on its
  card with the reason as its fault — "Built into Chimaera now: Agent communication (Settings →
  Agents). Remove this copy." — and the card's **Remove**; it never runs (no tools, no hook, no
  update offer, no trust question) and its switch refuses on. Every way in answers 409 in the
  same words: Install by id (`chimaera plugin add agent-notes`), by its repository (before
  anything is fetched), a release or local build whose manifest says `agent-notes` (Preview
  shows the reason as the fault), and Update, Use previous and Check of a kept copy. Its switch
  is dropped from every workspace's `plugins_on` when the daemon loads `workspaces.json`
  (written once). Old notes stay on the Timeline.
- **Where it lives.** `plugins/retired.rs` (`RETIRED`: id, repository, the reason), read by
  `plugins::resolve` (the fault), `put_workspace_plugin`, `installed.rs` (every install route,
  update, rollback), `releases::check`, `preview::describe` and `WorkspaceStore::load`. Tests:
  `tests/plugin_retired.rs`, `workspaces::tests`.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Connections — scope and placement
_Captured 2026-09-30 from the maintainer's instructions in this chat._

- **Problem it solves:** “We have the extensions page and I feel like those should
  be there too so it is easy to know connectors and plugins, auth them etc.”
- **Placement:** “connections should be second in the tab (more rarely used too)”;
  Plugins includes Chimaera plugins and remains first.
- **Deferred:** “I will fix the codex / claude auth flow after the PRO service is
  done and merged.” This change covers connectors, not agent account sign-in.
- **Sign-in UX:** after trying the first implementation, the maintainer asked, “do we
  really want that flow to go through the terminal ?” and chose “Build the in-app MCP
  sign-in dialog now.”
- **How settled it is:** no additional permanence constraints captured.

### Plugins, Skills, hook trust & Agent notes — why it exists
_Captured 2026-09-25 (from the maintainer, via capture-feature-intent)._

- **Problem it solves:** all four of the maintainer's triggers — the Mastermind was "sometimes superfluous, not really always doing anything" and the dashboard "doesn't say much and feels redundant"; Chimaera should know what is going on in each project — fully mycelium-compatible with good UI on top, yet still working a little without it; agents across vendors (claude ↔ codex, even subagents) should be able to talk to each other; and attaching mycelium should be quick and plugin-like, generic enough for LaTeX and other harnesses later.
- **How settled it is (intended vs provisional):** only the *why* is settled. The manifest shape, contribution points, the Installed/Skills layout, the attach sheet and how notes are delivered are how it works for now.
- **Deliberately open / where it may go:** all left open for later, not ruled out: a Browse/marketplace for skills and plugins; more workbench plugins (LaTeX, built by another agent; the specified-but-unbuilt views/settings/commands contribution points); a Chimaera-side knowledge store (today, without mycelium, Knowledge shows guidance files and Claude memory only); and agents directing each other beyond informational notes (subagents addressable).
- **Do not change (or: open to change):** open to change — an addition to the core, not a core bet. Offered four candidates to freeze (nothing changes for agents unless a plugin is on; Chimaera never curates knowledge; notes never start a turn; hook trust is never silent), the maintainer answered "all can change". They are how it was built today, not locked contracts.

The design's maintainer decisions (2026-09-25) are in the
[plan](../design/timeline-knowledge-plugins-plan.md#decisions-maintainer-2026-09-25).

### WASM plugins, versions & updates — why it exists
_Intent for the WASM plugin system: pending capture._

### The plugin platform: trust, screens, programs and tools, LaTeX and Typst — why it exists
_Captured 2026-09-29 (from the maintainer, via capture-feature-intent)._

- **Problem it solves:** "Just to become a platform and extendable, without missing chimaera's
  core principles."
- **How settled it is:** an addition to the core, not a core bet — "open to change". The
  platform's shape (the `ui/1` format, the surfaces, the job limits, the trust prompt) is how it
  works today.
- **Do not change (or: open to change):** open to change, as long as it keeps chimaera's core
  principles (the plan's [principles](../design/plugin-platform-plan.md#principles) are how it holds
  them today).

The design decisions of 2026-09-29 are recorded in the
[plugin platform plan](../design/plugin-platform-plan.md#decisions-maintainer-2026-09-29) and the
[LaTeX and Typst plan](../design/latex-reports-plan.md#decisions).
