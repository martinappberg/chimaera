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
5. **Versions and updates are baked in.** Every plugin has a version and an API
   version, an installed plugin can be updated (and rolled back) on its own
   cadence with a visible, checksum-verified download, and the daemon says on
   the card which version is running and where it came from
   ([Versions and updates](#versions-and-updates)).

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
| Compiled code | once per daemon lifetime, in memory (25 to 50 ms for a small component) | `runtime.rs`; an on-disk `.cwasm` (`Component::serialize`) is P4 if a login node's cold compile hurts; wasmtime's own `cache` feature stays out |
| Virtual memory | 64 MiB reserved per instance, not wasmtime's 4 GiB default | `memory_reservation`, `memory_guard_size`, `memory_reservation_for_growth` |
| WASI | nothing granted: no files, no env, no args, no network; the guest's stderr captured (4 KiB) and logged on a trap | `WasiCtxBuilder::new()` |
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
  repository as **their own cargo workspace** (`plugins/Cargo.toml`, depending on
  `../crates/chimaera-plugin-api` by path): built to WASM by the script, checked
  and unit-tested natively from that workspace (`cargo clippy` / `cargo test`
  with `--manifest-path plugins/Cargo.toml`), never compiled by the daemon
  workspace (a cdylib with component export names does not link natively on
  macOS). Their move to their own repositories is a file copy plus a line in
  `scripts/build-plugins.sh` that downloads the pinned release artifact with its
  checksum instead of building.
- **A plugin repository** is a claude and codex marketplace repository (so its
  skills, hooks and agent-side pieces install through the agents' own managers,
  as the authoring guide requires) plus the crate and its `plugin.toml`, and a
  release that publishes `plugin.wasm`.
- **Installed plugins:** `~/.chimaera/plugins/<id>/<version>/{plugin.toml,plugin.wasm}`
  behind a `current` link, written by a visible install from a plugin
  repository's release with the checksum shown, or by `chimaera plugin add`.
  Same host, same limits; the card names the source and the version. Versions,
  precedence between an embedded and an installed copy, compatibility gates and
  updates are their own section: [Versions and updates](#versions-and-updates).
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

- **Every manifest carries `version`** (the plugin's own, semver) and **`api`**
  (the WIT version it targets), and may carry **`requires.chimaera`** (a semver
  requirement on the daemon, for a plugin that needs a host import that arrived
  in a later daemon). The manifest is the one source of truth: the build script
  refuses a first-party plugin whose `plugin.toml` version differs from its
  crate's `Cargo.toml` version, rather than injecting one.
- **The wire says what is running.** `GET /plugins` and `GET /workspaces/{id}/plugins`
  gain `version`, `api`, `source` (`embedded` | `installed`), the installed copy's
  path, and `update` (`{version, url, checked_ms}`, present only when a check found
  a newer compatible release). The card shows `name · version` with a source chip
  ("ships with chimaera 0.4.1" or "installed"), an **Update** chip when one is
  available, and **Remove** / **Use previous** for installed copies. Nothing
  else on the wire changes.
- **Layout:** `~/.chimaera/plugins/<id>/<version>/{plugin.toml,plugin.wasm}`
  with an atomic `current` symlink swap (rename over the old link, the managed
  agent runtimes' idiom in `runtimes.rs`). An update never touches the version
  in use, the previous version stays for a one-click rollback, and moving
  `current` drops the plugin's instances: a running session sees the new tools
  on its next `tools/list` and the new paragraph at its next `initialize`.
- **Precedence, never silent:** the same id embedded and installed → the higher
  version loads and the card names both ("0.3.2 installed · 0.3.1 ships with
  chimaera"); equal versions → the embedded copy; an installed copy older than
  the embedded one is shown as stale with **Remove**.
- **Compatibility gates, before a component loads:** `api` must be a WIT version
  this host serves (0.1 now; a host may serve two worlds during a major
  transition), and `requires.chimaera` must match `chimaera_core::VERSION`. A
  mismatch renders "needs chimaera ≥ x" or "needs a newer plugin" on the card
  and the plugin stays off. Because a component is portable across wasmtime
  versions (only a precompiled `.cwasm` binds to one), a daemon update never
  invalidates an installed plugin, and an additive WIT bump keeps every older
  plugin loading.
- **Where updates come from:** the manifest's `[release]` section, `github =
  "owner/repo"` (the releases API; assets `plugin.wasm`, `plugin.toml` and
  `SHA256SUMS`; tags `v<version>`), later a plain `url` base. The checker asks
  each installed plugin's source at most once a day and on **Check now**, with
  the same cadence and a `CHIMAERA_PLUGIN_RELEASES_API` test knob as the daemon's
  own `update.rs`, compares semver, and only reports a release whose `api` and
  `requires.chimaera` this daemon satisfies. It never downloads on its own.
- **Update and install are one path:** `POST /plugins/{pid}/update` and
  `POST /plugins/install {github, version?}` download `plugin.wasm` and
  `plugin.toml` into `<id>/<version>/`, verify the sha256 against `SHA256SUMS`,
  check the new manifest's gates, then swap `current`; any failure leaves the
  old version current and says why. `chimaera plugin add|update|remove` are the
  same routes from the CLI. Embedded plugins update with the daemon through the
  existing update flow, and their card says so.
- **State across versions:** the host's per-(plugin, workspace) state and the
  per-workspace switch follow the plugin id, not the version. A new version must
  tolerate what an older one stored (the API guide says so; Agent notes'
  cursors are the example). A plugin that wants a clean slate writes a new key.
- **Pinning, once plugins live in their own repositories:** `plugins/plugins.lock`
  (id, version, source, sha256 of `plugin.wasm`) is what `scripts/build-plugins.sh`
  fetches for the release binary, so a first-party bump is a reviewed change to
  one file and the daemon embeds exactly those bytes.
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
captured, capped, and logged on a trap. Components are compiled once per daemon
lifetime and kept in memory; an on-disk `.cwasm` cache is P4 if a login node's
cold compile hurts. No Cargo dependency needs pinning between wit-bindgen and
wasmtime-wasi.

**One decision for the maintainer.** Cranelift in the daemon costs 9 to 13 MiB
of binary and about 10 MB of resident memory after a plugin is compiled, and lets
any `plugin.wasm` load, including a third party's. A runtime-only daemon costs
2.5 MiB and 3 to 5 MB, and loads only components precompiled for its exact
target and wasmtime version, so every plugin (first-party in CI, third-party by a
`chimaera plugin build` step somewhere with Cranelift) ships per-target
artifacts. The PoC builds the JIT host, since it is the one that keeps the
third-party story simple; P4 measures the runtime-only build on a real login node
if the size matters.

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

### P2: Mycelium as WASM

`plugins/mycelium` with the reader and the two tools; `mycelium.rs` deleted;
`knowledge.rs` on the provider export. The Knowledge route's JSON for the test
fixtures is byte-identical before and after (a snapshot test), the Timeline
attribution tests pass, and the reader's own unit tests run natively in the
plugin crate.

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

**Built** (2026-09-26): the manifest's `version`, `api`, `requires.chimaera`,
`[release] github` and `[provides] events` (a hook reaches only plugins that
declare `hook`: Agent notes does, Mycelium doesn't), and the build script's
version and API checks; the catalog on `AppState`, reloaded after each
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
its reason; a session's `tools/list` after an update is covered by the tests,
not yet by a live agent (P4).

### P4: docs and the live proof

Maps (`crates/chimaera-plugin-api/AGENTS.md`, `plugins/AGENTS.md`, the server
map's plugins rows), the feature page, the authoring guide rewritten around the
API crate, this plan's numbers. Then the isolated daemon: switch Agent notes on,
watch a session's `tools/list` gain `post_note` and `read_notes` and lose them
when switched off; post and read through the MCP endpoint; the hook hint; a
workspace with `.living/` showing Knowledge through the WASM reader; the fault
path (a plugin built to panic) leaving the daemon up; RSS before and after.

### P5: after the proof

The Browse view over plugin repositories; `exec` and `watch` in the WIT (0.2)
and the LaTeX plan's `build` point on them; the UI-facing `query` route;
precompiled first-party components if cold compile is slow on a login node.

## Risks, and what the spike answered

- **wasmtime on musl: answered.** Both musl targets cross-build with
  `cargo zigbuild`, static and stripped, with or without the cache feature.
- **Binary size: measured, under the line.** 9.4 MiB (arm64) to 13.2 MiB
  (x86_64) with Cranelift; 2.5 MiB runtime-only. The JIT host is the PoC; the
  runtime-only trade is the maintainer's call in P4.
- **Cold compile on a login node: small so far.** 25 to 50 ms for a 58 KB
  component on a laptop; the mycelium reader will be larger and P2 measures it.
  Once per daemon lifetime; an on-disk `.cwasm` is the fallback.
- **Native compile of plugin crates: answered by moving them.** The plugin
  workspace is separate; the daemon never compiles a cdylib.
- **A plugin's memory is the daemon's RSS.** About 10 MB stays resident after a
  compile, plus each instance's linear memory (a few MiB typical, 64 MiB cap).
  64 instances is a 4 GiB worst case on paper; idle eviction and the live proof's
  RSS reading (P3) keep it honest. Virtual memory per instance is 69 MiB with the
  reservation set, not 4 GiB, which matters under `ulimit -v`.
- **A trapped instance is dead**, by design: every trap costs the plugin its
  instance and a 20 µs re-creation, never the daemon.

## Out of scope

- Plugin-contributed UI code. Plugins contribute data; core owns viewers. A
  versioned browser host API for plugin JS modules is the escape hatch for later,
  and only if a plugin genuinely needs novel UI.
- Plugins in languages other than Rust. Any language that targets the component
  model can implement the world; only the Rust bindings are supported.
- Running plugins on a host other than the daemon's.
