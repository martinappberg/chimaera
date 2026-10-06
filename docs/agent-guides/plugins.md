# Writing a workbench plugin

A **workbench plugin** is an opt-in add-on that runs in Chimaera (not inside an
agent CLI) and says exactly what it adds. It is a Rust crate compiled to one
portable WebAssembly component, `plugin.wasm`, beside a `plugin.toml` manifest;
the daemon's plugin host runs it in a sandbox through the `chimaera:plugin` WIT
world. This guide is the recipe. Design and rationale:
[plugin-system-plan.md](../design/plugin-system-plan.md) (the host, the limits,
versions and updates) and
[timeline-knowledge-plugins-plan.md §6](../design/timeline-knowledge-plugins-plan.md)
(the seam and the card). What users see: [features/plugins.md](../features/plugins.md).
The maps: the API crate [chimaera-plugin-api](../../crates/chimaera-plugin-api/AGENTS.md),
the lock and the test fixture [plugins/](../../plugins/AGENTS.md). The first-party
plugins, each its own repository and a real example beside the illustration here:
[chimaera-plugin-mycelium](https://github.com/martinappberg/chimaera-plugin-mycelium) (sandboxed), and
[chimaera-plugin-latex](https://github.com/martinappberg/chimaera-plugin-latex) and
[chimaera-plugin-typst](https://github.com/martinappberg/chimaera-plugin-typst) (API 0.2, privileged: file
views, programs, a downloaded tool, long agent tools).

## Start here: choose the extension seam

In the UI, **Extensions** includes workbench plugins, agent-native plugins,
skills and connections. These are different authoring targets:

| You want to add | Use | Start with |
|---|---|---|
| A workbench view, file preview/action, diagnostics, Knowledge provider, or workspace-scoped agent tool | A workbench plugin: `plugin.toml` + a WebAssembly component | The [starter below](#minimal-api-02-plugin), then the [platform reference](#the-platform-api-02) |
| Instructions, skills, hooks or MCP connections inside an existing agent CLI | That agent's native extension package | Its own packaging/install contract; a workbench plugin may declare it in `requires.agent_plugins` or `recommends.agent_plugins` |
| A new agent harness or structured chat driver | Chimaera's agent integration | [Agent engine map](../../crates/chimaera-agent/AGENTS.md) and [protocol](../../crates/chimaera-agent/PROTOCOL.md); a plugin manifest cannot register a harness |
| A general workbench capability unavailable through host imports | A deliberate change to the host API | [API map](../../crates/chimaera-plugin-api/AGENTS.md), [server map](../../crates/chimaera-server/AGENTS.md), and [interface versioning](#versions-and-updates-from-the-authors-side) |

Keep domain parsing, compilation orchestration and UI trees in the plugin's
own repository. Extend the host only with a reusable capability and tests;
the daemon and client must not branch on a new plugin's id or file format.
Workbench plugins use the host's semantic `ui/1` trees and existing viewers,
rather than shipping arbitrary JavaScript, HTML or a Svelte bundle.

For an agent implementing a plugin, work in this order:

1. Read the plugin repository's own `AGENTS.md`, manifest, API dependency pin
   and tests. In this repository, read the [API map](../../crates/chimaera-plugin-api/AGENTS.md)
   and [fixtures map](../../plugins/AGENTS.md); if changing the host or renderer,
   also read the most specific map and matching `.claude/rules/` files.
2. Write the promised **For you** / **For agents** behavior and choose the
   smallest access, views, events and programs that implement it.
3. Build and inspect capabilities before installing in an isolated daemon.
   Installing, trusting and switching on are separate operations.
4. Verify the visible flow and tools while on, while off, in another workspace
   and after reinstalling the build. Keep pure parsers/state transitions in
   native tests; exercise host imports through the real daemon.
5. Report commands, observed outcomes and any unverified platform or agent.
   Follow the [author checklist](#author-checklist) before releasing.

The current SDK targets **API 0.2**; the host also serves the frozen 0.1 world.
For source questions, the authority is the
[WIT](../../crates/chimaera-plugin-api/wit/chimaera.wit),
[Rust SDK](../../crates/chimaera-plugin-api/src/lib.rs),
[manifest parser](../../crates/chimaera-server/src/plugins/mod.rs),
[platform declarations](../../crates/chimaera-server/src/plugins/platform.rs)
and [screen validator](../../crates/chimaera-server/src/plugins/screens.rs).
The linked design plans explain rationale; a plan does not establish that an
API or command has shipped.

## Minimal API 0.2 plugin

This complete starter exposes one agent tool and one workbench tab without
filesystem, Timeline, session or program access. Create these three files in
a separate `chimaera-plugin-hello/` directory. There is no scaffold generator
in this repository. Replace the dependency path with the absolute path to
your Chimaera checkout while developing locally:

`Cargo.toml`:

```toml
[package]
name = "chimaera-plugin-hello"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
chimaera-plugin-api = { path = "/absolute/path/to/chimaera/crates/chimaera-plugin-api" }
```

`plugin.toml`:

```toml
id = "hello"
name = "Hello"
version = "0.1.0"
summary = "A greeting tab and a greeting tool for agents in this workspace."
api = "0.2"

[access]
files = "none"
timeline = "none"
sessions = "none"

[provides]
mcp_tools = ["hello_greet"]

[adds]
ui = ["A Hello tab with a greeting"]
agents = ["The hello_greet tool replies with a greeting"]

[[views]]
id = "greeting"
title = "Hello"
slot = "tab"
```

`src/lib.rs`:

```rust
use chimaera_plugin_api::serde_json::{json, Value};
use chimaera_plugin_api::{ui, Context, Plugin, ToolDef, ToolResult};

struct Hello;

impl Plugin for Hello {
    fn tools() -> Vec<ToolDef> {
        vec![ToolDef::new(
            "hello_greet",
            "Reply with a greeting.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        )]
    }

    fn instructions() -> Option<String> {
        Some("Use hello_greet when asked to test the Hello plugin.".into())
    }

    fn call_tool(_cx: Context, name: &str, _args: Value) -> ToolResult {
        match name {
            "hello_greet" => ToolResult::text("Hello from this workspace's plugin."),
            other => ToolResult::error(format!("unknown tool {other}")),
        }
    }

    fn render(_cx: Context, view: &str, _args: Value) -> Result<Value, String> {
        match view {
            "greeting" => Ok(ui::tree(ui::stack(vec![
                ui::heading("Hello"),
                ui::text("Hello from this workspace's plugin."),
            ]))),
            other => Err(format!("unknown view {other}")),
        }
    }
}

chimaera_plugin_api::export!(Hello);
```

From the plugin directory, use the same pinned compiler as the current
Chimaera checkout (check `rust-toolchain.toml` when it changes):

```sh
rustup target add --toolchain 1.96.0 wasm32-wasip2
cargo +1.96.0 fmt --all --check
cargo +1.96.0 clippy --all-targets -- -D warnings
cargo +1.96.0 test
cargo +1.96.0 build --release --target wasm32-wasip2
mkdir -p dist
cp target/wasm32-wasip2/release/chimaera_plugin_hello.wasm dist/plugin.wasm
cp plugin.toml dist/plugin.toml
```

Do not use a plain native `cargo build` for the component. Keep `dist/` and
`target/` out of version control. For a published plugin, replace the local
SDK path with a git dependency pinned to a real commit that provides API 0.2
(as in [the crate section](#the-crate)), and commit its Cargo lockfile.

Start the isolated daemon from the Chimaera checkout using
[develop](../../.claude/skills/develop/SKILL.md#isolated-per-worktree-run-coding-agents--use-this-in-a-worktree).
Then run these commands **from that checkout**, replacing the install path
with your plugin's absolute `dist/` path:

```sh
target/debug/chimaera plugin caps /absolute/path/to/chimaera-plugin-hello/dist/plugin.toml
CHIMAERA_HOME="$PWD/.chimaera-dev" target/debug/chimaera plugin add --path /absolute/path/to/chimaera-plugin-hello/dist
CHIMAERA_HOME="$PWD/.chimaera-dev" target/debug/chimaera plugin list
```

The CLI reads the daemon manifest under `CHIMAERA_HOME`; omitting it can
target your normal daemon. A local build may ask you to trust its declared
capabilities. Review them as part of the install flow. In the preview's
**Extensions → Plugins**, switch **Hello** on in a test workspace, expand its
card and use **Open** to show the tab. A new agent session in that workspace
should offer and successfully call `hello_greet` (an agent CLI may display its
own MCP-server prefix). Switch it off and verify that the tool and
view are unavailable there and that another workspace remains unaffected.

For the edit loop, rebuild, copy both files to `dist/`, then repeat the same
isolated `plugin add --path` command. Reinstalling the same version replaces
the bytes and resets the live instance; no daemon restart is needed. A
client sees the new tool set on its next `tools/list`; use a fresh agent
session if its CLI caches tool discovery. If staging `SHA256SUMS`, regenerate
it after every edit or the next install will correctly refuse stale hashes.

## The rules that don't bend

- **A workbench plugin's entry point is a WASM component.** A Rust `cdylib`
  built for `wasm32-wasip2` against `chimaera-plugin-api`, loaded through the
  host contract. Native programs are separate declared jobs. No plugin behaviour is daemon
  code; a plugin that needs something the host doesn't offer needs a new host
  import (a WIT change, below), not a special case in the daemon.
- **The host enforces the component's boundaries.** WebAssembly has no
  direct file, process, environment or network access; it asks through host
  imports. The host enforces deadlines, memory, read/listing caps, state size
  and Timeline rate caps. A plugin with declared programs or downloads is
  **privileged**: its native jobs run with the daemon user's access, outside
  the WASM sandbox. Job resource limits are not filesystem or network
  isolation. Never rely on a plugin to enforce a host boundary;
  pre-checking for a friendlier message is fine.
- **The WIT world is the complete contract.** API 0.2 has six exports in
  `plugin` and three in `screens`; the `Plugin` trait supplies defaults for
  all of them. Removing or changing an export or returned record breaks
  components targeting that world. Keep the frozen 0.1 world and its host
  bindings when evolving 0.2; an older compiled component does not acquire
  a new world simply because the Rust trait has defaults.
- **No host call in a native test.** Built natively (tests, clippy), every
  host import is a wit-bindgen stub that aborts the whole test binary. Keep
  pure logic in functions that take data (the example's `src/pad.rs`) or
  behind a trait the test implements (Mycelium's `src/fs.rs`); the
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
  `agents = […]`, the card's "For you: …" / "For agents: …" sentences in
  plain words. A test fails a locked first-party release that adds nothing.
- **Versions are required.** Every manifest carries `version` (the plugin's
  own, plain `MAJOR.MINOR.PATCH`, equal to its crate's) and `api` (the WIT
  version it targets). A new version tolerates whatever an older one stored.
- **Agent-side pieces ride open standards** (MCP, Agent Skills, the agents'
  own plugin managers) so they keep working outside Chimaera. Never
  reimplement `claude plugin` / `codex plugin`.
- **Login-node discipline.** Detection is a few `stat`s off the reactor and
  cached. Use the host's bounded events/watch set for changes; avoid a
  plugin-specific polling loop. A plugin nobody switches on is never compiled.

## The manifest

`plugin.toml` at the crate root, parsed with `deny_unknown_fields` (a typo is
an error, not a silently ignored key) and validated: the id, the version, the
release source.

```toml
id = "mycelium"                 # stable; lowercase letters, digits, dashes; ≤ 64; "install"/"preview" reserved
name = "Mycelium"
version = "0.1.1"               # the plugin's own, MAJOR.MINOR.PATCH; must equal the crate's
summary = "Project memory your agents record as they work — findings, decisions, learnings."
description = "Mycelium is the Arjun Raj lab's living-repository framework: …"   # optional: a few plain sentences
homepage = "https://github.com/arjunrajlaboratory/mycelium"   # optional; the card's name links here
api = "0.1"                     # the chimaera:plugin WIT version it targets (MAJOR.MINOR): "0.1" or "0.2"

[detect]                        # workspace-relative; ANY present ⇒ detected
any = [".living/INDEX.md", "MYCELIUM.md"]   # empty/omitted ⇒ always present (on = active)

[access]                        # optional: what it reads through the host (below)
timeline = "none"               # "none" | "read" | "notes"; files and sessions: "none" | "read"

[requires]
chimaera = ">=0.4.0"            # optional: a semver requirement on the daemon

[recommends]
summary = "Mycelium's own agent plugin gives claude and codex the skills that record findings, decisions and learnings as they work. Install it for the agents you use; the Knowledge view reads .living/ either way."   # optional: one plain sentence

[recommends.agent_plugins.claude]   # an agent-side plugin that helps, per agent; never needed
id = "mycelium@mycelium"
marketplace = "arjunrajlaboratory/mycelium"
# [requires] summary and [requires.agent_plugins.<agent>] have the same shape, for a genuine hard requirement

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

(Real manifests: the `plugin.toml` at the root of each first-party
repository; each names its own repository in `[release]`, which is where
chimaera installs them from. The host records that source; a manifest alone cannot
grant a copy the maintainer badge.
`[requires] chimaera` above is an illustration; none carries it.
Mycelium v0.1.1 moves its agent plugin from `requires` to `recommends`: the
Knowledge reader works with no agent plugin at all; v0.1.2 adds its
`description` and `[recommends] summary`, the shape above.)

What each part does, and what exists today:

| Part | What it does | Where the host reads it |
|---|---|---|
| `id`, `name`, `summary`, `homepage` | the card; `id` names a directory and a URL segment, so it is charset-gated (and `install` / `preview`, routes' own segments, are taken); the name links to `homepage` (an http(s) URL; opened in the system browser) | `plugins::validate`, `manifest_json` |
| `description` | optional: a few plain sentences from the author — what the plugin is and why a person would switch it on — shown under the summary, clamped to two lines with "more"; blank is omitted (`description: null` on the wire) | `manifest_json` |
| `version`, `api` | which build runs, and the WIT it needs; `api` is a gate | `plugins::gate`, `resolve` |
| `detect.any` | footprint → "active here" (no path component may be a symlink); each path relative, plain components only (no `..`, `.` or absolute path: refused at parse) | `plugins::validate`, `plugins::detect_blocking` |
| `[access]` | what the plugin may read through the host: `files` (`read` / `none`), `timeline` (`none` / `read` / `notes` = read and post notes), `sessions` (`read` / `none`). A 0.1 manifest without it (or a key left out) keeps exactly what 0.1 allowed without saying: files, notes, sessions. The card lists it; `hostfns` refuses what it doesn't allow. Narrow it to what the plugin uses — a new version that asks for more asks the user again | `plugins::capabilities`, `hostfns` |
| `requires.agent_plugins` | a genuine hard requirement (no plugin has one today): per-agent install state (asked of the agents), "Requires the <agent> plugin <id>" on the card for agents installed here, and an install button running the agent's own installer (Claude/Codex marketplace pipeline, Antigravity/Grok native source) in a visible terminal; the attach sheet's step 1; codex hook trust | `agent_probe.rs`, `plugins::install_requirement` |
| `recommends.agent_plugins` | the same shape and the same install route, attach-sheet step and hook trust, for an agent-side plugin that makes this one more useful to the agents the user runs but is never needed: the card's **Agent-side plugin** box, one row per agent installed here ("claude · installed 0.7.2", "codex · not installed [Install]") | `agent_probe.rs`, `plugins::install_requirement` |
| `requires.summary`, `recommends.summary` | optional: one plain sentence saying what the agent-side plugin is for; the box and the attach sheet's step 1 say it above the agents' rows (`requires_summary` / `recommends_summary` on the wire; a plain fallback when absent) | `manifest_json` |
| `requires.chimaera` | a gate: this daemon's version must match | `plugins::gate` |
| `setup.prompt` | a new chat session of the user's chosen agent, sent this prompt | `plugins::setup_workspace` |
| `provides.knowledge` | the plugin is the Knowledge provider; its `knowledge` export feeds the view and `GET /workspaces/{id}/knowledge` | `knowledge.rs`, `runtime::knowledge` |
| `provides.mcp_tools` | tools served by the chimaera MCP where active, plus the `instructions` paragraph; pre-allowed at spawn. Unique names, 1–64 ASCII letters, digits, underscores or dashes — no dots (codex pre-approves a tool by a dotted config key a dot would split); built-in names are reserved | `plugins/tools.rs`, `runtime::offer` |
| `provides.events` | declared events only (none by default): `hook`, `session-ended`, `switched-on`, `switched-off`; API 0.2 also supports file, settings and job events ([events](#events)) | `runtime`, workspace switch handler, `files.rs`, `pdata.rs`, `jobs.rs` |
| `provides.views` | legacy string list; does not declare a platform screen. Use top-level `[[views]]` for screens | `manifest_json`; `platform::validate` for `[[views]]` |
| `[[views]]`, `[[files]]`, `[[actions]]`, `[[settings]]` | API 0.2 screens, file kinds, file actions and declared settings; see the [platform reference](#the-platform-api-02) | `platform.rs`, `screens.rs`, `files.rs`, `pdata.rs` |
| `[[programs]]`, `[[tools]]` | API 0.2 native jobs and optional downloaded tools; either makes the plugin privileged | `platform.rs`, `jobs.rs`, `toolchain.rs` |
| `[adds]` | the card's "For you: …" (`ui`) and "For agents: …" (`agents`) sentences | the UI |
| `[release] github` | where the checker and Update look for newer versions; required for release installs and must match the repository being fetched; the maintainer badge additionally requires a recorded official source or pinned bytes | `plugins/releases.rs`, `plugins/installed.rs`, `plugins/mod.rs` |

What a manifest says is read before anything is installed, too: a card
opened before Install, and a repository previewed from the Extensions tab's
form, show the `description`, `[adds]` and the `[requires]` / `[recommends]`
summaries from the release's own `plugin.toml`, fetched and checksum-verified
by the daemon (`GET /plugins/{pid}/details`, `POST /plugins/preview`) — so
write them for someone deciding whether to install.

Use `[[settings]]` for API 0.2 settings and `[[actions]]` for file actions.
There is no manifest `commands` key; unknown keys are rejected.

### What it can do: the capabilities

The daemon derives one list from the manifest — the plugin's **capabilities**
([platform plan §1](../design/plugin-platform-plan.md#1-capabilities)): its
`[access]`, each agent tool, the hook line (`events = ["hook"]`), being the
Knowledge provider, each agent-side plugin it names (with its marketplace) and
a setup prompt. That list is the card's **Can** row, what a trust prompt asks
about, and (for `[access]`) what the host enforces. Its **digest** (the SHA-256
of the sorted atoms) is what the lock records the maintainers approved and what
a user's trust answer covers. Print both for your manifest, no daemon needed:

```sh
chimaera plugin caps plugin.toml          # tier, digest, the Can list
chimaera plugin caps plugin.toml --json   # the atoms too
```

A release whose digest isn't covered asks the user before it installs or
updates (the running version keeps running): so a new tool, a wider
`[access]` or a new agent-side plugin is a question for your users, and a
release that asks for nothing new is not. A local build (`--path`) asks once
per id and digest, so the rebuild loop asks nothing.

Antigravity and Grok agent-side packages can use `source` instead of `marketplace`:

```toml
[recommends.agent_plugins.agy]
id = "my-agent-kit"
source = "/absolute/path/to/antigravity-kit"

[recommends.agent_plugins.grok]
id = "my-agent-kit"
source = "https://github.com/example/grok-kit.git"
```

`source` is passed as one argument to that agent's native `plugin install`; no shell
expansion or trust flag comes from the manifest. Local sources must exist on the daemon
host. Grok asks the user to trust installation in its visible terminal before executing it.
Do not reuse a Claude/Codex marketplace automatically: publish a compatible package for each
agent you declare. The UI does not offer installation for an unknown provider or a new
provider without a source. Native skills, hooks, permissions and MCP configuration retain
their own semantics. This manifest contribution installs an add-on inside an existing agent;
it does not register a new agent harness with Chimaera.

## The crate

The example throughout is an illustration, a small **Scratchpad** plugin:
one agent tool that adds a line to a per-workspace list, and a hint on the
hook the agent already fires. The first-party repositories are the real
thing.

```
chimaera-plugin-scratchpad/       its own repository
  Cargo.toml          [lib] crate-type = ["cdylib"]; depends on chimaera-plugin-api
  plugin.toml         the manifest
  rust-toolchain.toml the pinned toolchain, with the wasm32-wasip2 target
  src/lib.rs          impl Plugin for Scratchpad { … } + chimaera_plugin_api::export!(Scratchpad)
  src/pad.rs          the pure logic (the line's checks, the kept length, the texts), unit-tested natively
  .github/workflows/  ci.yml (fmt, clippy, tests, the wasm build) · release.yml (the three assets)
```

```toml
[package]
name = "chimaera-plugin-scratchpad"
version = "0.1.0"               # plugin.toml's `version` must equal this (and the tag)

[lib]
crate-type = ["cdylib"]

[dependencies]
chimaera-plugin-api = { git = "https://github.com/martinappberg/chimaera.git", rev = "<commit>" }
```

A plugin crate is its own cargo workspace, never a member of the daemon's: a
component `cdylib` does not link for the native target on macOS (its export
names contain `#`, which Apple's linker reads as a comment), and the daemon's
lockfile stays free of the guest-side tooling. `cargo check`, clippy and
`cargo test` are fine natively, since the test harness links the rlib.
Nothing publishes `chimaera-plugin-api` to crates.io yet: a plugin takes it as
a git dependency on the chimaera repository, pinned to a commit so a release
is reproducible.

## The `Plugin` trait and `export!`

`chimaera_plugin_api` wraps the generated bindings: implement `Plugin` for a
unit struct, override only what the plugin offers (`tools`, `instructions`,
`call_tool`, `knowledge`, `query`, `on_event`; each has a default), and wire
it with `export!` once at the crate root. JSON arrives parsed
(`serde_json::Value`), and `serde_json` is re-exported. The example's
`src/lib.rs`:

```rust
use chimaera_plugin_api::serde_json::{json, Value};
use chimaera_plugin_api::{host, Context, Event, Plugin, ToolDef, ToolResult};

mod pad;

struct Scratchpad;

impl Plugin for Scratchpad {
    fn tools() -> Vec<ToolDef> {
        vec![ToolDef::new(
            "pad_add",
            "Add a line to this workspace's scratchpad, for you and the other \
             agents here to read later.",
            json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {"type": "string", "description": "The line (under 500 characters)"},
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
            "pad_add" => add(&cx, &args),
            other => ToolResult::error(format!("unknown plugin tool {other}")),
        }
    }

    fn on_event(cx: Context, event: Event) -> Option<String> {
        match event {
            // A one-line hint on a carrier that already fires, never a new
            // turn (the manifest declares `events = ["hook"]`).
            Event::Hook(hook) if hook.name == "SessionStart" => pad::hint(lines(&cx).len()),
            _ => None,
        }
    }
}

chimaera_plugin_api::export!(Scratchpad);

/// pad_add {text}
fn add(cx: &Context, args: &Value) -> ToolResult {
    let line = match pad::line_text(args) {
        Ok(line) => line,
        Err(err) => return ToolResult::error(err),
    };
    let mut all = lines(cx);
    all.push(json!({"text": line, "at": host::now_ms(), "by": cx.session}));
    pad::keep_newest(&mut all);
    match host::state_put(cx, "lines", &Value::Array(all)) {
        Ok(()) => ToolResult::text(pad::added(&line)),
        Err(err) => ToolResult::error(err),
    }
}

/// The kept lines; none yet is an empty list.
fn lines(cx: &Context) -> Vec<Value> {
    match host::state_get(cx, "lines") {
        Ok(Some(Value::Array(all))) => all,
        _ => Vec::new(),
    }
}
```

`INSTRUCTIONS` is in the same file (the paragraph an agent gets at
`initialize`). Note what the plugin does not do: cap its state or the hint
line, or fill in which workspace a line belongs to. The host does (state is
per plugin and workspace, 64 KiB; a hook line ≤ 1 KiB); `keep_newest` is the
plugin's own choice of how much to keep.

The other exports: `knowledge(cx, known)` returns `Ok(None)` when `known` (the
stamp the host holds) is still current, else `Snapshot::new(&stamp, &data)`
(Mycelium's `src/lib.rs` is the example); `query` serves the authenticated UI
query route and paginated `ui/1` lists/tables.
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
| `host::timeline_append(&cx, &entry)` | appends a Timeline entry; returns its seq | only from a session's call; `{"kind":"note","to":…,"text":…}` and no other kind or field; text ≤ 2 KiB; `to` a session in this workspace, `"mastermind"` or null; 10 posts per session per minute (the window agent messages share); shown on the Timeline, never delivered into an agent |
| `host::timeline_recent(&cx, &kinds, limit)` | the newest entries of `kinds`, newest first | looks through the newest 200 entries; at most 16 kinds |
| `host::emit(&cx, &event)` | a `{"type":"plugin","plugin":…,"workspace":…, …event}` frame on `/ws/events` | one JSON object ≤ 16 KiB; the host's keys win; a ring of 64; routed to the client's platform-frame listeners. Prefer `invalidate` or `publish` for standard screen/surface updates |
| `host::now_ms()` | wall-clock ms since the epoch | |
| `host::log(level, message)` | a daemon log line tagged with the plugin | 64 lines per call, 2 KiB each |

And around every call (`crates/chimaera-server/src/plugins/runtime.rs`): a
deadline of 5 s (30 s for `knowledge`), 64 MiB of linear memory across all
memories, 65,536 table elements in total, at most 16 memories, 16 tables and
64 core instances per component, WASI with
nothing granted (no files, env, args or network; stderr kept, 4 KiB, only to
explain a trap), one call at a time per (plugin, workspace) instance. A trap
costs the instance, not the daemon; five traps in a minute mark the plugin
faulted in that workspace until the user switches it off and on. What a
plugin returns is capped too: a tool result at 256 KiB, the instruction
paragraph at 8 KiB, a hook line at 1 KiB, a tool description at 2 KiB. A
component whose `tools()` names differ from its manifest's
`provides.mcp_tools` is refused, and so is one whose tool input schema isn't a
JSON object schema (`"type": "object"`, ≤ 16 KiB): agents' MCP clients
validate the whole tool list, so one bad schema would cost them every
chimaera tool. A call whose caller stops waiting (a hook the agent gave up on,
a closed window) drops its instance; the next call starts a fresh one and
nothing counts as a failure.

Knowledge stamps and Timeline baselines are scoped to the provider id and
component digest: an update, rollback or provider change gets a fresh snapshot
even when its input files are unchanged.

A snapshot's ids may repeat — real `.living/` repositories reuse numbers.
The UI keys its lists by a client-side `key` (never the id), and Timeline
attribution takes the first entry with an id as the one meant, so a provider
need not dedupe. An addendum (`### F-027 addendum:` under F-027) is part of
its finding, not another one: Mycelium (≥ 0.1.3) lists it in the finding's
optional `addenda: [{label, title, text, line}]`, which the Knowledge view
renders under the finding.

## Build and test

In the plugin's repository:

```sh
cargo build --release --target wasm32-wasip2   # the component, target/wasm32-wasip2/release/<crate>.wasm
cargo clippy --all-targets -- -D warnings
cargo test                                     # the plugin's own native unit tests
cargo fmt --all
```

To run a build in a daemon before it has a release, put `plugin.wasm` (the
component, renamed) and `plugin.toml` in a directory and install it from there
on the daemon's host:

```sh
mkdir -p /tmp/scratchpad-dev
cp target/wasm32-wasip2/release/chimaera_plugin_scratchpad.wasm /tmp/scratchpad-dev/plugin.wasm
cp plugin.toml /tmp/scratchpad-dev/plugin.toml
# From the Chimaera checkout, after starting its isolated daemon:
CHIMAERA_HOME="$PWD/.chimaera-dev" target/debug/chimaera plugin add --path /tmp/scratchpad-dev
```

The authenticated REST equivalent is `POST /api/v1/plugins/install` with
`{"path": "/tmp/scratchpad-dev"}`. The directory must exist on the daemon's
host, including when the UI connects remotely. The daemon copies both files
(and a `SHA256SUMS`, when the directory has one: both files must then match it)
into `<CHIMAERA_HOME>/plugins/<id>/<version>/` (`~/.chimaera` by default),
notes the directory in `local-path`, and gates the manifest like any other.
Installing the same version again replaces it in place, so the loop is:
rebuild, copy, `chimaera plugin add --path <dir>`, and a running session's
next `tools/list` is the new build. Switch the plugin on in a workspace to see
it; the card notes the copy as a "local build". A debug daemon has no plugins until one is
installed (from the Extensions tab, `chimaera plugin add <id>`, or `--path`),
and its first call into a plugin is slow (debug Cranelift compiling the
component: seconds for Mycelium, against about 200 ms in release).

In the chimaera repository, only the tests need plugin builds:

```sh
bash scripts/build-plugins.sh      # or `just plugins`: the fixture (and its v2) and the locked releases → plugins/dist-test
just check                         # the daemon workspace and the fixture's; runs the script first
```

The script builds the test fixture into `plugins/dist-test/`, and downloads
each locked release's `plugin.wasm`, `plugin.toml` and `SHA256SUMS` into
`plugins/dist-test/<id>/`, verified against the lock and kept there (a second
run re-verifies them and fetches nothing). The daemon's tests install those by
path. Building or running the daemon doesn't need the script: the daemon
embeds no plugin bytes, only `plugins/plugins.lock`.

The integration runs in the daemon's tests:

- `crates/chimaera-server/src/tests/plugin_host.rs` — the host itself: the
  deadline trap, the memory cap, panic survival, refused symlinks and `..`,
  the state cap, the append caps and kind allowlist, the fault counter, the
  manifest/tools mismatch refusal, emit frames. It runs against
  `plugins/test-fixture` (one tool per host limit), which the script builds
  into `plugins/dist-test/`; only the daemon's test builds embed it.
- `tests/plugins.rs` (Mycelium as the installed plugin: available until
  installed, tools offered and call-gated only where it is on and its
  footprint is present),
  `tests/knowledge.rs` (Mycelium: the route JSON and tool texts against
  fixtures), `tests/plugin_updates.rs` (the three install kinds, update,
  rollback, remove and the checker, against a fake releases server),
  `tests/plugin_retired.rs` (a plugin built into Chimaera now: listed with
  why, never run, never installed again),
  `tests/agent_view.rs` (the plugin-free view, unchanged).

## What the card says about a plugin

What the card shows is the daemon's, never the plugin's own claim:

- **The check badge** ("Verified by the Chimaera maintainers"; `first_party`
  on the wire): the plugin's id is in `plugins/plugins.lock` — the curated
  list the maintainers keep, whose versions follow each repository's releases
  automatically (CI-gated) — and the installed copy's `[release] github`
  matches the lock's `repo` (case-insensitive). The bytes must also match the
  lock or come from a release install whose source the host recorded in
  `source-github`. Its standing must also remain `verified`: a sandboxed
  update needs the lock's approved capability digest; a privileged build
  needs the pinned version and verified bytes. Local installs do not copy
  that marker, so a rebuilt copy cannot assert the badge.
- **"local build"**: the copy was installed from a directory (`--path`;
  `local_path` on the wire).
- **Can**: what it can do, in the daemon's words (`can` on the wire), for
  every plugin, verified or not — and, when it can't run on this host, why
  (`hold`: waiting for the user's trust, blocked by Chimaera, or the host's
  policy). A sandboxed first-party update keeps the badge only while its
  capability digest is the one the lock recorded (`caps`); one that asks for
  more is the user's to trust, and loses the badge. Privileged builds require
  the lock's pin even when their capability digest stays the same.
- **Nothing about checksums.** The safety mechanism is the daemon's: it
  checks every download against the release's `SHA256SUMS` (and a
  first-party one against the lock's two sha256s), keeps that file beside the
  installed copy and re-checks the copy every time it loads the catalog
  (`verified` on the wire). Only a failure reaches the card, as its fault
  line: the copy is listed, never loaded.

## Shipping it

**First-party** plugins live in their own repositories and are named, with
one pinned release each, in `plugins/plugins.lock`. They are the worked
examples: [chimaera-plugin-mycelium](https://github.com/martinappberg/chimaera-plugin-mycelium), [chimaera-plugin-latex](https://github.com/martinappberg/chimaera-plugin-latex) and [chimaera-plugin-typst](https://github.com/martinappberg/chimaera-plugin-typst). Each is the crate and its `plugin.toml` (naming
the repository as `[release] github`), a CI workflow (fmt, clippy, tests, the
wasm build) and a release workflow that, on a `v<version>` tag whose version
equals `Cargo.toml`'s and `plugin.toml`'s, builds `plugin.wasm` and publishes
it with `plugin.toml` and `SHA256SUMS`. The lock names, per plugin, `id`,
`name` and `summary` (what the card shows before anything is installed), the
pinned `version`, `repo`, and the release's two sha256s (`sha256_wasm`,
`sha256_toml`). The daemon embeds the lock, never a plugin's bytes: it lists
each lock entry as available, and **Install** (or `chimaera plugin add <id>`)
downloads that release from `https://github.com/<repo>/releases/download/v<version>/`
and checks both files twice, against the release's `SHA256SUMS` and against
the lock, refusing any other bytes.

A first-party bump:

1. In the plugin's repository, bump `version` in `Cargo.toml` and
   `plugin.toml` together, merge, and push the tag `v<version>`: the release
   workflow publishes the three assets.
2. Daemons that have it installed offer it: the checker asks every
   installed plugin whose manifest names `[release]`, and **Update** (or
   `chimaera plugin update <id>`) installs it. A first-party copy updated past
   the pin keeps its check badge only when sandboxed and its capabilities
   still match the lock; privileged updates require review/trust until pinned.
3. The chimaera repository follows on its own: within the hour, the
   `plugin-lock` workflow checks the new release (`SHA256SUMS` against the
   downloaded bytes; the manifest's id, version and `[release] github`
   against the lock), rewrites the entry in `plugins/plugins.lock` and opens
   `fix: update <Name> to <version>`. Auto-merge is limited to sandboxed
   releases whose capability declarations match the pinned release; other
   bumps wait for a maintainer to approve `tier` and `caps`. CI installs the
   release against the new lock, and the merge requests a patch release: from
   that release (the next daily batch), **Install** fetches that version. Details, the one-time token and how to
   turn a version down: [plugins/AGENTS.md](../../plugins/AGENTS.md).

To add a first-party plugin: a repository shaped like the examples, its first release, a
`[[plugin]]` in the lock, the daemon-side tests (above), a feature page and
this guide if it adds a manifest part, and a live check (install it in the
isolated preview, switch it on, watch a new session get exactly the
advertised tools, switch it off, watch them go).
`plugins::tests::every_locked_release_is_what_the_lock_says` checks each
locked release the script downloaded against the lock: both sha256s, the id,
the version, the name, `[release] github` equal to the lock's `repo`, the
gates, and that it says what it adds.

**Third-party** plugins live in their own repository and install on a daemon's
host:

- If it also ships agent-side pieces, package them for each supported agent's
  own plugin manager (for Claude/Codex, their marketplace metadata, skills
  and hooks). A workbench-only plugin needs no agent marketplace package.
- Its manifest names `[release] github = "owner/repo"`, so the daemon can
  check it and the card can offer **Update**.
- Each release is tagged `v<version>` and carries three assets:
  `plugin.wasm` (`cargo build --target wasm32-wasip2 --release`, renamed),
  `plugin.toml` (that version's manifest; its `version` must equal the tag),
  and `SHA256SUMS` in `sha256sum` format listing both.
- The user installs it with `chimaera plugin add owner/repo [--version x]` on
  the daemon's host, **Install from a repository** in the Extensions tab, or
  `POST /api/v1/plugins/install {github, version?}`. The daemon fetches
  `SHA256SUMS` and `plugin.toml`, checks the id, the tag's version and the
  gates, streams `plugin.wasm` (16 MiB cap), verifies both checksums, and
  only then makes it current under `~/.chimaera/plugins/<id>/<version>/`,
  with the release's `SHA256SUMS` beside the two files (re-checked each time
  the catalog loads it). It then runs under the same host and limits as a
  first-party plugin, off until switched on. It never gets the check badge.

## Versions and updates, from the author's side

- **Bump `version` in `Cargo.toml` and `plugin.toml` together**, and tag
  that version; the first-party release workflow refuses a tag, crate and
  manifest that disagree rather than injecting one, and chimaera refuses to
  install a first-party release whose manifest isn't the lock's version.
- **Tolerate old state.** Host state and the per-workspace switch follow the
  plugin id, not the version: a new version reads what an older one stored
  (the example's `lines` key). A plugin that wants a clean slate writes a new
  key.
- **The gates decide who gets it.** `api` must be a WIT version the daemon
  serves (0.1 and 0.2 here), and `requires.chimaera` (optional) must match the
  daemon. A release that fails either is never offered, and an installed copy
  that stops passing is listed off with its reason ("needs a newer chimaera",
  "needs a newer plugin", "needs chimaera ≥ x"). Set `requires.chimaera` when
  the plugin needs a host import a later daemon added.
- **Updates are never automatic.** The daemon checks the release source of
  each installed plugin whose manifest names `[release]` once after boot and
  daily (and on **Check for updates**), reads only the release's `plugin.toml`, and
  offers a version only when it is strictly newer than the one that runs and
  passes the gates. Installing it is the user's click (or
  `chimaera plugin update <id>`). The old version stays as `previous` for
  **Use previous**; at most two versions stay on disk. **Remove** deletes
  every installed version; a first-party plugin is then listed as available
  again.
- **Changing the interface.** The WIT package version is the contract. Adding
  a host import needs a new package version; changing or removing an export
  requires a new compatible host binding/world strategy, served beside the
  old one for a transition. API 0.2 is implemented (see the
  [platform plan](../design/plugin-platform-plan.md#11-the-interface-wit-02) for its
  design). With any WIT change, bump the package version, the
  API crate's version with it, and the host's `plugins::API`, adding the new
  version to `plugins::SERVED_APIS` beside those it still serves.

## The platform: API 0.2

A plugin that says `api = "0.2"` gets the platform
([plan](../design/plugin-platform-plan.md) §3–§9): screens in Chimaera's own format,
file kinds and file actions, data surfaces core draws, file events, an output
folder, declared settings and durable state. The host serves 0.1 beside it
unchanged (its own bindings, `wit-0.1/`), so moving is a choice: bump the
`chimaera-plugin-api` dependency and `api`, and **declare `[access]`** — in 0.2
a key left out means none (0.1 implied files, the Timeline with notes, and
sessions). The new trait methods have defaults, so existing 0.1 source can
often be rebuilt against 0.2 without new method implementations; the rebuilt
component must declare `api = "0.2"`. Existing 0.1 components keep using
the frozen world. Programs, side-program downloads and long agent tools are in
[their own section](#programs-jobs-and-tools); they make a plugin privileged.

```toml
api = "0.2"

[access]
files = "read"

[provides]
events = ["file-saved", "file-changed", "settings-changed", "switched-on", "switched-off"]

[[views]]                  # a screen: slot tab | panel | file | status | card
id = "document"
title = "Document"         # required: every view has a name
slot = "file"

[[files]]                  # files matching `match` open in `view` (Text one click away)
match = ["*.tex", "*.ltx"] # a pattern without `/` matches the name anywhere; `**` spans folders
view = "document"          # must be a `slot = "file"` view
label = "LaTeX"
debounce_ms = 300          # how long a burst of changes settles (≤ 5000)

[[actions]]                # a file toolbar item; its click calls on_action("", action, {"file"})
match = ["*.md"]
label = "Export PDF"
action = "export-pdf"      # never a built-in action's name (below)

[[settings]]               # drawn in Settings → Plugins and on the card
key = "engine"
type = "enum"              # bool | enum | string | number | path
options = ["pdflatex", "xelatex"]
default = "pdflatex"
label = "Engine"
scope = "workspace"        # workspace (default) | host
```

Claiming a file kind is on the card's **Can** list ("Opens *.tex files in its own
view"), so a release that claims a new kind asks its users again.

### The exports and imports

`Plugin` gains `render(cx, view, args)` (the view's tree; `args` carries
`file` for a file view, `width` `narrow`/`wide`, `slot`), `on_action(cx, view,
action, payload)` (the new tree, or `None` to keep it) and `tool_resume` (a
long agent tool's final answer, [below](#programs-jobs-and-tools)). `platform::`
has what 0.2 adds:

| Call | What |
|---|---|
| `output_read(cx, path, offset, cap)` · `output_list` · `output_write` · `output_remove` | the plugin's output folder for this workspace (`output:<path>`), outside the repository; reads ≤ 8 MiB a call from an offset, its own writes ≤ 8 MiB a file, 1 GiB per plugin (the oldest top-level entries go past it); never through a link |
| `publish(cx, surface, key, &data)` · `unpublish` | a data surface (below); checked, kept, announced to this workspace's windows |
| `invalidate(cx, view)` | windows showing the view render it again (≤ 4 a second; a burst is one) |
| `watch(cx, &paths)` | up to 256 workspace paths heard as `file-changed`; swept every 5 s only while one of its views was rendered in the last 10 minutes |
| `setting(cx, key)` | a declared setting: the user's value, else its default |
| `state_keep(cx, key, &value)` | durable state, read back with `host::state_get`; within the same 64 KiB. `state_put` stays memory-only (and makes a kept key memory-only again) |
| `roots(cx)` | the absolute workspace root and output folder (for arguments a program will need, and for mapping printed paths back) |
| `job_start(cx, &spec)` · `job_status` · `job_cancel` | run a declared program as a job ([below](#programs-jobs-and-tools)) |
| `tool_state(cx, tool)` | a declared tool: its version, whether (and which version) is installed here, what Install would download, `installing` and its `progress` (`{stage, done, total}`) while it installs |

### Screens: `ui/1`

A view's tree is `{"ui": "1", "root": <node>}`; a node is `{"type": …,
props…, "children": […]}` (`chimaera_plugin_api::ui` has helpers). Props are
semantic, never visual: `tone` is `neutral | accent | good | warn | bad`, `size`
`small | large`, `gap` `small | large`. The daemon checks every tree before a
window sees it (≤ 256 KiB, ≤ 5,000 nodes, ≤ 200 rows a list or table); a bad one
is not drawn, and the view says so with each problem's JSON path (also in the
daemon log).

| Node | Props (required in bold) |
|---|---|
| `stack`, `row`, `grid`, `card` | `children`; `gap`; row `align` (`center`, `end`, `between`); grid `columns` (1–6); card `title` |
| `split` | **`children`** (two); `ratio` (0.1–0.9) — stacked in a narrow window |
| `tabs` | **`tabs`**: `[{title, children}]` |
| `section` | **`title`**, `children`, `collapsed` |
| `divider` | — |
| `text` | **`text`**, `tone`, `size`, `emphasis`, `mono` |
| `heading` | **`text`**, `level` (1–3) |
| `markdown` | **`text`** (chat's renderer and its sanitizing) |
| `code` | **`text`**, `language`; syntax colors through the shared extension/Mod code renderer, bounded to 100,000 characters or 2,000 lines with a truncation notice |
| `keyvalue` | **`items`**: `[{key, value, tone?}]` |
| `badge` | **`text`**, `tone` |
| `status` | **`text`**, `state` (`idle busy ok warn bad`: a spinner for busy, a check for ok), `detail` (muted, beside it) — one pill for a process's state, a build's |
| `icon` | **`name`** (`check x alert info clock file folder play stop refresh download external grid list book bolt settings search`), `label`, `tone` |
| `progress` | `value` (0–1; absent: indeterminate), `label` |
| `empty` | **`title`**, `text`, `action` `{label, action, payload}` |
| `callout` | **`text`**, `title`, `tone`, `actions` (buttons beside it: a notice with its fix) |
| `list` | **`items`**: `[{title, subtitle?, badges?, actions?: [nodes], action?, payload?, file?, line?}]`; `more` `{query, args}` pages through `query` (it answers `{items, more?}`) |
| `table` | **`columns`** `[{key, title, align?}]`, **`rows`** `[{<key>: text}]`; `more` as for a list (`{rows, more?}`) |
| `file` | **`path`** (a workspace path or `output:<path>`), `label`, `line` — a card that opens it |
| `link` | **`text`**; `href` (http/https, opens outside) or `file` + `line` |
| `image` | **`src`**, **`alt`** |
| `button` | **`label`**, **`action`**, `payload`, `tone`, `icon`, `disabled`, `title` (its tooltip) |
| `toggle` | **`label`**, **`name`**, `value`, `action` (sends `{value}` beside the payload) |
| `select` | **`label`**, **`name`**, **`options`** (`[{value, label}]` or strings), `value`, `action` |
| `segmented` | **`name`**, **`options`** (`[{value, label, icon?, title?}]` or strings), `value`, `action` (sends `{value}` beside the payload), `label` — the app's own switch (Split · Source · PDF) |
| `textfield` | **`label`**, **`name`**, `value`, `placeholder`, `multiline` |
| `form` | **`action`**, `children` (its fields), `submit`; sends `{form: {name: value}}` beside the payload |
| `editor` | **`path`** — the file in the app's own editor (saves, merges) |
| `pdf` | **`src`** — the app's PDF viewer |
| `log` | **`src`** — the app's viewer for that file (a `.log` streams from its tail) |
| `diagnostics` | `file`; `key` (one of your surface keys only: a document's problems); `mine` (only yours); `quiet` (draws nothing while there is nothing to say); `compact` (a shorter list); `title` — the problems list from every active plugin's `diagnostics/1`: counts in its head, each row opens its place (**Go to**) with **Ask agent**, `info`/`hint` items (a LaTeX box) behind a "Show N layout notes" toggle |
| `diff` | `before` + `after`, or `path` + `base` (`head`, `index`, `rev:<ref>`, `output:<path>`); `mode` `prose` (default: words within a changed line) or `code` |

A node this chimaera doesn't know draws its `fallback` (a node, or `"drop"`),
else a quiet "needs a newer chimaera" with its children. Actions a plugin names
but never handles (the app carries them out): `open-file {file, line?}`,
`open-view {view}`, `open-url {url}`, `copy {text}`, `save-to-workspace {from,
to}` (the user's click copies an output file into the workspace; asks before
replacing), `ask-agent {file?, line?, text}`, `install-tool {tool}` (the user's
click installs one of your `[[tools]]`, as the card's Install does: a progress
bar with its stage and megabytes sits at the top of your screen meanwhile, the
view draws again at once — `tool_state` then says `installing` — and when it is
done).

Where a view draws: `tab` (its own tab: the card's **Open**, `open-view`, a file
action's `open`), `panel` (the dashboard, after core's sections), `file` (a
claimed file; the view owns the pane: the root stack's last child grows to its
height, and an `editor` or `pdf` first in a split pane, a stack or a tab fills
it — an embedded viewer shows no path bar), `status` (a chip in a claimed file's bar), `card` (inside its
Extensions card). The UI renders only on open, on an action, and on
`invalidate`; nothing polls.

### Data surfaces

Data core draws with its own views; `publish` checks the shape:

| Surface | Shape |
|---|---|
| `diagnostics/1` | `{items: [{file, severity (error warning info hint), line (from 1), column?, end_line?, end_column?, message, context?, source?}]}`, ≤ 200 per file, ≤ 2,000 per key |
| `output/1` | `{source, output, state (building ok errors failed), label?, finished_ms?, changed_pages?, log?}` |
| `sourcemap/1` | `{output, files: [path], records: [[file, line, page, x, y, width, height]]}`, ≤ 4 MiB, kept in the output folder |
| `knowledge/1` | the Knowledge snapshot (see the [coordination section](../design/plugin-platform-plan.md#coordination-the-knowledge-redesign-2026-09-29)), ≤ 4 MiB |
| `references/1` | ids it answers for: `{shapes: [{kind, pattern}], ids: [{id, key, kind, title, span?: {path, line, end_line}, view?}]}` — 1–16 shapes, each a regex source of ≤ 80 bytes with no groups or anchors; ≤ 5,000 ids a key; ≤ 4 MiB, kept in the output folder. Every id with a `span` or a `view` becomes a chip where its shape matches in chats and Knowledge: hover previews the span, a click opens it (or the plugin's view). `end_line` 0 means to the end of the file |

Paths are workspace-relative or `output:<path>`. A window asks
`GET /workspaces/{id}/surfaces/{kind}/{version}?file=` and hears a `surface`
frame when one changes.

### Events

`file-saved(path)` (the editor saved a file you claim), `file-changed(path)` (a
claimed or watched file changed: an agent's write, a file operation, or the
sweep), `settings-changed(key)`, `switched-on`, `switched-off` (on the instance
it had, before it goes) — each only if declared in `provides.events`, and file
events debounced per file. Declaring one of the 0.2 events needs `api = "0.2"`.

### Programs, jobs and tools

A plugin may run programs on the host, and download the ones it needs
([plan](../design/plugin-platform-plan.md) §6, §8). Either makes it **privileged**: the
card says "runs programs" and lists each one, a shell (`sh`, `bash`, `python`,
`node`, `env`, …) gets "Runs sh: this plugin can run any command on this host",
and a first-party privileged plugin is verified only at the lock's pin, since
every release is reviewed.

```toml
[[programs]]               # only declared names run, by name, never a path
name = "latexmk"
version = ["-v"]           # the arguments that print its version

[[programs]]
name = "tlmgr"
network = "CTAN mirrors (TeX Live packages)"   # what it reaches: said on the card, not enforced

[[tools]]                  # a side program the host downloads on the user's click
id = "tinytex"
name = "TeX Live (TinyTeX)"
version = "2026.09"
programs = ["latexmk", "pdflatex"]   # each also a [[programs]] entry
home = "https://github.com/rstudio/tinytex-releases"

[[tools.artifacts]]        # one per platform: linux-x86_64 linux-aarch64 macos-x86_64 macos-aarch64
platform = "linux-x86_64"
url = "https://github.com/…/releases/download/v2026.09/TinyTeX-1.tar.xz"  # https, a fixed release
sha256 = "…"               # required; checked while it streams
size = 159_000_000         # the download stops past it (and past 2 GiB)
unpack = "tar.xz"          # tar | tar.gz | tar.xz | zip | none (the file is the program, named after the tool's first program)
bin = "TinyTeX/bin/x86_64-linux"     # where its programs are, inside the folder

[[tools.setup]]            # run once after unpacking, as jobs; this tool's programs only
program = "tlmgr"
args = ["install", "latexmk"]

[provides]
events = ["job-finished"]
```

A URL that moves (`/latest/`, `/daily/`, `/nightly/`, `/main/`, …) doesn't
validate: the manifest's sha256 must stay true.

**A job** is `platform::job_start(cx, &json!({…}))` with `program` (declared),
`args` (a list; there is no shell), `cwd` (a workspace path, or `output:` for
the output folder), `env` (added variables, portable names in either case; never `PATH`,
`HOME`, `SHELL`, `USER`, `LD_*`, `DYLD_*`, `CHIMAERA_*`), `stdin` (≤ 4 MiB,
else closed), `wall_s` (60 by default, ≤ 600), `label` (the UI's words),
`priority` (`user`, `agent`, `background`) and `prefer` (`"tool:<id>"`: this
plugin's own copy even when the user has one; otherwise the user's copy on the
PATH their terminals get, host and workspace prelude included, wins). It answers
the job's id at once; the host runs it:

| Limit | What |
|---|---|
| Queue | 2 jobs running daemon-wide, 1 per plugin, 8 waiting per plugin (a 9th is refused), by priority then age |
| Time | `wall_s`, then SIGTERM to the whole process group and SIGKILL 5 s later |
| Memory, CPU, files | `ulimit -v` 4 GiB, `-t` the wall time plus slack, `-f` 256 MiB per written file; `nice -n 10`, idle I/O where there is `ionice` |
| Output | `output:.jobs/<id>/stdout.log` and `stderr.log`, 16 MiB each |
| Switched off, blocked | its jobs are cancelled |

`job_status(cx, id)` answers `{id, state (queued running done), program,
from (`path`: the user's copy; `tool:<id>`: one of yours), label, exit, timed_out, cancelled, error, queued_ms, started_ms, finished_ms,
duration_ms, stdout, stderr}` for this plugin's jobs in this workspace. When
one ends the plugin hears `job-finished {id, exit, timed_out, duration_ms}`
(with the 30 s budget, so it can digest a large output), and windows get a
`job` frame.

**A long agent tool** starts a job and answers `ToolResult::wait(job_id,
"still building")`. The host holds the agent's call until the job ends (at most
45 s) and calls `tool_resume(cx, name, job)` for the final answer; past that
the agent gets the `text` given with `wait`, so say how to check back.

**Tools** install only on the user's click (the card's **Tools** section:
Install, Update, Remove; before install its **Downloads** line says what and
from where). The host downloads over https, checks the size and sha256 while it
streams, unpacks into an empty folder (refusing absolute paths, `..`, hard
links, devices, links that leave the folder, and writes through a link; ≤ 4 GiB
and 200,000 entries), runs the setup steps as jobs, and keeps
`~/.chimaera/tools/<plugin>/<tool>/<version>/` behind a `current` link (two
versions at most; the host setting `plugins.toolsDir` moves that root, and an
install first checks for room: the download, about three times it unpacked,
and 1 GB to spare). The host confines unpacking to that folder and adds its
`bin` only to this plugin's jobs' PATH. Setup steps and later native jobs are
privileged processes: they can access files and network as the daemon user,
and a `programs.network` description is disclosure, not an enforced network
allowlist. Removing the plugin removes its managed tools.

### Testing a 0.2 plugin

`plugins/test-platform` uses every node, import and event once; the daemon's
`tests/plugin_platform.rs` drives it through the routes (render, actions, file
actions, query, settings, output, surfaces, file events, a restart).
`plugins/test-privileged` does the same for programs and tools
(`tests/plugin_jobs.rs`: a job and its event, the limits, the queue, a tool
served by a local fake host, a wrong checksum, a waiting agent tool). Copy
their shape: pure logic in functions that take data, the host calls in the
daemon's tests.

## Worked examples and acceptance checks

Choose the smallest example that covers the capability you are adding.
Fixtures are host-test inputs, with intentionally hostile tools for checking
limits; use their API shape rather than publishing a fixture unchanged.

| Task | Concrete reference | What to verify live |
|---|---|---|
| Add a tab, dashboard panel, status chip or card section | [Platform manifest](../../plugins/test-platform/plugin.toml) and [implementation](../../plugins/test-platform/src/lib.rs) | Open each declared slot, trigger an action, resize narrow/wide, check light/dark and keyboard navigation |
| Claim a file kind and add file actions | The platform fixture's `doc` view and `count-words` action | Open a matching file, use Text/Open with, save it, change it from an agent or shell and observe the declared events |
| Publish diagnostics, output or references | The platform fixture's `publish` action; [surface validation](../../crates/chimaera-server/src/plugins/surfaces.rs) | Check editor marks and links, replace/remove a surface, switch off and confirm stale entries disappear |
| Keep durable settings/state | The platform fixture's `keep` action and settings; [data/settings host](../../crates/chimaera-server/src/plugins/pdata.rs) | Reset defaults, switch off/on and restart the isolated daemon; migrate values written by the previous plugin version |
| Run a program, download a tool or answer a long agent call | [Privileged manifest](../../plugins/test-privileged/plugin.toml), [implementation](../../plugins/test-privileged/src/lib.rs) and [job tests](../../crates/chimaera-server/src/tests/plugin_jobs.rs) | Missing program/tool, explicit install, progress, success/error, timeout/cancel and switching off during a job; check `tool_resume` and the delayed response |
| Read project Knowledge | [Mycelium repository](https://github.com/martinappberg/chimaera-plugin-mycelium), [Knowledge tests](../../crates/chimaera-server/src/tests/knowledge.rs) | Detection, empty/malformed input, duplicate ids and links, changed snapshot, provider update and switch-off |
| Build documents into a preview | [LaTeX repository](https://github.com/martinappberg/chimaera-plugin-latex), [Typst repository](https://github.com/martinappberg/chimaera-plugin-typst) | Source/PDF view, errors, output links, host-program selection and downloaded-tool selection on every claimed platform |

For a change to the host itself, build the fixtures with
`bash scripts/build-plugins.sh`, then run `just check` and the relevant UI
checks. For a plugin-only change, use that repository's native checks and
WASM build, followed by a real isolated-daemon flow. A docs-only edit needs
the doc-link check; it does not require starting a daemon. The
[verify-app workflow](../../.claude/skills/verify-app/SKILL.md) describes
runtime evidence to record when behavior changes.

### When the local build does not appear

| Symptom | Check next |
|---|---|
| Installed on the wrong daemon, or no daemon found | Run the CLI on the daemon host with the preview's `CHIMAERA_HOME`; compare `plugin list` with that preview's Extensions cards |
| Card says on but not active | Check `detect.any`, workspace root, symlink refusal, daemon/API gates, agent requirements and `hold`/`fault`; the workspace switch alone is not activation |
| Import refused after moving from 0.1 to 0.2 | Declare `[access]` explicitly; 0.2 defaults each omitted access to none |
| Component will not load | Confirm `wasm32-wasip2`, SDK WIT version and manifest `api`; copy the renamed component, not a native library or `wasm32-unknown-unknown` module |
| No agent tools offered | Make `provides.mcp_tools` match `tools()` exactly, use unique names prefixed for your plugin, check object schemas, then request a new `tools/list` or start a fresh session |
| Screen rejected, action does nothing | Read the reported JSON-path problems, return a `ui/1` tree, match the declared view/action, and respect built-in action payloads; use `invalidate` when updating another open view |
| Native unit tests abort | A test reached a `host::*` or `platform::*` import; move the logic behind pure inputs or a test adapter |
| Reinstall fails verification | Regenerate or remove a local staging `SHA256SUMS` after changing either file; published checksums must match both assets |
| New version asks for trust | Inspect `plugin caps`: capability growth and privileged updates can require approval even for an existing first-party repository |

## Author checklist

- The extension seam and repository are correct; domain behavior stays in the
  plugin, and a host change is generic, versioned and tested.
- `Cargo.toml`, `plugin.toml`, release tag and compiled WIT agree. The SDK is
  pinned for release builds; a new plugin targets API 0.2 and declares access.
- `[adds]`, descriptions and setup prompts describe actual user/agent
  behavior. Tools have plugin-specific names, object schemas, useful errors
  and matching manifest/export lists.
- Native tests cover pure logic and failure cases without host imports. The
  WASM build succeeds; real host calls and UI behavior have live evidence.
- Activation and call gates work while on/off and across workspaces. Check
  detection, absent/malformed inputs, relevant caps and fault recovery.
- Views use semantic nodes and core viewers; verify narrow/wide, light/dark,
  labels, focus and an understandable empty/error state. Events, watches,
  invalidation and published surfaces are used for updates.
- Programs/downloads are declared and disclosed. Test missing dependencies,
  job limits/cancellation and each claimed platform. Keep reproducible URLs,
  checksums and tool setup; a native job is privileged.
- Updates accept old state and settings, and rollback is usable. Capability
  changes are intentional and checked with `chimaera plugin caps`.
- A release carries `plugin.wasm`, `plugin.toml` and `SHA256SUMS`, with a
  matching `[release] github`. Curated first-party inclusion additionally
  requires the lock review and tests in [plugins/AGENTS.md](../../plugins/AGENTS.md).
- The plugin repository's README/agent instructions explain build, local
  install, supported hosts and usage. Update this guide for an API change,
  the relevant maps for host wiring, and the feature catalog for shipped
  user-facing behavior. Report what was verified and what still needs a host,
  credential or platform you could not exercise.
