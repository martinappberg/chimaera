# The plugin system: WASM plugins on a small host

Dated 2026-09-26. A plan with a proof of concept attached: the design below is
proven by porting the two workbench plugins that exist today (Agent notes,
Mycelium) to run as WASM components before anything new is built on it. Read
this first when touching `crates/chimaera-plugin-api`, `crates/chimaera-server/src/plugins/`
or `plugins/`. The earlier plugin seam (manifests embedded as data, behaviour as
named first-party code) is in [timeline-knowledge-plugins-plan.md §6](timeline-knowledge-plugins-plan.md);
this plan replaces its "named built-in" column. The LaTeX and Typst plugins are
the first new plugins on it ([latex-reports-plan.md](latex-reports-plan.md)).

## Decisions (maintainer, 2026-09-26)

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
4. **First-party plugins ship inside the binary** as embedded `.wasm` files, so
   one static daemon over ssh still carries everything and nothing is downloaded
   to a host by surprise. Third-party plugins install into `~/.chimaera/plugins/`
   later, through the same host and limits.

## The short version

- **One artifact per plugin, portable.** A plugin is `plugin.wasm` (a WebAssembly
  component built from Rust for `wasm32-wasip2`) plus `plugin.toml` (today's
  manifest). The same file runs on a macOS laptop, an x86 login node and an ARM
  box: no per-platform builds, no per-host installs for first-party plugins.
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
plugins/agent-notes/            (one crate; later its own repository)
  Cargo.toml                    crate-type = ["cdylib"], depends on chimaera-plugin-api
  plugin.toml                   the manifest: id, name, summary, provides, adds …
  src/lib.rs                    impl chimaera_plugin_api::Plugin for AgentNotes { … }
                                chimaera_plugin_api::export!(AgentNotes);
  README.md · skills/ · .claude-plugin/ · .codex-plugin/   (agent-side assets, optional)
```

Built with `cargo build --target wasm32-wasip2 --release`, the crate yields a
component. `scripts/build-plugins.sh` (also `just plugins`) builds every
first-party plugin and lays out `plugins/dist/<id>/{plugin.wasm,plugin.toml}`,
which the daemon embeds with rust-embed exactly as it embeds `web-ui/dist` (a
debug daemon reads the folder from disk per load, so a rebuilt plugin needs no
daemon restart). `plugins/dist/` is gitignored and built in CI before `cargo
build`, like the UI.

The manifest is unchanged in format (`plugins/mod.rs` parses it with
`deny_unknown_fields`) and gains one field, `api = "0.1"`, the WIT version the
component targets. The host refuses a component whose `provides.mcp_tools` does
not match the names its `tools()` export returns: the card's Adds line and the
tool gate come from the manifest, so the two must agree.

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
    use types.{context, stat, entry, session, json, level};

    /// Workspace files. Paths are workspace-relative; no component may be a
    /// symlink; reads stop at `cap` bytes; listings at `cap` entries. All of
    /// it runs off the reactor behind the filesystem semaphore.
    read: func(cx: context, path: string, cap: u32) -> result<list<u8>, string>;
    stat: func(cx: context, path: string) -> result<stat, string>;
    list: func(cx: context, path: string, cap: u32) -> result<list<entry>, string>;

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
that panics, so a plugin's pure logic (parsers, addressing rules) is unit-tested
with plain `cargo test` and only the integration runs in the daemon's tests.

## The host

`crates/chimaera-server/src/plugins/` becomes the host and nothing else:

| File | What |
|---|---|
| `mod.rs` | the manifest (unchanged), the catalog now loaded from `plugins/dist` (embedded) and `~/.chimaera/plugins/` (installed), per-workspace on/off, detect, routes: as today |
| `runtime.rs` | one `wasmtime::Engine` (Cranelift, async, epoch interruption, compile cache); per-(plugin, workspace) instances created on first use, dropped after 10 min idle or on switch-off; one call at a time per instance; per-call deadlines; the fault counter |
| `hostfns.rs` | the `host` interface: bounded fs behind `fs::FILESYSTEM_WORK` on a blocking thread, state (capped), sessions, Timeline append (allowed kinds, rate cap) and recent, emit, log |
| `tools.rs` | generic: `owner(tool)` from the manifests as today; `defs` / `instructions` / `call` go to the plugin's exports through the runtime |

Limits, all in the host:

| Limit | Value | How |
|---|---|---|
| Time per call | 5 s for tools, queries and events; 30 s for knowledge | epoch interruption: a 100 ms ticker, a wall-clock deadline checked on each tick, then a trap |
| Memory per instance | 64 MiB linear memory | `StoreLimits` |
| Compiled code | cached on disk per component hash | wasmtime's cache under `~/.cache/chimaera/wasmtime` (measured in the PoC; a first-party plugin may ship precompiled later if cold compile is slow on a login node) |
| WASI | clocks and random only; no files, no env, no args, no network | `WasiCtxBuilder` with nothing granted |
| Reads | `cap` bytes per read, 8 MiB ceiling; 4,096 entries per list | `hostfns.rs` |
| State | 64 KiB per plugin per workspace | `hostfns.rs` |
| Timeline appends | 10 per session per minute; `note` entries only; text ≤ 2 KiB | `hostfns.rs` |
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
`tell_mastermind` (a Mastermind feature with its wake caps) and the `age`
helper, in `notes.rs` reduced to those. The hint in `agents.rs::ingest` becomes
one `on-event(hook)` call for each active plugin.

**Mycelium.** The reader (`mycelium.rs`: plan, parse, stamp, fingerprint), the
snapshot types and the two tools move to `plugins/mycelium`. The snapshot JSON
is byte-identical to today's `Knowledge` serialization, which is already the
daemon↔UI wire, so the Knowledge view does not change. `knowledge.rs` keeps the
core: guidance files, the Timeline diff that attributes recorded entries to
turns, the route that merges provider and guidance. It asks the active provider
plugin for `knowledge(cx, known_stamp)` instead of calling `mycelium::read`,
caches by stamp as today, and attributes changes by stat-ing the paths the
snapshot names. The stamp JSON carries the same `(path, mtime, len)` triples.

**The catalog.** `MANIFESTS` (two `include_str!`s) becomes a load of
`plugins/dist/*/plugin.toml` plus, when present, `~/.chimaera/plugins/*/plugin.toml`,
each paired with its `plugin.wasm`. The UI is untouched: cards, switches, Adds
lines and the attach sheet read the same routes.

## Packaging and third parties

- **First-party, now:** the plugin crates live under `plugins/` in this
  repository as workspace members (they compile natively for clippy and tests,
  and to WASM for the artifact). Their move to their own repositories is a file
  copy plus a line in `scripts/build-plugins.sh` that downloads the pinned
  release artifact with its checksum instead of building.
- **A plugin repository** is a claude and codex marketplace repository (so its
  skills, hooks and agent-side pieces install through the agents' own managers,
  as the authoring guide requires) plus the crate and its `plugin.toml`, and a
  release that publishes `plugin.wasm`.
- **Installed plugins (later):** `~/.chimaera/plugins/<id>/{plugin.toml,plugin.wasm}`,
  written by a visible install from a release URL with the checksum shown, or by
  `chimaera plugin add`. Same host, same limits; the card names the source; an
  id that exists both embedded and installed is a conflict the card shows, never
  a silent override. A component targeting a WIT version this daemon does not
  serve renders "needs a newer chimaera" and stays off.
- **Trust** is what the sandbox gives: a third-party plugin cannot read outside
  the workspace, cannot run anything, cannot reach the network, and cannot take
  the daemon down. What it can do is what the manifest says it adds, and every
  Timeline write it makes is attributed to it.

## Proof of concept: phases and what each must show

### P0: the spike (done before any code lands)

A scratch host and guest outside the repo, on the pinned toolchain: wasmtime 49
components with async host functions, epoch interruption stopping a busy loop,
the memory cap refusing a 200 MiB allocation, a guest panic surviving as an
error, cold and warm compile time, instantiate time, call latency, RSS, the
binary-size delta, and a `cargo zigbuild` musl cross-build of a wasmtime host.

Measured (filled in from the spike):

| Measurement | Value |
|---|---|
| the daemon today (release, stripped, UI embedded, macOS arm64) | 26.8 MB |
| host binary size delta (release, stripped) | _pending_ |
| cold component compile / warm (cache) | _pending_ |
| instantiate | _pending_ |
| call latency (mean of 1,000) | _pending_ |
| RSS: engine + one instance | _pending_ |
| musl x86_64 / aarch64 cross-build | _pending_ |

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

### P2: Mycelium as WASM

`plugins/mycelium` with the reader and the two tools; `mycelium.rs` deleted;
`knowledge.rs` on the provider export. The Knowledge route's JSON for the test
fixtures is byte-identical before and after (a snapshot test), the Timeline
attribution tests pass, and the reader's own unit tests run natively in the
plugin crate.

### P3: docs and the live proof

Maps (`crates/chimaera-plugin-api/AGENTS.md`, `plugins/AGENTS.md`, the server
map's plugins rows), the feature page, the authoring guide rewritten around the
API crate, this plan's numbers. Then the isolated daemon: switch Agent notes on,
watch a session's `tools/list` gain `post_note` and `read_notes` and lose them
when switched off; post and read through the MCP endpoint; the hook hint; a
workspace with `.living/` showing Knowledge through the WASM reader; the fault
path (a plugin built to panic) leaving the daemon up; RSS before and after.

### P4: after the proof

The installed-plugin directory and the Browse install; `exec` and `watch` in the
WIT (0.2) and the LaTeX plan's `build` point on them; the UI-facing `query`
route; precompiled first-party components if cold compile is slow on a login
node.

## Risks the proof must answer

- **wasmtime on musl.** The release builds cross-compile with `cargo zigbuild`.
  wasmtime is pure Rust with Cranelift, but its optional compile cache pulls in
  zstd's C code; the spike builds both ways.
- **Binary size.** The daemon is deployed over ssh on every update. If wasmtime
  costs more than about 20 MB, the PoC records it and the decision is the
  maintainer's; the design does not change.
- **Cold compile on a login node.** Cranelift compiling a 2 MB component on a
  busy node could take seconds; the cache makes it once per plugin version, and
  first-party components can ship precompiled per target if it matters.
- **Native compile of plugin crates.** The workspace's clippy and test runs
  compile the plugin crates for the host target; the bindings must build there
  (stubs) or the crates leave the workspace and build only through the script.
- **A plugin's memory is the daemon's RSS.** 64 MiB per instance and 64 instances
  is a 4 GiB worst case on paper; the idle eviction and the fact that a typical
  plugin uses a few MiB keep the real number small, and the live proof measures
  it.

## Out of scope

- Plugin-contributed UI code. Plugins contribute data; core owns viewers. A
  versioned browser host API for plugin JS modules is the escape hatch for later,
  and only if a plugin genuinely needs novel UI.
- Plugins in languages other than Rust. Any language that targets the component
  model can implement the world; only the Rust bindings are supported.
- Running plugins on a host other than the daemon's.
