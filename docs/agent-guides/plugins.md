# Writing a workbench plugin

A **workbench plugin** is an opt-in add-on that runs in Chimaera (not inside an
agent CLI) and says exactly what it adds. It is a Rust crate compiled to one
portable WebAssembly component, `plugin.wasm`, beside a `plugin.toml` manifest;
the daemon's plugin host runs it in a sandbox through the `chimaera:plugin` WIT
world. This guide is the recipe. Design and rationale:
[plugin-system-plan.md](../plugin-system-plan.md) (the host, the limits,
versions and updates) and
[timeline-knowledge-plugins-plan.md §6](../timeline-knowledge-plugins-plan.md)
(the seam and the card). What users see: [features/plugins.md](../features/plugins.md).
The maps: the API crate [chimaera-plugin-api](../../crates/chimaera-plugin-api/AGENTS.md),
the first-party crates [plugins/](../../plugins/AGENTS.md).

## The rules that don't bend

- **A plugin is a Rust crate compiled to WASM.** A `cdylib` built for
  `wasm32-wasip2` against `chimaera-plugin-api`: never code compiled into the
  daemon, never a script, never native code. No plugin behaviour is daemon
  code; a plugin that needs something the host doesn't offer needs a new host
  import (a WIT change, below), not a special case in the daemon.
- **The host bounds everything.** A plugin cannot open a file, start a
  process, reach the network or touch the daemon's state; it asks the host,
  and every limit (deadlines, memory, read and listing caps, state size,
  Timeline rate caps) is enforced there, for every plugin. Never enforce a
  host limit in the plugin; pre-checking for a friendlier message is fine.
- **The exports are the complete set; imports are additive.** Every plugin
  provides the six exports of the `plugin` interface (the `Plugin` trait
  gives each a default). Removing or changing an export, or a record a
  plugin returns, breaks every built plugin. A new host import does not: a
  host may offer more than a component uses, so WIT 0.2 adds `exec` and
  `watch` without breaking a 0.1 plugin.
- **No host call in a native test.** Built natively (tests, clippy), every
  host import is a wit-bindgen stub that aborts the whole test binary. Keep
  pure logic in functions that take data (`plugins/agent-notes/src/notes.rs`)
  or behind a trait the test implements (`plugins/mycelium/src/fs.rs`); the
  integration runs in the daemon's tests.
- **JSON for open-ended payloads.** Tool arguments and results, Timeline
  entries, the Knowledge snapshot, UI events and state values cross as JSON
  text: they already have JSON shapes on the daemon↔UI wire, and adding a
  field never breaks the ABI. Small fixed things are typed records.
- **Nothing changes for agents unless a plugin is on.** A plugin is off by
  default and switched on per workspace; it is *active* where it is on, its
  `detect` footprint is present and it passes its gates. Its tools and
  instruction paragraph are offered AND call-gated only there. The
  plugin-free worker view is pinned byte for byte by
  `crates/chimaera-server/src/tests/agent_view.rs`; if that test fails, you
  changed core.
- **It says what it adds.** Every manifest carries `[adds] ui = […]` and/or
  `agents = […]`, the card's "Adds / Would add" lines in plain words. A test
  fails a shipped manifest that adds nothing.
- **Versions are required.** Every manifest carries `version` (the plugin's
  own, plain `MAJOR.MINOR.PATCH`, equal to its crate's) and `api` (the WIT
  version it targets). A new version tolerates whatever an older one stored.
- **Agent-side pieces ride open standards** (MCP, Agent Skills, the agents'
  own plugin managers) so they keep working outside Chimaera. Never
  reimplement `claude plugin` / `codex plugin`.
- **Login-node discipline.** Detection is a few `stat`s off the reactor,
  cached; nothing polls; a plugin nobody switches on is never compiled.

## The manifest

`plugin.toml` at the crate root, parsed with `deny_unknown_fields` (a typo is
an error, not a silently ignored key) and validated: the id, the version, the
release source.

```toml
id = "mycelium"                 # stable; lowercase letters, digits, dashes; ≤ 64; not "install"
name = "Mycelium"
version = "0.1.0"               # the plugin's own, MAJOR.MINOR.PATCH; must equal the crate's
summary = "Project memory your agents record as they work — findings, decisions, learnings."
homepage = "https://github.com/arjunrajlaboratory/mycelium"   # optional
api = "0.1"                     # the chimaera:plugin WIT version it targets (MAJOR.MINOR)

[detect]                        # workspace-relative; ANY present ⇒ detected
any = [".living/INDEX.md", "MYCELIUM.md"]   # empty/omitted ⇒ always present (on = active)

[requires]
chimaera = ">=0.4.0"            # optional: a semver requirement on the daemon

[requires.agent_plugins.claude] # agent-native plugins it needs, per agent
id = "mycelium@mycelium"
marketplace = "arjunrajlaboratory/mycelium"

[setup]                         # the plugin's OWN documented setup prompt
prompt = "Set up Mycelium in this repository."

[provides]
knowledge = "mycelium"          # a Knowledge provider: its `knowledge` export fills the view
mcp_tools = ["knowledge_search", "knowledge_get"]   # must equal the names `tools()` returns
events = []                     # any of "hook", "session-ended", "switched-on", "switched-off"

[adds]
ui = ["Fills Knowledge and “Where things stand”"]
agents = ["2 read tools for every agent here: knowledge_search · knowledge_get"]

[release]                       # optional: where newer versions are published
github = "owner/repo"
```

(Shipped manifests: `plugins/agent-notes/plugin.toml`, `plugins/mycelium/plugin.toml`;
`[requires] chimaera` and `[release]` above are illustrations, neither ships
them.)

What each part does, and what exists today:

| Part | What it does | Where the host reads it |
|---|---|---|
| `id`, `name`, `summary`, `homepage` | the card; `id` names a directory and a URL segment, so it is charset-gated | `plugins::validate`, `manifest_json` |
| `version`, `api` | which build runs, and the WIT it needs; `api` is a gate | `plugins::gate`, `resolve` |
| `detect.any` | footprint → "active here" (no path component may be a symlink) | `plugins::detect_blocking` |
| `requires.agent_plugins` | per-agent install state (asked of the agents) and an install button running the agent's own `plugin marketplace add` + `install`/`add` in a visible terminal | `agent_probe.rs`, `plugins::install_requirement` |
| `requires.chimaera` | a gate: this daemon's version must match | `plugins::gate` |
| `setup.prompt` | a new chat session of the user's chosen agent, sent this prompt | `plugins::setup_workspace` |
| `provides.knowledge` | the plugin is the Knowledge provider; its `knowledge` export feeds the view and `GET /workspaces/{id}/knowledge` | `knowledge.rs`, `runtime::knowledge` |
| `provides.mcp_tools` | tools served by the chimaera MCP where active, plus the `instructions` paragraph; pre-allowed at spawn | `plugins/tools.rs`, `runtime::offer` |
| `provides.events` | which `on-event` variants the host delivers (none by default); `hook` and `session-ended` are delivered, `switched-on` / `switched-off` are declarable but not delivered yet | `runtime::hook`, `runtime::session_ended` |
| `provides.views` | parses and rides the wire; nothing renders it | none yet |
| `[adds]` | the card's Adds lines | the UI |
| `[release] github` | where the checker and Update look for newer versions | `plugins/releases.rs`, `plugins/installed.rs` |

`settings` and `commands`, sketched in the earlier plan, are not manifest
keys; the first plugin that needs one adds it with a test and a row here.

## The crate

```
plugins/agent-notes/
  Cargo.toml        [lib] crate-type = ["cdylib"]; depends on chimaera-plugin-api
  plugin.toml       the manifest
  src/lib.rs        impl Plugin for AgentNotes { … } + chimaera_plugin_api::export!(AgentNotes)
  src/notes.rs      the pure logic (addressing, unread, the texts), unit-tested natively
```

```toml
[package]
name = "chimaera-plugin-agent-notes"
version.workspace = true        # plugin.toml's `version` must equal this

[lib]
crate-type = ["cdylib"]

[dependencies]
chimaera-plugin-api.workspace = true   # path = "../crates/chimaera-plugin-api" in plugins/Cargo.toml
```

A first-party crate is a member of the `plugins/` cargo workspace, never the
daemon's: a component `cdylib` does not link for the native target on macOS
(its export names contain `#`, which Apple's linker reads as a comment), and
the daemon's lockfile stays free of the guest-side tooling. `cargo check`,
clippy and `cargo test` are fine natively, since the test harness links the
rlib. Nothing publishes `chimaera-plugin-api` to crates.io yet: a crate
outside this repository takes it as a git dependency on the chimaera
repository.

## The `Plugin` trait and `export!`

`chimaera_plugin_api` wraps the generated bindings: implement `Plugin` for a
unit struct, override only what the plugin offers (`tools`, `instructions`,
`call_tool`, `knowledge`, `query`, `on_event`; each has a default), and wire
it with `export!` once at the crate root. JSON arrives parsed
(`serde_json::Value`), and `serde_json` is re-exported. From
`plugins/agent-notes/src/lib.rs`, trimmed to one tool:

```rust
use chimaera_plugin_api::serde_json::{json, Value};
use chimaera_plugin_api::{host, Context, Event, Plugin, ToolDef, ToolResult};

mod notes;

struct AgentNotes;

impl Plugin for AgentNotes {
    fn tools() -> Vec<ToolDef> {
        vec![ToolDef::new(
            "post_note",
            "Leave a short note on the workspace Timeline. `to` is a \
             session id, \"mastermind\", or omitted for everyone. Never \
             starts anyone's turn.",
            json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {"type": "string", "description": "The note (under 2 KB)"},
                    "to": {"type": "string", "description": "Session id or \"mastermind\""},
                },
                "additionalProperties": false,
            }),
        )]
    }

    fn instructions() -> Option<String> {
        Some(INSTRUCTIONS.to_string())
    }

    fn call_tool(cx: Context, name: &str, args: Value) -> ToolResult {
        match name {
            "post_note" => post(&cx, &args),
            other => ToolResult::error(format!("unknown plugin tool {other}")),
        }
    }

    fn on_event(cx: Context, event: Event) -> Option<String> {
        match event {
            // Mail waits to be read: a one-line hint on a carrier that
            // already fires, never a new turn.
            Event::Hook(hook)
                if matches!(hook.name.as_str(), "SessionStart" | "UserPromptSubmit") =>
            {
                let cursor = cursors(&cx).get(&hook.session).and_then(Value::as_u64);
                let recent = recent_notes(&cx).ok()?;
                let (unread, _) =
                    notes::unread(recent, &hook.session, cx.mastermind, cursor.unwrap_or(0));
                notes::hint(unread.len())
            }
            _ => None,
        }
    }
}

chimaera_plugin_api::export!(AgentNotes);

/// post_note {text, to?}
fn post(cx: &Context, args: &Value) -> ToolResult {
    if cx.session.is_none() {
        return ToolResult::error("post_note needs a calling session");
    }
    let body = match notes::message_text(args) {
        Ok(body) => body,
        Err(err) => return ToolResult::error(err),
    };
    let to = match notes::target(args) {
        None => None,
        Some("mastermind") => Some("mastermind".to_string()),
        Some(target) => {
            // Notes never cross workspaces.
            if !host::sessions(cx).iter().any(|s| s.id == target) {
                return ToolResult::error(notes::not_in_workspace(target));
            }
            Some(target.to_string())
        }
    };
    match host::timeline_append(cx, &json!({"kind": "note", "to": to, "text": body})) {
        Ok(seq) => ToolResult::text(notes::posted(seq, to.as_deref())),
        Err(err) => ToolResult::error(err),
    }
}
```

`INSTRUCTIONS`, `cursors` and `recent_notes` are in the same file (the
paragraph, a `host::state_get` of the read cursors, a `host::timeline_recent`
of `note` entries). Note what the plugin does not do: rate-cap posts, cap the
text server-side or fill in who posted. The host does.

The other exports: `knowledge(cx, known)` returns `Ok(None)` when `known` (the
stamp the host holds) is still current, else `Snapshot::new(&stamp, &data)`
(`plugins/mycelium/src/lib.rs` is the example); `query` has no caller yet.
`Context` carries `workspace`, `session` (absent for workspace-level asks such
as `knowledge`) and `mastermind`. The host ignores the `cx` a plugin passes
back and serves the workspace and session of the call in flight, so a plugin
cannot speak for another.

## The host functions and their limits

Every import in `crates/chimaera-plugin-api/wit/chimaera.wit`'s `host`
interface, through the `host::` wrappers (JSON in and out; fallible calls
return the host's reason as a `String`). The limits are in
`crates/chimaera-server/src/plugins/hostfns.rs`:

| Call | What | Limit |
|---|---|---|
| `host::read(&cx, path, cap)` | a workspace file's bytes | workspace-relative only (`..` and absolute paths refused), `O_NOFOLLOW` on every component (no symlink anywhere in the path), at most `cap`, clamped to 8 MiB; off the reactor behind the filesystem semaphore |
| `host::stat(&cx, path)` | size, `mtime_ms`, `is_dir` | the same path rules |
| `host::list(&cx, path, cap)` | a directory's entries (`name`, `is_dir`, `is_symlink`) | the same path rules; at most `cap`, clamped to 4,096 |
| `host::state_get` / `state_put(&cx, key, &value)` | small per-(plugin, workspace) state; `null` removes a key | 64 KiB per plugin per workspace, keys and values; in memory, so a daemon restart clears it; kept across a switch off and on and across versions |
| `host::sessions(&cx)` | the workspace's sessions: `id`, `kind`, `name`, `chat`, `alive`, `mastermind` | at most 256 |
| `host::timeline_append(&cx, &entry)` | appends a Timeline entry; returns its seq | only from a session's call; `{"kind":"note","to":…,"text":…}` and no other kind or field; text ≤ 2 KiB; `to` a session in this workspace, `"mastermind"` or null; 10 posts per session per minute (the window `tell_mastermind` shares) |
| `host::timeline_recent(&cx, &kinds, limit)` | the newest entries of `kinds`, newest first | looks through the newest 200 entries; at most 16 kinds |
| `host::emit(&cx, &event)` | a `{"type":"plugin","plugin":…,"workspace":…, …event}` frame on `/ws/events` | one JSON object ≤ 16 KiB; the host's keys win; a ring of 64 (no UI reads these yet) |
| `host::now_ms()` | wall-clock ms since the epoch | |
| `host::log(level, message)` | a daemon log line tagged with the plugin | 64 lines per call, 2 KiB each |

And around every call (`crates/chimaera-server/src/plugins/runtime.rs`): a
deadline of 5 s (30 s for `knowledge`), 64 MiB of linear memory, WASI with
nothing granted (no files, env, args or network; stderr kept, 4 KiB, only to
explain a trap), one call at a time per (plugin, workspace) instance. A trap
costs the instance, not the daemon; five traps in a minute mark the plugin
faulted in that workspace until the user switches it off and on. What a
plugin returns is capped too: a tool result at 256 KiB, the instruction
paragraph at 8 KiB, a hook line at 1 KiB. A component whose `tools()` names
differ from its manifest's `provides.mcp_tools` is refused.

## Build and test

```sh
bash scripts/build-plugins.sh      # or `just plugins`: every plugins/* crate → plugins/dist/<id>/{plugin.wasm,plugin.toml}
cargo clippy --manifest-path plugins/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path plugins/Cargo.toml          # the plugins' own native unit tests
cargo fmt --all --manifest-path plugins/Cargo.toml
just check                         # all of the above plus the daemon workspace
```

The script needs the `wasm32-wasip2` target (`rust-toolchain.toml` lists it)
and first checks every `plugin.toml` against its crate and the WIT: `version`
must equal the crate's resolved version and `api` the WIT package's
MAJOR.MINOR, or nothing is laid out. Build the plugins before any build of
`chimaera-server`: its embed of `plugins/dist` fails to compile without them.
A debug daemon reads `plugins/dist` from disk once, when its catalog loads, so
after a rebuild restart the daemon (no cargo rebuild needed). A debug build's
first call into a plugin is slow (debug Cranelift compiling the component:
seconds for Mycelium, against about 200 ms in release).

The integration runs in the daemon's tests:

- `crates/chimaera-server/src/tests/plugin_host.rs` — the host itself: the
  deadline trap, the memory cap, panic survival, refused symlinks and `..`,
  the state cap, the append caps and kind allowlist, the fault counter, the
  manifest/tools mismatch refusal, emit frames. It runs against
  `plugins/test-fixture` (one tool per host limit), which the script lays out
  in `plugins/dist-test/`; only the daemon's test builds embed it, and a test
  proves it never reaches the shipped catalog.
- `tests/plugins.rs` (Agent notes: tools only where on, posts and reads stay
  in their workspace, the hook hint, the texts byte for byte),
  `tests/knowledge.rs` (Mycelium: the route JSON and tool texts against
  fixtures), `tests/plugin_updates.rs` (install, update, rollback, remove and
  the checker, against a fake releases server), `tests/agent_view.rs` (the
  plugin-free view, unchanged).

## Shipping it

**First-party** plugins live under `plugins/` and ship inside the daemon
binary: the script builds them, the daemon embeds `plugins/dist` with
rust-embed exactly as it embeds `web-ui/dist`, and they update with chimaera.
To add one: the crate and its `plugin.toml`, a member line in
`plugins/Cargo.toml`, tests as above, a feature page and this guide if it
adds a manifest part, and a live check (switch it on in the isolated preview,
watch a new session get exactly the advertised tools, switch it off, watch
them go). `plugins::tests::every_manifest_parses_and_ids_are_unique` covers
every shipped manifest.

**Third-party** plugins live in their own repository and install on a daemon's
host:

- The repository is also a claude and codex marketplace repository, so its
  agent-side pieces install through the agents' own plugin managers:
  `.claude-plugin/`, `.codex-plugin/`, skills and hooks beside the crate and
  its `plugin.toml`.
- Its manifest names `[release] github = "owner/repo"`, so the daemon can
  check it and the card can offer **Update**.
- Each release is tagged `v<version>` and carries three assets:
  `plugin.wasm` (`cargo build --target wasm32-wasip2 --release`, renamed),
  `plugin.toml` (that version's manifest; its `version` must equal the tag),
  and `SHA256SUMS` in `sha256sum` format listing both.
- The user installs it with `chimaera plugin add owner/repo [--version x]` on
  the daemon's host (or `POST /api/v1/plugins/install {github, version?}`).
  The daemon fetches `SHA256SUMS` and `plugin.toml`, checks the id, the tag's
  version and the gates, streams `plugin.wasm` (16 MiB cap), verifies both
  checksums, and only then makes it current under
  `~/.chimaera/plugins/<id>/<version>/`. It then runs under the same host and
  limits as a first-party plugin, off until switched on.

## Versions and updates, from the author's side

- **Bump `version` in `Cargo.toml` and `plugin.toml` together** (first-party
  crates share the `plugins/` workspace version); the build script refuses a
  mismatch rather than injecting one.
- **Tolerate old state.** Host state and the per-workspace switch follow the
  plugin id, not the version: a new version reads what an older one stored
  (Agent notes' read cursors are the example). A plugin that wants a clean
  slate writes a new key.
- **The gates decide who gets it.** `api` must be a WIT version the daemon
  serves (0.1 today), and `requires.chimaera` (optional) must match the
  daemon. A release that fails either is never offered, and an installed copy
  that stops passing is listed off with its reason ("needs a newer chimaera",
  "needs a newer plugin", "needs chimaera ≥ x"). Set `requires.chimaera` when
  the plugin needs a host import a later daemon added.
- **Updates are never automatic.** The daemon checks each installed plugin's
  release source once after boot and daily (and on **Check now**), reads only
  the release's `plugin.toml`, and offers a version only when it is strictly
  newer and passes the gates. Installing it is the user's click (or
  `chimaera plugin update <id>`). The old version stays as `previous` for
  **Use previous**; at most two versions stay on disk.
- **Changing the interface.** The WIT package version is the contract. Adding
  a host import is a minor bump (0.2 adds `exec` and `watch`); changing or
  removing an export is a new major with a new world, served beside the old
  one for a transition. With any WIT change, bump the package version, the
  API crate's version with it, and the host's `plugins::API`, adding the new
  version to `plugins::SERVED_APIS` beside those it still serves.

## The LaTeX plugin

The LaTeX and Typst plugins are the first new plugins planned on this host.
Their contribution point is `build` (files → a bounded, quiet child process
on the host, diagnostics as editor marks, SyncTeX, a `compile_document` tool
where on), designed in the
[LaTeX and Typst plan](../latex-reports-plan.md#the-plugin-shape). It waits for
WIT 0.2's `exec` import: running an engine is a host call, never something a
plugin does itself.
