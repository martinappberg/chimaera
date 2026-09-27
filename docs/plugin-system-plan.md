# The plugin system: WASM plugins on a small host

Dated 2026-09-26. A plan with a proof of concept attached: the design below is
proven by porting the two workbench plugins that exist today (Agent notes,
Mycelium) to run as WASM components before anything new is built on it. Read
this first when touching `crates/chimaera-plugin-api`, `crates/chimaera-server/src/plugins/`
or `plugins/`. The earlier plugin seam (manifests embedded as data, behaviour as
named first-party code) is in [timeline-knowledge-plugins-plan.md §6](timeline-knowledge-plugins-plan.md);
this plan replaces its "named built-in" column. The LaTeX and Typst plugins are
the first new plugins on it ([latex-reports-plan.md](latex-reports-plan.md)).

## Status (2026-09-27)

P1 to P3 are built, P4's live proof passed, the first-party plugins live in
their own repositories, and (2026-09-27, decision 6) the daemon ships only the
lock that names them, never their bytes: every plugin, first-party or not,
installs from its release or from a directory. The feature page
([plugins](features/plugins.md)), the authoring guide
([agent-guides/plugins.md](agent-guides/plugins.md)) and the maps
(`crates/chimaera-plugin-api/AGENTS.md`, `plugins/AGENTS.md`, the server map)
describe what shipped; the phases below remain the design record.

- **Shipped:**
  - P1 (`e2873f4`): `chimaera-plugin-api` (the WIT world `chimaera:plugin@0.1.0`
    and the Rust bindings), the wasmtime host (`plugins/runtime.rs`,
    `plugins/hostfns.rs`), `scripts/build-plugins.sh` and the CI step, and
    Agent notes as the first WASM plugin, with `notes.rs` reduced to core.
  - P2 (`79d2e35`): Mycelium as a WASM plugin; `mycelium.rs` deleted;
    `knowledge.rs` asks the provider through its `knowledge` export.
  - P3 (`55acf73`): `version`, `api`, `requires.chimaera`, `[release]` and
    `provides.events` in the manifest; the catalog with the precedence rule
    and the gates; the installed directory with `current` and `previous`
    links; install, update, Use previous and Remove with checksum-verified
    downloads; the release checker and Check now;
    `chimaera plugin list|add|update|remove`; the card. Also
    `macos_use_mach_ports(false)` (the P0 facts below say why).
  - P4: the maps, the feature page, the authoring guide and this plan; the
    live proof.
  - The plugins' own repositories (P5, first slice):
    [martinappberg/chimaera-plugin-agent-notes](https://github.com/martinappberg/chimaera-plugin-agent-notes)
    and [martinappberg/chimaera-plugin-mycelium](https://github.com/martinappberg/chimaera-plugin-mycelium),
    each a standalone crate (`chimaera-plugin-api` as a git dependency pinned
    to a chimaera commit) whose release workflow publishes `plugin.wasm`,
    `plugin.toml` and `SHA256SUMS` on a `v<version>` tag, and whose manifest
    names its repository as `[release] github`. The sources left this
    repository: `plugins/plugins.lock` pins one release of each (id,
    version, repo, both sha256s), and `scripts/build-plugins.sh` downloads,
    verifies and lays those out in `plugins/dist` (cached under
    `plugins/cache/`, re-verified every run; `plugins/plugins.local.toml`
    builds a local checkout instead). The `plugins/` workspace keeps only the
    test fixture. The release checker now asks every plugin whose manifest
    names `[release]`, embedded ones included, against the version that
    runs, and Update of a plugin that only ships with chimaera installs the
    newer release as its installed copy, which the precedence rule loads
    (Remove goes back to the embedded one; removing that stays refused).
    (Embedding, `plugins/dist`, the cache, the local override and the
    precedence rule are gone since the next bullet.)
  - The curated list, not the bytes (2026-09-27, decisions 6 and 7): the
    daemon embeds only `plugins/plugins.lock` (which gained `name` and
    `summary`), and the catalog is the lock's entries, listed `available`
    while nothing is installed, plus the installed copies. A first-party
    plugin installs from its repository at the pinned version
    (`POST /plugins/{pid}/install`, or `POST /plugins/install {github}` naming
    the lock's repository), verified against the release's `SHA256SUMS` and
    the lock; a local build installs from a directory
    (`POST /plugins/install {path}`, `chimaera plugin add --path`), replacing
    `plugins/plugins.local.toml`. Each copy keeps its release's `SHA256SUMS`
    beside its files, re-hashed at every load (`verified`; a mismatch is a
    fault and the copy never loads); `first_party` is an id in the lock whose
    copy names the lock's repository. `embedded_version`, `installed_version`,
    `stale` and the `embedded` source left the wire; `installed`,
    `first_party`, `verified`, `sha256_wasm`, `repo`, `pinned_version` and
    `local_path` joined it. Remove of a first-party plugin lists it as
    available again. `[recommends.agent_plugins.<agent>]` joined the manifest
    beside `requires`. `scripts/build-plugins.sh` is test-only (the fixture
    and the locked releases into `plugins/dist-test`); the release jobs, the
    musl CI job and the Tauri build no longer run it. The user-facing tab is
    **Extensions** (Plugins · Skills · Browse).
  - The Extensions tab's design pass (2026-09-27): the manifest gained
    optional `description` and `[requires]` / `[recommends]` `summary` (on the
    wire as `description`, `requires_summary`, `recommends_summary`); each card
    has one primary control (Install, or the switch with its state in words),
    everything secondary in a "…" menu (Check for updates — was "Check now" —,
    Use previous, Set up, Open on GitHub, Remove), the author's description,
    a For you / For agents / Here list, an **Agent-side plugin** box with one
    row per installed agent, an update callout and a fault callout with
    **Reinstall** (the daemon now replaces a same-version copy whose files no
    longer match its release). Fault and download refusals speak plain words,
    the hashes left to the log; `chimaera plugin list` marks first-party
    plugins with `✓`, and add / update print one line. The Skills segment
    splits the built-ins per agent; the glyph is drawn for the pixel grid
    (12 px, one definition for the tab, the rail and quick-open); the
    dashboard's quiet state says what Agent notes and Mycelium do, linking
    Extensions. agent-notes 0.1.2 in the lock (its own wording for the
    instruction paragraph; the daemon test now checks it names both tools).
- **Proven live** on the isolated debug daemon, with a fake claude binary
  standing in for the agent so nothing was billed (the 2026-09-26 runs below
  predate decision 6, so their first-party plugins were still embedded):
  - Agent notes: the catalog listed both plugins from `plugins/dist`; a
    session spawned with the plugin on had `mcp__chimaera__post_note` and
    `read_notes` pre-allowed in its generated settings, and its `initialize`
    instructions carried the byte-identical Agent notes paragraph (2,130
    characters with the plugin on, 1,739 off). `post_note` from session A and
    `read_notes` from session B returned the note quoted; a second read said
    "No new notes for you."; B posted to A by session id; a target in another
    workspace was refused with the old wording; both notes reached the
    Timeline attributed to their sessions; the `UserPromptSubmit` hook
    answered "1 unread note from other sessions in this workspace —
    read_notes shows it.". Switching the plugin off removed both tools and the
    paragraph, a `post_note` call answered -32602 "isn't switched on" and the
    hook answer became `{}`; switching it on again kept the read cursor.
  - Mycelium: a workspace holding a copy of the test fixture's `.living/`
    answered `provider: null` before the switch and `"mycelium"` after, with
    counts findings 4 · decisions 4 · learnings 4 · open 6, the four
    confidence levels, the handoff and the legacy-heading warning; a
    session's `tools/list` carried `knowledge_search` and `knowledge_get`,
    both returned the pinned text, and the instructions carried the Mycelium
    paragraph.
  - Versions and updates, against a local static server standing in for
    GitHub (`CHIMAERA_PLUGIN_RELEASES_API`) that published the test fixture
    as `acme/fixture`: `chimaera plugin add acme/fixture` installed 0.1.0 and
    printed both sha256s, laid out as `<data>/plugins/test-fixture/0.1.0/` with
    `current -> 0.1.0`, listed as installed beside the two embedded plugins; a
    session with it on listed its 8 tools. With v0.2.0 published, Check now
    offered it, Update reported 0.2.0 (previous 0.1.0) with the verified
    sha256, the links moved, and the **same** session's next `tools/list`
    carried 9 tools, the new `version` tool answering 0.2.0. Use previous
    went back to 0.1.0 (8 tools again). `chimaera plugin remove agent-notes`
    was refused ("ships with chimaera and has no installed copy to remove");
    removing the fixture deleted its directory and the session's plugin tools.
  - Not run live: the fault path (a panicking plugin), which
    `src/tests/plugin_host.rs` covers.
  - Proven live (2026-09-27), on a scratch debug daemon (its own
    `CHIMAERA_HOME`) against the real GitHub releases: `GET /plugins` listed
    Agent notes and Mycelium as `available` with `pinned_version` 0.1.0 and
    no bytes on disk; `POST /plugins/agent-notes/install` fetched v0.1.0 by
    direct download and answered `installed`, `first_party` and `verified`,
    `sha256_wasm` the lock's `fd75d0ef…`, with `plugin.toml`, `plugin.wasm`
    and the release's `SHA256SUMS` under `plugins/agent-notes/0.1.0/`;
    switching Mycelium on answered 409 "Mycelium isn't installed — install it
    first" and Agent notes 200 (active); Check now offered 0.1.1 and Update
    moved `current` to 0.1.1 (`previous` 0.1.0), still `first_party` and
    `verified` with `pinned_version` 0.1.0; `chimaera plugin list` printed
    "chimaera · verified · installed · previous 0.1.0 · chimaera pins 0.1.0"
    and "available · chimaera pins 0.1.0 · install with: chimaera plugin add
    mycelium"; `chimaera plugin add --path plugins/dist-test/mycelium`
    installed it `chimaera · verified`; in the Extensions tab (light and
    dark) Remove listed Agent notes as available with **Install 0.1.0**, and
    Install brought it back verified and active again in the workspace
    whose switch was left on.
- **Measured:** the release binary 38.4 MB against 26.8 MB before (Cranelift);
  Agent notes' `plugin.wasm` 135 KB, a release-grade compile of it 55 to
  82 ms; Mycelium's 305 KB, its first `knowledge` ask 202 to 209 ms in a
  release test build, and 6.7 s on the debug daemon (debug Cranelift) with the
  next ask there 10 ms; a warm tool call through the MCP endpoint 1 to 2 ms
  (P1) and under 10 ms in the live proof. Release daemon RSS: 5.8 MB idle,
  10.2 MB with a workspace and a session, 28.8 MB after the first plugin call
  (compile + instantiate, 70 ms end to end), 28.9 MB after 50 more calls. The
  debug daemon stayed at 29 to 64 MB through the P3 run.
- **Later (P5):** the Browse view; `exec` and `watch` (WIT 0.2) and the LaTeX
  plan's `build` point on them; the UI-facing `query` route; delivering
  `switched-on` / `switched-off`; a UI that reads `emit` frames; and, only if
  cold compile or binary size hurts on a real login node (not yet measured on
  one), an on-disk `.cwasm` cache or the runtime-only host. (Adding a
  plugin from a repository shipped: the Plugins segment's **Install from a
  repository** row calls `POST /plugins/install`.)

## Decisions (maintainer, 2026-09-26 and 2026-09-27)

1. **Plugins are Rust, compiled to WASM, in their own repositories.** Not scripts
   in another language, not an extension host running native code, not a crate
   compiled into the daemon. The chimaera repository keeps a small host and a
   published API crate; every plugin is a separate crate that builds to one
   portable `plugin.wasm`.
2. **The core stays small and generic.** It owns the runner, the limits, the
   viewers and the data shapes. A plugin contributes behaviour through a pinned
   interface and never touches the daemon's internals. The 3,752-line mycelium
   reader leaves the daemon.
3. **Prove it on what exists first.** Agent notes, then Mycelium, run as WASM
   components through the new host before LaTeX is started. If the port shows
   the model does not hold on a login node, the model changes, not the plugins.
4. ~~**First-party plugins ship inside the binary** as embedded `.wasm` files, so
   one static daemon over ssh still carries everything and nothing is downloaded
   to a host by surprise. Third-party plugins install into `~/.chimaera/plugins/`
   later, through the same host and limits.~~ **Reversed on 2026-09-27 by
   decision 6.**
5. **Versions and updates are baked in.** Every plugin has a version and an API
   version, an installed plugin can be updated (and rolled back) on its own
   cadence with a visible, checksum-verified download, and the daemon says on
   the card which version is running and where it came from
   ([Versions and updates](#versions-and-updates)).

Added on 2026-09-27:

6. **The daemon ships the curated list, not the bytes** (reverses decision 4).
   The binary embeds `plugins/plugins.lock`, which names the first-party
   plugins, the release of each to install and that release's sha256s, and no
   plugin's `.wasm`. First-party plugins install from their own repositories at
   the pinned version, like any plugin, on the user's click, with double
   verification: the release's `SHA256SUMS` and the lock. Why: the plugins live
   in their own repositories, and the card must say so honestly. A host with
   no network installs from a directory (`chimaera plugin add --path`).
7. **`recommends` beside `requires`, and the surface is called Extensions.**
   `[recommends.agent_plugins.<agent>]` names an agent-side plugin that makes a
   workbench plugin more useful for the agents the user runs but is never
   needed for it to work (mycelium's: the Knowledge reader works with no agent
   plugin); `requires` stays for a genuine hard requirement. The tab users see
   is **Extensions**, with the segments Plugins · Skills · Browse; ids, the
   wire, the store and the code keep "plugins".

## The short version

- **One artifact per plugin, portable.** A plugin is `plugin.wasm` (a WebAssembly
  component built from Rust for `wasm32-wasip2`) plus `plugin.toml` (today's
  manifest). The same file runs on a macOS laptop, an x86 login node and an ARM
  box: no per-platform builds, one install per host.
- **The interface is a WIT world**, `chimaera:plugin`, versioned, published from
  `crates/chimaera-plugin-api` together with the Rust bindings a plugin author
  uses. It is the third public interface Chimaera pins, beside the daemon↔UI
  wire and the agent protocols.
- **The host mediates everything.** A plugin cannot open a file, start a process,
  touch the network or the daemon's state. It asks the host, and every host
  function is bounded: capped reads, workspace-relative paths with symlinks
  refused, rate-capped Timeline writes, capped state. Limits live in the host, so
  a plugin cannot forget them.
- **The sandbox is the runtime.** wasmtime with epoch interruption (a runaway
  loop is stopped at its deadline), a memory cap per instance, WASI with no
  capabilities granted, and a trap that costs the plugin its instance, never the
  daemon.
- **Nothing changes for an agent unless a plugin is on**, exactly as today: the
  `agent_view` fixtures stay byte-identical, the tools and the instruction
  paragraph join a session only where the plugin is active.

## What a plugin is

```
chimaera-plugin-agent-notes/    (its own repository, one crate)
  Cargo.toml                    crate-type = ["cdylib"], depends on chimaera-plugin-api
  plugin.toml                   the manifest: id, name, summary, provides, adds …
  src/lib.rs                    impl chimaera_plugin_api::Plugin for AgentNotes { … }
                                chimaera_plugin_api::export!(AgentNotes);
  README.md · skills/ · .claude-plugin/ · .codex-plugin/   (agent-side assets, optional)
```

Built with `cargo build --target wasm32-wasip2 --release`, the crate yields a
component, which the repository's release publishes (`plugin.wasm`,
`plugin.toml`, `SHA256SUMS`, tag `v<version>`). The daemon carries none of it:
it embeds `plugins/plugins.lock` (the first-party plugins, the release of each
it installs and that release's sha256s) and installs any plugin on the user's
click into `~/.chimaera/plugins/<id>/<version>/`, from its release or, for a
build in development, from a directory (`chimaera plugin add --path <dir>`;
the same version again replaces it, so the next `tools/list` is the new
build). `scripts/build-plugins.sh` (also `just plugins`) is test-only: it
builds the host's test fixture into `plugins/dist-test/` and downloads the
locked releases there, verified against the lock, for the tests that install
them by path; building or running the daemon doesn't need it.

The manifest keeps its format (`plugins/mod.rs` parses it with
`deny_unknown_fields`) and gains `api = "0.1"`, the WIT version the component
targets, plus the fields of [Versions and updates](#versions-and-updates):
`version` (required, like `api`), `requires.chimaera`, `[release] github`,
and `provides.events` (the `on-event` variants the host delivers to it; none
by default). The host refuses a component whose `provides.mcp_tools` does not
match the names its `tools()` export returns: the card's Adds line and the tool
gate come from the manifest, so the two must agree.

## The interface (WIT)

`crates/chimaera-plugin-api/wit/chimaera.wit`, package `chimaera:plugin@0.1.0`.
Open-ended payloads (tool arguments and results, Timeline entries, the Knowledge
snapshot, events to the UI) are JSON text: they already have JSON shapes on the
daemon↔UI wire, and adding a field to JSON never breaks the ABI. Small fixed
things are typed records.

```wit
package chimaera:plugin@0.1.0;

interface types {
    /// A JSON document as text (serde on both sides).
    type json = string;

    /// Who is asking. Every call carries one.
    record context {
        /// The workspace id the call is for.
        workspace: string,
        /// The calling agent session (tool calls, hook events); absent for
        /// workspace-level asks (knowledge, queries from the UI).
        session: option<string>,
        /// That session is the workspace's Mastermind.
        mastermind: bool,
    }

    record tool-def { name: string, description: string, input-schema: json }
    record tool-result { text: string, is-error: bool }

    record stat { size: u64, mtime-ms: u64, is-dir: bool }
    record entry { name: string, is-dir: bool, is-symlink: bool }

    record session { id: string, kind: string, name: string, chat: bool, alive: bool, mastermind: bool }

    /// Knowledge: a stamp that changes when the source files do, and the
    /// snapshot in the fixed shape the Knowledge view reads.
    record snapshot { stamp: json, data: json }

    variant event {
        /// A hook the agent already fires (SessionStart, UserPromptSubmit);
        /// the plugin may answer with one line of context for the agent.
        hook(hook),
        session-ended(string),
        switched-on,
        switched-off,
    }
    record hook { session: string, name: string }

    enum level { debug, info, warn, error }
}

/// What a plugin may ask the host. Every function is bounded by the host.
interface host {
    use types.{context, stat as file-stat, entry, session, json, level};

    /// Workspace files. Paths are workspace-relative; no component may be a
    /// symlink; reads stop at `cap` bytes; listings at `cap` entries. All of
    /// it runs off the reactor behind the filesystem semaphore.
    read: func(cx: context, path: string, cap: u32) -> result<list<u8>, string>;
    stat: func(cx: context, path: string) -> result<file-stat, string>;
    %list: func(cx: context, path: string, cap: u32) -> result<list<entry>, string>;

    /// Small per-(plugin, workspace) state the host keeps for the plugin
    /// (cursors, caches): 64 KiB per plugin per workspace, in memory.
    state-get: func(cx: context, key: string) -> option<json>;
    state-put: func(cx: context, key: string, value: json) -> result<_, string>;

    /// The workspace's sessions, and one session's display name.
    sessions: func(cx: context) -> list<session>;

    /// The Timeline. `entry` is the wire shape of an entry; the host accepts
    /// the kinds a plugin may write (`note` today), fills seq/ts/name, and
    /// rate-caps appends per session (10 a minute). `recent` returns the
    /// newest entries of the given kinds, newest first, at most `limit`.
    timeline-append: func(cx: context, entry: json) -> result<u64, string>;
    timeline-recent: func(cx: context, kinds: list<string>, limit: u32) -> list<json>;

    /// A frame on /ws/events: {"type":"plugin","plugin":<id>, ...event}.
    emit: func(cx: context, event: json);

    now-ms: func() -> u64;
    log: func(level: level, message: string);
}

/// What a plugin provides. Every export is optional in effect: a plugin
/// without tools returns an empty list, one without knowledge returns none.
interface plugin {
    use types.{context, tool-def, tool-result, snapshot, event, json};

    /// The MCP tools offered in workspaces where the plugin is on. Names
    /// must equal the manifest's provides.mcp_tools.
    tools: func() -> list<tool-def>;
    /// The paragraph appended to the agents' MCP instructions where on.
    instructions: func() -> option<string>;
    call-tool: func(cx: context, name: string, args: json) -> tool-result;

    /// The Knowledge snapshot, or none when `known` is still current.
    knowledge: func(cx: context, known: option<json>) -> result<option<snapshot>, string>;

    /// Reads the UI makes (GET /workspaces/{id}/plugins/{pid}/query/{name}).
    query: func(cx: context, name: string, args: json) -> result<json, string>;

    /// Something happened. A hook event may return one line the host adds to
    /// the agent's hook context (the "N unread notes" hint).
    on-event: func(cx: context, event: event) -> option<string>;
}

world chimaera-plugin {
    import host;
    export plugin;
}
```

Two WIT spellings to know: `%list` escapes a keyword (the function is
`list`), and a type and a function may not share a name inside one interface,
hence `stat as file-stat` in `host`.

What is deliberately not in 0.1: **`exec`** (a bounded child process on the
host, the heart of the LaTeX plan's `build` point) and **`watch`** (ask the host
to report file changes). Both are additive: a host may offer more imports than a
component uses, so 0.2 adds them without breaking 0.1 plugins. They land with the
first plugin that needs them. Removing or changing an export is the breaking
direction; the exports above are the complete set a plugin must provide.

### The Rust side of it

`chimaera-plugin-api` wraps the generated bindings so a plugin author writes
plain Rust:

```rust
use chimaera_plugin_api::{host, Context, Plugin, ToolDef, ToolResult, Snapshot, Event};

struct AgentNotes;

impl Plugin for AgentNotes {
    fn tools() -> Vec<ToolDef> { vec![ToolDef::new("post_note", "…", schema!{…}), …] }
    fn instructions() -> Option<String> { Some("Agent notes (a plugin the user switched on): …".into()) }
    fn call_tool(cx: Context, name: &str, args: serde_json::Value) -> ToolResult {
        match name {
            "post_note" => post(&cx, args),
            "read_notes" => read(&cx, args),
            _ => ToolResult::error("unknown tool"),
        }
    }
    fn on_event(cx: Context, ev: Event) -> Option<String> { … }
}

chimaera_plugin_api::export!(AgentNotes);
```

Host calls look like `host::read(&cx, ".living/INDEX.md", 256 * 1024)?` and
return `Result`. The crate compiles natively too, with every host import a stub
that aborts the test binary, so a plugin's pure logic (parsers, addressing
rules) is unit-tested with plain `cargo test`, never reaching a host call, and
only the integration runs in the daemon's tests.

## The host

`crates/chimaera-server/src/plugins/` becomes the host and nothing else:

| File | What |
|---|---|
| `mod.rs` | the manifest, the embedded `plugins/plugins.lock`, the catalog (the lock's entries listed as available, plus the installed copies under `~/.chimaera/plugins/`) and the gates, per-workspace on/off, detect, routes |
| `runtime.rs` | one `wasmtime::Engine` (Cranelift, async, epoch interruption); each build compiled once and kept in memory (the last two per plugin, by SHA-256); per-(plugin, workspace) instances created on first use, dropped after 10 min idle or when the switch flips; one call at a time per instance; per-call deadlines; the fault counter; events delivered only to plugins that declare them |
| `hostfns.rs` | the `host` interface: bounded fs behind `fs::FILESYSTEM_WORK` on a blocking thread, state (capped), sessions, Timeline append (allowed kinds, rate cap) and recent, emit, log |
| `tools.rs` | generic: `owner(tool)` from the manifests as before; `offered` and `call` go to the plugin's exports through the runtime |
| `installed.rs` | the installed directory, its `current` and `previous` links, the `SHA256SUMS` check when a copy is read, and the install (release, first-party, local directory), update, rollback and remove routes (P3; 2026-09-27) |
| `releases.rs` | the release checker and Check now (P3) |

Limits, all in the host:

| Limit | Value | How |
|---|---|---|
| Time per call | 5 s for tools, queries and events; 30 s for knowledge | epoch interruption: a 100 ms ticker, a wall-clock deadline checked on each tick, then a trap; an outer tokio timeout (2 s past the budget) abandons a call stuck in a host function on a slow filesystem |
| Memory per instance | 64 MiB linear memory | `StoreLimits` |
| Compiled code | once per build per daemon lifetime, in memory, the last two builds per plugin (25 to 50 ms for the spike's 58 KB guest; 55 to 82 ms for Agent notes and about 200 ms for Mycelium, release) | `runtime.rs`; an on-disk `.cwasm` (`Component::serialize`) is later (P5) if a login node's cold compile hurts; wasmtime's own `cache` feature stays out |
| Virtual memory | 64 MiB reserved per instance, not wasmtime's 4 GiB default | `memory_reservation`, `memory_guard_size`, `memory_reservation_for_growth` |
| WASI | nothing granted: no files, no env, no args, no network; the guest's stderr captured (4 KiB) and logged on a trap | `WasiCtxBuilder::new()` |
| Reads | `cap` bytes per read, 8 MiB ceiling; 4,096 entries per list | `hostfns.rs` |
| State | 64 KiB per plugin per workspace | `hostfns.rs` |
| Timeline appends | 10 per session per minute (the window `tell_mastermind` shares); `note` entries only; text ≤ 2 KiB | `hostfns.rs` |
| What a plugin returns | a tool result 256 KiB, the instruction paragraph 8 KiB, a hook line 1 KiB (cut past that) | `runtime.rs` |
| `emit`, `log` | one JSON object ≤ 16 KiB a frame, 64 frames kept; 64 log lines a call, 2 KiB each | `hostfns.rs`, `runtime.rs` |
| Faults | a trap drops the instance; 5 traps in a minute mark the plugin faulted for the workspace (card shows why) until switched off and on | `runtime.rs` |
| Instances | at most 64 live instances daemon-wide; least recently used dropped | `runtime.rs` |

A call into a plugin never blocks a reactor thread: wasmtime runs the guest on
the calling task with async host functions, so a `read` suspends the guest while
the blocking thread pool does the stat. Instances are created lazily and the
catalog is read from the manifests, so a daemon whose user never switches a
plugin on never instantiates one.

## What moves where

**Agent notes.** `post_note`, `read_notes`, the addressing rule, unread and the
hook hint move to `plugins/agent-notes`. Cursors are host state; the post rate
cap is the host's Timeline cap. What stays in core is what is core: `deliver`
(the user sending a note as a real message: a route over Timeline entries),
`tell_mastermind` (a Mastermind feature with its wake caps), the per-session
post window both it and the host's Timeline appends use (`take_post_slot`) and
the `age` helper, in `notes.rs` reduced to those. The hint in
`agents.rs::ingest` becomes one `on-event(hook)` call for each active plugin
that declares the `hook` event.

**Mycelium.** The reader (`mycelium.rs`: plan, parse, stamp, fingerprint), the
snapshot types and the two tools move to `plugins/mycelium`. The snapshot JSON
is byte-identical to today's `Knowledge` serialization, which is already the
daemon↔UI wire, so the Knowledge view does not change. `knowledge.rs` keeps the
core: guidance files, the Timeline diff that attributes recorded entries to
turns, the route that merges provider and guidance. It asks the active provider
plugin for `knowledge(cx, known_stamp)` instead of calling `mycelium::read`,
caches by stamp as today, and attributes changes from the stamp's mtimes. The
stamp JSON carries the same `(path, mtime, len)` triples.

**The catalog.** `MANIFESTS` (two `include_str!`s) becomes a load of
`plugins/dist/*/plugin.toml` plus, when present, the installed
`~/.chimaera/plugins/<id>/current/plugin.toml`, each paired with its
`plugin.wasm`, sorted by id (the Plugins page's order, and the order active
plugins' tools and paragraphs reach an agent in). The UI is untouched by the
port: cards, switches, Adds lines and the attach sheet read the same routes;
P3 adds the version line and its actions. (Since 2026-09-27 nothing is
embedded: the catalog is the installed copies, and the lock's entries with
nothing installed are listed as available — [Versions and
updates](#versions-and-updates). The page is the Extensions tab.)

## Packaging and third parties

- **First-party: done.** Each plugin is its own repository,
  [chimaera-plugin-agent-notes](https://github.com/martinappberg/chimaera-plugin-agent-notes)
  and [chimaera-plugin-mycelium](https://github.com/martinappberg/chimaera-plugin-mycelium):
  the crate, its `plugin.toml` (naming the repository as `[release] github`),
  its native tests and CI, and a release workflow that publishes the three
  assets on a `v<version>` tag. The chimaera repository keeps
  `plugins/plugins.lock` (per plugin: `id`, `name`, `summary`, `version`,
  `repo`, `sha256_wasm`, `sha256_toml`), the only thing of theirs the daemon
  embeds: a user installs each from its release at the pinned version
  (decision 6), and a plugin in development installs from a directory
  ([plugins/AGENTS.md](../plugins/AGENTS.md)). The `plugins/` cargo workspace
  keeps only the host's test fixture.
- **A plugin repository** is a claude and codex marketplace repository (so its
  skills, hooks and agent-side pieces install through the agents' own managers,
  as the authoring guide requires) plus the crate and its `plugin.toml`, and a
  release that publishes `plugin.wasm`.
- **Installed plugins** (every plugin that runs):
  `~/.chimaera/plugins/<id>/<version>/{plugin.toml,plugin.wasm,SHA256SUMS[,local-path]}`
  behind `current` and `previous` links, written by a visible install from a
  plugin repository's release with the checksum shown, from a local
  directory, or by `chimaera plugin add`. Same host, same limits; the card
  names the version and whether the copy is Chimaera's and verified.
  Versions, the catalog, compatibility gates and updates are their own
  section: [Versions and updates](#versions-and-updates).
- **Trust** is what the sandbox gives: a third-party plugin cannot read outside
  the workspace, cannot run anything, cannot reach the network, and cannot take
  the daemon down. What it can do is what the manifest says it adds, and every
  Timeline write it makes is attributed to it.

## Versions and updates

Plugins have their own release cadence, so versions and updates are part of the
contract from the start, not a later feature. Everything here follows the two
rules the daemon already applies to agent CLIs: never guess that an update
exists, and never install or update anything without the user's click and a
visible, checksum-verified download.

- **Every manifest carries `version`** (the plugin's own, plain
  `MAJOR.MINOR.PATCH` with no pre-release part) and **`api`**
  (the WIT version it targets), and may carry **`requires.chimaera`** (a semver
  requirement on the daemon, for a plugin that needs a host import that arrived
  in a later daemon). The manifest is the one source of truth: a first-party
  repository's release workflow (and, for the test fixture, the build script)
  refuses a `plugin.toml` version that differs from the crate's `Cargo.toml`
  version, rather than injecting one; the build script also refuses a
  fixture whose `api` differs from the WIT package's MAJOR.MINOR.
- **The wire says what is running** (2026-09-27 shape). `GET /plugins` and
  `GET /workspaces/{id}/plugins` carry, for an installed entry: `version`,
  `api`, `source: "installed"`, `installed: true`, `first_party`,
  `verified`, `sha256_wasm`, `path`, and only when set `repo`,
  `pinned_version` (first-party), `local_path`, `previous`, `update`
  (`{version, url, checked_ms}`, present only when a check found a newer
  compatible release) and `fault` (a failed gate, files that don't match
  their `SHA256SUMS`, or the fault counter's reason). An available entry (a
  lock id with nothing installed) has the lock's `name`, `summary`, `version`,
  `repo` and `pinned_version`, `source: "available"`, `installed: false`,
  `first_party: true`, `verified: false`, `api: null` and empty
  contributions. The workspace route adds `on` / `detected` / `active`,
  `requires` and `recommends`. The card shows the name, a check badge for
  `first_party` ("Verified by the Chimaera maintainers"), the version and a
  "local build" note for `local_path` — `verified` stays off the card, its
  failure shows as the `fault` line — **Install <pinned>** for
  an available plugin, an **Update** chip when one is available, and **Use
  previous** / **Check now** / **Remove** for installed copies. The full
  field list: [features/plugins.md](features/plugins.md#versions-installs--updates).
- **Layout:** `~/.chimaera/plugins/<id>/<version>/{plugin.toml,plugin.wasm,SHA256SUMS[,local-path]}`
  (the release's `SHA256SUMS` kept beside the two files; `local-path`, one
  line, only for a copy installed from a directory) with an atomic `current`
  symlink swap (rename over the old link, the managed agent runtimes' idiom in
  `runtimes.rs`). An update never touches the version in use, the previous
  version stays behind a `previous` link for a one-click, reversible rollback
  (at most two versions on disk), and moving `current` drops the plugin's
  instances: a running session sees the new tools on its next `tools/list` and
  the new paragraph at its next `initialize`.
- **The catalog is the lock's entries plus the installed copies.** A lock
  entry with nothing installed is listed as available (never active; its
  switch answers 409 until it is installed); everything else is an installed
  copy, and only an installed copy loads. Each copy is checked when it is
  read: both files re-hashed against the `SHA256SUMS` beside them
  (`verified`; none → unverified without a fault; a mismatch → a fault, and
  the copy never loads), and a first-party copy at the pinned version also
  against the lock. `first_party` is an id in the lock whose copy's
  `[release] github` is the lock's `repo`, so an update keeps it and a
  third-party plugin reusing the id never has it. There is one copy per id, so
  no precedence rule.
- **Compatibility gates, before a component loads:** `api` must be a WIT version
  this host serves (0.1 now; a host may serve two worlds during a major
  transition), and `requires.chimaera` must match `chimaera_core::VERSION` (a
  dev build, the `0.0.1` sentinel, matches every requirement). A mismatch
  renders "needs chimaera ≥ x (this is y)", "needs a newer chimaera" (a newer
  `api`) or "needs a newer plugin" (an older one) on the card, and the plugin
  stays off with its switch kept. Because a component is portable across wasmtime
  versions (only a precompiled `.cwasm` binds to one), a daemon update never
  invalidates an installed plugin, and an additive WIT bump keeps every older
  plugin loading.
- **Where updates come from:** the manifest's `[release]` section, `github =
  "owner/repo"` (the releases API; assets `plugin.wasm`, `plugin.toml` and
  `SHA256SUMS`; tags `v<version>`), later a plain `url` base. The checker asks
  each installed plugin's source (the first-party manifests name their own
  repositories) at most once a day and on **Check now**, with the same cadence
  and a `CHIMAERA_PLUGIN_RELEASES_API` test knob as the daemon's own
  `update.rs`, compares semver, and only reports a release whose `api` and
  `requires.chimaera` this daemon satisfies. It never downloads on its own.
  A first-party plugin updated past the pin stays first-party and verified.
- **Update and install are one path:** `POST /plugins/{pid}/update` and
  `POST /plugins/install {github, version?}` fetch `SHA256SUMS` and
  `plugin.toml` first (small, into memory), verify the manifest's sha256 and
  check its id, the tag's version and its gates, then stream `plugin.wasm`
  (16 MiB cap) into a temp dir under `<id>/`, verify it, rename the dir to
  `<version>/` and swap `current` (the replaced version becomes `previous`);
  any failure leaves the old version current and says why.
  `POST /plugins/{pid}/rollback` (Use previous) swaps the two links and
  `DELETE /plugins/{pid}` removes the id's directory (a first-party plugin is
  then available again). A first-party install
  (`POST /plugins/{pid}/install`, or `POST /plugins/install {github}` naming
  the lock's repository) fetches the pinned release by its direct download
  URLs (`https://github.com/<repo>/releases/download/v<version>/…`, the base
  replaced by the `CHIMAERA_PLUGIN_DOWNLOADS` test knob) and checks both
  files against the release's `SHA256SUMS` and the lock. A local install
  (`POST /plugins/install {path}`) copies `plugin.toml`, `plugin.wasm` and an
  optional `SHA256SUMS` from a directory, the same version replacing itself.
  Update, rollback, check and remove on a plugin that isn't installed answer
  409 (a lock id) or 404. `chimaera plugin list|add|add --path|update|remove`
  are the same routes from the CLI.
- **State across versions:** the host's per-(plugin, workspace) state and the
  per-workspace switch follow the plugin id, not the version. A new version must
  tolerate what an older one stored (the API guide says so; Agent notes'
  cursors are the example). A plugin that wants a clean slate writes a new key.
- **Pinning:** `plugins/plugins.lock` (per plugin: `id`, `name`, `summary`,
  `version`, `repo`, and the release's sha256s of `plugin.wasm` and
  `plugin.toml`) is embedded in the daemon, and a first-party install fetches
  exactly that release and refuses any other bytes, so a first-party bump is a
  reviewed change to one file, shipped by the next chimaera release. It pins
  what Install fetches, not what an installed copy may update to.
- **Versioning the interface itself:** the WIT package version is the contract.
  Adding a host import is a minor bump (0.2 adds `exec` and `watch`); changing
  or removing an export is a new major with a new world, which the host serves
  beside the old one for a transition. The `chimaera-plugin-api` crate's
  version tracks the WIT.

## Proof of concept: phases and what each must show

### P0: the spike (done before any code lands)

A scratch host and guest outside the repo, on the pinned toolchain: wasmtime 49
components with async host functions, epoch interruption stopping a busy loop,
the memory cap refusing a 200 MiB allocation, a guest panic surviving as an
error, cold and warm compile time, instantiate time, call latency, RSS, the
binary-size delta, and a `cargo zigbuild` musl cross-build of a wasmtime host.

Measured (2026-09-26, wasmtime 49.0.1, wit-bindgen 0.62.0, Rust 1.96.0, a 58 KB
guest component; the scratch host and guest are the reference for the real host):

| Measurement | Value |
|---|---|
| the daemon today (release, stripped, UI embedded, macOS arm64) | 26.8 MB |
| host binary delta, Cranelift JIT in (release, stripped) | +9.4 MiB macOS arm64 · +13.2 MiB x86_64 musl · +9.4 MiB aarch64 musl |
| the same with wasmtime's `cache` feature | +0.75 to +0.9 MiB more, plus zstd C code, a background worker and `.wip` files: **not used** |
| host binary delta, runtime-only (no Cranelift, precompiled `.cwasm` only) | +2.5 MiB on every target |
| cold component compile (single-threaded Cranelift) | 25 to 30 ms (47 ms the very first run) |
| warm load from a serialized `.cwasm` (`serialize` / `deserialize_file`) | 0.1 to 0.2 ms |
| instantiate (Store + WASI context + instance) | 120 µs the first time, 17 to 25 µs after |
| one call (mean of 1,000, a small file read inside) | 18 to 20 µs; p99 24 to 54 µs |
| RSS: bare / after Engine + one JIT compile / after an instance / after 1,000 calls | 2.4 / 15 / 15.6 / 15.9 MB (about 10 MB of the compile's RSS stays; a runtime-only host sits at 3 to 5 MB) |
| virtual memory per live instance | 4 GiB by default; 69 MiB with `memory_reservation(64 MiB)`, `memory_guard_size(64 KiB)`, `memory_reservation_for_growth(0)`, at no latency cost. **Set it**: login nodes run with `ulimit -v` |
| a busy loop, 1 s deadline | interrupted at 1.003 to 1.043 s; the daemon survives; the instance is dead (`CannotEnterComponent`) and is re-created in about 20 µs |
| a 200 MiB allocation under a 64 MiB cap | a trap; the host survives; 16 + 40 MiB on the same instance succeed |
| a guest panic | a trap with the message on the guest's stderr; the host survives; a fresh instance works |
| musl cross-build with `cargo zigbuild` | x86_64 and aarch64 both build static, stripped, no warnings (with or without the cache feature) |

Facts the host is built on: `wasmtime` with `default-features = false` and
`component-model`, `async`, `runtime`, `std`, `cranelift` (the defaults pull in
gc, threads, pooling, profiling, coredump and more); `wasmtime-wasi` with only
`p2`; `bindgen!` with `imports: { default: async }, exports: { default: async }`;
`Config::async_support` is a deprecated no-op in 49 (async is implied by the
`_async` calls); `epoch_interruption(true)` and an epoch deadline **armed per
call** (the deadline starts at 0, so an unarmed store traps on instantiate) with
a callback that yields to tokio on each tick and interrupts past the wall-clock
deadline; `wasmtime_wasi::p2::add_to_linker_async` (the wasip2 std imports
`wasi:io` and `wasi:cli` at 0.2.6; wasmtime-wasi 49 serves them by semver; no
files, clocks, random or sockets are imported by a guest that does not use
them); `WasiCtxBuilder::new()` with nothing granted and the guest's stderr
captured, capped, and logged on a trap; `macos_use_mach_ports(false)`, so
traps ride ordinary Unix signal handlers on macOS too: wasmtime's default
Mach-port handler thread aborts the whole process when a caught signal
interrupts its `mach_msg`, and the daemon catches one all the time (tokio's
SIGCHLD reaper, firing as PTY shells end). That was the intermittent SIGABRT of
the server's test binary from P1 until P3 found it, and a shipped daemon on a
Mac was exposed the same way; the flag is a no-op off macOS. Components are
compiled once per daemon lifetime and kept in memory; an on-disk `.cwasm` cache
is later (P5) if a login node's cold compile hurts. No Cargo dependency needs
pinning between wit-bindgen and wasmtime-wasi.

**One decision for the maintainer.** Cranelift in the daemon costs 9 to 13 MiB
of binary and about 10 MB of resident memory after a plugin is compiled, and lets
any `plugin.wasm` load, including a third party's. A runtime-only daemon costs
2.5 MiB and 3 to 5 MB, and loads only components precompiled for its exact
target and wasmtime version, so every plugin (first-party in CI, third-party by a
`chimaera plugin build` step somewhere with Cranelift) ships per-target
artifacts. The PoC builds the JIT host, since it is the one that keeps the
third-party story simple; measuring the runtime-only build on a real login node,
if the size matters, is later (P5).

**A packaging fact the spike settled.** A plugin crate (a `cdylib`) does not link
for the native target on macOS: its export names contain `#`, which Apple's
linker reads as a comment in the exported-symbols list. `cargo check`, clippy
and `cargo test` are fine (the test harness links the rlib, not the cdylib), but
a plain `cargo build --workspace` would fail. So the plugin crates form **their
own cargo workspace under `plugins/`**, built only through the script for
`wasm32-wasip2` and checked and unit-tested natively from there; the daemon
workspace never compiles them. That also keeps the daemon's lockfile free of
wit-bindgen's copies of the wasm-tools crates, and mirrors where the plugins are
going. A native unit test must not call a host import (the stub aborts the test
binary), so plugin crates keep their pure logic in modules that take data, not
the host.

### P1: the API crate, the host, Agent notes as WASM

`chimaera-plugin-api` with the WIT and the bindings; `runtime.rs` and
`hostfns.rs`; `plugins/agent-notes`; `scripts/build-plugins.sh`, `just plugins`,
the CI step and `wasm32-wasip2` in `rust-toolchain.toml`; `notes.rs` reduced to
core. Every test in `src/tests/plugins.rs` passes unchanged in meaning
(post/read/stay in their workspace; tools only where on; pre-allows), the
`agent_view` fixtures are byte-identical, and new tests cover the host: the
deadline trap, the memory cap, a panicking plugin, the append rate cap, a
symlinked path refused, state capped. A fixture plugin under
`plugins/test-fixture` (loop, allocate, panic, echo) is built by the same script
for those tests.

**Built** (2026-09-26, `e2873f4`): as above, plus the manifest/tools mismatch
refusal, emit frames, the fault counter, and a test that the fixture never
reaches a production catalog (`src/tests/plugin_host.rs`); `plugins/` is its own
cargo workspace; CI, the release workflow, the Tauri `beforeBuildCommand`, the
cloud bootstrap and the isolated-daemon script build the plugins first.
Measured: the release daemon 38.4 MB against 26.8 MB before (+9.8 MiB,
Cranelift); Agent notes' `plugin.wasm` 135 KB; a release-grade compile of it 55
to 82 ms; a warm tool call through the MCP endpoint 1 to 2 ms.

### P2: Mycelium as WASM

`plugins/mycelium` with the reader and the two tools; `mycelium.rs` deleted;
`knowledge.rs` on the provider export. The Knowledge route's JSON for the test
fixtures is byte-identical before and after (a snapshot test), the Timeline
attribution tests pass, and the reader's own unit tests run natively in the
plugin crate.

**Built** (2026-09-26, `79d2e35`): the reader, the caps, the warning texts and
the `Serialize` shapes moved unchanged behind a small `Fs` trait (the host's
functions in the component, `std::fs` only for the 27 moved unit tests); the
`knowledge(cx, known)` export answers "unchanged" for its own stamp and keeps
the last snapshot for the two tools; the Knowledge route's bytes and both
tools' text match the fixtures pinned before the move, with no re-bless.

Measured (2026-09-26, macOS arm64, the daemon's tests in release,
`--test-threads=1`, the `tests/fixtures/living` workspace):
`plugins/dist/mycelium/plugin.wasm` is 305,006 bytes; the first `knowledge` ask
(Cranelift compile + instantiate + read) takes 202 to 209 ms, about 200 ms of
it the compile (the spike's 58 KB guest compiled in 25 to 50 ms); an ask in a
second workspace (instantiate + read) 1.3 to 1.7 ms; the stamp-unchanged answer
0.40 to 0.50 ms. The daemon's source loses 4,226 lines (`mycelium.rs`, the
native manifest, the native tool arms) and gains 222.

### P3: versions, the installed directory, updates

Everything in [Versions and updates](#versions-and-updates): `version`, `api`
and `requires.chimaera` in the manifest with the build script's version check;
the wire fields and the card (version, source chip, Update, Remove, Use
previous); the installed directory with `current` links, the precedence rule and
the compatibility gates; the release checker behind `CHIMAERA_PLUGIN_RELEASES_API`;
the install, update, remove and rollback routes with the checksum-verified
download and the atomic swap; `chimaera plugin add|update|remove|list`. Tests
run against a local fake releases server, as `update.rs`'s do: a newer
compatible release is offered and an incompatible one is not, an update swaps
`current` and drops instances, a bad checksum leaves the old version current,
precedence picks the higher version and the card names both, a stale installed
copy is flagged. The live proof installs a plugin from a fake release into the
isolated daemon's home, sees it on the card, bumps the fake release, sees
**Update**, updates, and watches a session's next `tools/list` carry the new
version's tools.

**Built** (2026-09-26, `55acf73`): the manifest's `version`, `api`,
`requires.chimaera`, `[release] github` and `[provides] events` (a hook
reaches only plugins that declare `hook`: Agent notes does, Mycelium doesn't),
and the build script's version and API checks; the catalog on `AppState`, reloaded after each
change, with the precedence rule and the gates (`plugins/mod.rs`); the
installed directory with `current` and `previous` links, and the install,
update, Use previous and Remove routes (`plugins/installed.rs`); the checker
on the daemon's own release loop, and Check now (`plugins/releases.rs`);
`chimaera plugin list|add|update|remove`; the card. `previous` is a second
link rather than "the next lower version", so Use previous is reversible and
at most two versions stay on disk. Tests in `src/tests/plugin_updates.rs`
against a fake releases server, with the fixture's `v2` build as the next
release. The live pass (an isolated daemon, a static fake release): add from
the CLI, the card, a published 0.2.0 offered by Check now and by the boot
check, Update from the card (the verified checksum shown), Use previous and
back, Remove through its dialog, a planted `api = "0.2"` copy listed off with
its reason. P4's live proof added the rest: a session's `tools/list` after an
update and after Use previous ([Status](#status-2026-09-27)).

### P4: docs and the live proof

Maps (`crates/chimaera-plugin-api/AGENTS.md`, `plugins/AGENTS.md`, the server
map's plugins rows), the feature page, the authoring guide rewritten around the
API crate, this plan's numbers. Then the isolated daemon: switch Agent notes on,
watch a session's `tools/list` gain `post_note` and `read_notes` and lose them
when switched off; post and read through the MCP endpoint; the hook hint; a
workspace with `.living/` showing Knowledge through the WASM reader; the fault
path (a plugin built to panic) leaving the daemon up; RSS before and after.

**Done** (2026-09-26): the maps, the feature page, the authoring guide and this
plan, and the live proof in [Status](#status-2026-09-27), except the fault path,
which stays with the host tests.

### P5: after the proof

First slice done (see [Status](#status-2026-09-27)): the first-party plugins in
their own repositories, `plugins/plugins.lock`, and embedded plugins updating
from their repositories; then (2026-09-27, decisions 6 and 7) no embedded
plugins at all, the three install kinds, `SHA256SUMS` kept and re-checked,
`recommends`, and the Extensions tab. Still to come: the Browse view over plugin
repositories; `exec` and `watch` in the WIT (0.2)
and the LaTeX plan's `build` point on them; the UI-facing `query` route;
precompiled first-party components if cold compile is slow on a login node.

## Risks, and what the spike answered

- **wasmtime on musl: answered.** Both musl targets cross-build with
  `cargo zigbuild`, static and stripped, with or without the cache feature.
- **Binary size: measured, under the line.** 9.4 MiB (arm64) to 13.2 MiB
  (x86_64) with Cranelift in the spike; the real daemon went from 26.8 to
  38.4 MB (P1). 2.5 MiB runtime-only. The JIT host shipped; the runtime-only
  trade is still the maintainer's call.
- **Cold compile on a login node: small on a laptop, unmeasured on a login
  node.** 25 to 50 ms for the spike's 58 KB component, 55 to 82 ms for Agent
  notes, about 200 ms for the 305 KB Mycelium reader (release; a debug build
  takes seconds). Once per daemon lifetime; an on-disk `.cwasm` is the
  fallback (P5).
- **Native compile of plugin crates: answered by moving them.** The plugin
  workspace is separate; the daemon never compiles a cdylib.
- **A plugin's memory is the daemon's RSS.** About 10 MB stays resident after a
  compile, plus each instance's linear memory (a few MiB typical, 64 MiB cap).
  64 instances is a 4 GiB worst case on paper; idle eviction keeps it honest.
  Measured in P4 on a release daemon: 5.8 MB idle, 28.8 MB after the first
  plugin call, 28.9 MB after 50 more. Virtual memory per instance is 69 MiB with
  the reservation set, not 4 GiB, which matters under `ulimit -v`.
- **wasmtime's Mach-port trap handler on macOS: answered.** It aborted the
  whole process when tokio's SIGCHLD handler interrupted its `mach_msg` (the
  intermittent test-binary SIGABRT from P1 to P3). `macos_use_mach_ports(false)`
  moves traps to Unix signal handlers; every trap test still passes, and a
  full-suite run under SIGCHLD load no longer aborts.
- **A trapped instance is dead**, by design: every trap costs the plugin its
  instance and a 20 µs re-creation, never the daemon.

## Out of scope

- Plugin-contributed UI code. Plugins contribute data; core owns viewers. A
  versioned browser host API for plugin JS modules is the escape hatch for later,
  and only if a plugin genuinely needs novel UI.
- Plugins in languages other than Rust. Any language that targets the component
  model can implement the world; only the Rust bindings are supported.
- Running plugins on a host other than the daemon's.
