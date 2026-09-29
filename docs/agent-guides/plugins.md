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
the lock and the test fixture [plugins/](../../plugins/AGENTS.md). The first-party
plugins, each its own repository and the worked examples here: [chimaera-plugin-agent-notes](https://github.com/martinappberg/chimaera-plugin-agent-notes) and
[chimaera-plugin-mycelium](https://github.com/martinappberg/chimaera-plugin-mycelium).

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
  host may offer more than a component uses, so the planned WIT 0.2 (jobs,
  a watch set, screens: the [platform plan](../plugin-platform-plan.md)) adds
  imports without breaking a 0.1 plugin.
- **No host call in a native test.** Built natively (tests, clippy), every
  host import is a wit-bindgen stub that aborts the whole test binary. Keep
  pure logic in functions that take data (Agent notes' `src/notes.rs`) or
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
- **Login-node discipline.** Detection is a few `stat`s off the reactor,
  cached; nothing polls; a plugin nobody switches on is never compiled.

## The manifest

`plugin.toml` at the crate root, parsed with `deny_unknown_fields` (a typo is
an error, not a silently ignored key) and validated: the id, the version, the
release source.

```toml
id = "mycelium"                 # stable; lowercase letters, digits, dashes; ≤ 64; not "install"
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
repository; both name their own repository in `[release]`, which is where
chimaera installs them from. The host records that source; a manifest alone cannot
grant a copy the maintainer badge.
`[requires] chimaera` above is an illustration; neither carries it.
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
| `requires.agent_plugins` | a genuine hard requirement (no plugin has one today): per-agent install state (asked of the agents), "Requires the <agent> plugin <id>" on the card for agents installed here, and an install button running the agent's own `plugin marketplace add` + `install`/`add` in a visible terminal; the attach sheet's step 1; codex hook trust | `agent_probe.rs`, `plugins::install_requirement` |
| `recommends.agent_plugins` | the same shape and the same install route, attach-sheet step and hook trust, for an agent-side plugin that makes this one more useful to the agents the user runs but is never needed: the card's **Agent-side plugin** box, one row per agent installed here ("claude · installed 0.7.2", "codex · not installed [Install]") | `agent_probe.rs`, `plugins::install_requirement` |
| `requires.summary`, `recommends.summary` | optional: one plain sentence saying what the agent-side plugin is for; the box and the attach sheet's step 1 say it above the agents' rows (`requires_summary` / `recommends_summary` on the wire; a plain fallback when absent) | `manifest_json` |
| `requires.chimaera` | a gate: this daemon's version must match | `plugins::gate` |
| `setup.prompt` | a new chat session of the user's chosen agent, sent this prompt | `plugins::setup_workspace` |
| `provides.knowledge` | the plugin is the Knowledge provider; its `knowledge` export feeds the view and `GET /workspaces/{id}/knowledge` | `knowledge.rs`, `runtime::knowledge` |
| `provides.mcp_tools` | tools served by the chimaera MCP where active, plus the `instructions` paragraph; pre-allowed at spawn. Unique names, 1–64 ASCII letters, digits, underscores or dashes — no dots (codex pre-approves a tool by a dotted config key a dot would split); built-in names are reserved | `plugins/tools.rs`, `runtime::offer` |
| `provides.events` | which `on-event` variants the host delivers (none by default); `hook` and `session-ended` are delivered, `switched-on` / `switched-off` are declarable but not delivered yet | `runtime::hook`, `runtime::session_ended` |
| `provides.views` | parses and rides the wire; nothing renders it | none yet |
| `[adds]` | the card's "For you: …" (`ui`) and "For agents: …" (`agents`) sentences | the UI |
| `[release] github` | where the checker and Update look for newer versions; required for release installs and must match the repository being fetched; the maintainer badge additionally requires a recorded official source or pinned bytes | `plugins/releases.rs`, `plugins/installed.rs`, `plugins/mod.rs` |

What a manifest says is read before anything is installed, too: a card
opened before Install, and a repository previewed from the Extensions tab's
form, show the `description`, `[adds]` and the `[requires]` / `[recommends]`
summaries from the release's own `plugin.toml`, fetched and checksum-verified
by the daemon (`GET /plugins/{pid}/details`, `POST /plugins/preview`) — so
write them for someone deciding whether to install.

`settings` and `commands`, sketched in the earlier plan, are not manifest
keys; the first plugin that needs one adds it with a test and a row here.

### What it can do: the capabilities

The daemon derives one list from the manifest — the plugin's **capabilities**
([platform plan §1](../plugin-platform-plan.md#1-capabilities)): its
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

## The crate

```
chimaera-plugin-agent-notes/      its own repository
  Cargo.toml          [lib] crate-type = ["cdylib"]; depends on chimaera-plugin-api
  plugin.toml         the manifest
  rust-toolchain.toml the pinned toolchain, with the wasm32-wasip2 target
  src/lib.rs          impl Plugin for AgentNotes { … } + chimaera_plugin_api::export!(AgentNotes)
  src/notes.rs        the pure logic (addressing, unread, the texts), unit-tested natively
  .github/workflows/  ci.yml (fmt, clippy, tests, the wasm build) · release.yml (the three assets)
```

```toml
[package]
name = "chimaera-plugin-agent-notes"
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
(`serde_json::Value`), and `serde_json` is re-exported. From
Agent notes' `src/lib.rs`, trimmed to one tool:

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
(Mycelium's `src/lib.rs` is the example); `query` has no caller yet.
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
mkdir -p /tmp/agent-notes-dev
cp target/wasm32-wasip2/release/chimaera_plugin_agent_notes.wasm /tmp/agent-notes-dev/plugin.wasm
cp plugin.toml /tmp/agent-notes-dev/plugin.toml
chimaera plugin add --path /tmp/agent-notes-dev   # or POST /api/v1/plugins/install {"path": "/tmp/agent-notes-dev"}
```

The daemon copies both files (and a `SHA256SUMS`, when the directory has one:
both files must then match it) into `~/.chimaera/plugins/<id>/<version>/`,
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
- `tests/plugins.rs` (Agent notes: tools only where on, posts and reads stay
  in their workspace, the hook hint, the texts byte for byte),
  `tests/knowledge.rs` (Mycelium: the route JSON and tool texts against
  fixtures), `tests/plugin_updates.rs` (the three install kinds, update,
  rollback, remove and the checker, against a fake releases server),
  `tests/agent_view.rs` (the plugin-free view, unchanged).

## What the card says about a plugin

What the card shows is the daemon's, never the plugin's own claim:

- **The check badge** ("Verified by the Chimaera maintainers"; `first_party`
  on the wire): the plugin's id is in `plugins/plugins.lock` — the curated
  list the maintainers keep, whose versions follow each repository's releases
  automatically (CI-gated) — and the installed copy's `[release] github`
  matches the lock's `repo` (case-insensitive). The bytes must also match the
  lock or come from a release install whose source the host recorded in
  `source-github`. An update from that repository keeps the badge. Local
  installs do not copy that marker, so a rebuilt copy cannot assert the badge.
- **"local build"**: the copy was installed from a directory (`--path`;
  `local_path` on the wire).
- **Can**: what it can do, in the daemon's words (`can` on the wire), for
  every plugin, verified or not — and, when it can't run on this host, why
  (`hold`: waiting for the user's trust, blocked by Chimaera, or the host's
  policy). A first-party update keeps the badge only while its capability
  digest is the one the lock recorded (`caps`); one that asks for more is the
  user's to trust, and loses the badge.
- **Nothing about checksums.** The safety mechanism is the daemon's: it
  checks every download against the release's `SHA256SUMS` (and a
  first-party one against the lock's two sha256s), keeps that file beside the
  installed copy and re-checks the copy every time it loads the catalog
  (`verified` on the wire). Only a failure reaches the card, as its fault
  line: the copy is listed, never loaded.

## Shipping it

**First-party** plugins live in their own repositories and are named, with
one pinned release each, in `plugins/plugins.lock`. The two that exist are the
worked example: [chimaera-plugin-agent-notes](https://github.com/martinappberg/chimaera-plugin-agent-notes) and [chimaera-plugin-mycelium](https://github.com/martinappberg/chimaera-plugin-mycelium). Each is the crate and its `plugin.toml` (naming
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
   the pin keeps its check badge.
3. The chimaera repository follows on its own: within the hour, the
   `plugin-lock` workflow checks the new release (`SHA256SUMS` against the
   downloaded bytes; the manifest's id, version and `[release] github`
   against the lock), rewrites the entry in `plugins/plugins.lock` and opens
   `fix: update <Name> to <version>` with squash auto-merge. CI installs the
   release against the new lock, and the merge cuts a patch release: from it,
   **Install** fetches that version. Details, the one-time token and how to
   turn a version down: [plugins/AGENTS.md](../../plugins/AGENTS.md).

To add one: a repository shaped like those two, its first release, a
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
  (Agent notes' read cursors are the example). A plugin that wants a clean
  slate writes a new key.
- **The gates decide who gets it.** `api` must be a WIT version the daemon
  serves (0.1 today), and `requires.chimaera` (optional) must match the
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
  a host import is a minor bump (0.2 is planned in the
  [platform plan](../plugin-platform-plan.md#11-the-interface-wit-02)); changing or
  removing an export is a new major with a new world, served beside the old
  one for a transition. With any WIT change, bump the package version, the
  API crate's version with it, and the host's `plugins::API`, adding the new
  version to `plugins::SERVED_APIS` beside those it still serves.

## The platform: API 0.2

A plugin that says `api = "0.2"` gets the platform
([plan](../plugin-platform-plan.md) §3–§9): screens in Chimaera's own format,
file kinds and file actions, data surfaces core draws, file events, an output
folder, declared settings and durable state. The host serves 0.1 beside it
unchanged (its own bindings, `wit-0.1/`), so moving is a choice: bump the
`chimaera-plugin-api` dependency and `api`, and **declare `[access]`** — in 0.2
a key left out means none (0.1 implied files, the Timeline with notes, and
sessions). Every new export has a default, so a 0.1 plugin compiles on 0.2
unchanged. Programs, side-program downloads and long agent tools are in
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
| `tool_state(cx, tool)` | a declared tool: its version, whether (and which version) is installed here, what Install would download |

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
| `code` | **`text`**, `language` |
| `keyvalue` | **`items`**: `[{key, value, tone?}]` |
| `badge` | **`text`**, `tone` |
| `icon` | **`name`** (`check x alert info clock file folder play stop refresh download external grid list book bolt settings search`), `label`, `tone` |
| `progress` | `value` (0–1; absent: indeterminate), `label` |
| `empty` | **`title`**, `text`, `action` `{label, action, payload}` |
| `callout` | **`text`**, `title`, `tone` |
| `list` | **`items`**: `[{title, subtitle?, badges?, actions?: [nodes], action?, payload?, file?, line?}]`; `more` `{query, args}` pages through `query` (it answers `{items, more?}`) |
| `table` | **`columns`** `[{key, title, align?}]`, **`rows`** `[{<key>: text}]`; `more` as for a list (`{rows, more?}`) |
| `file` | **`path`** (a workspace path or `output:<path>`), `label`, `line` — a card that opens it |
| `link` | **`text`**; `href` (http/https, opens outside) or `file` + `line` |
| `image` | **`src`**, **`alt`** |
| `button` | **`label`**, **`action`**, `payload`, `tone`, `icon`, `disabled` |
| `toggle` | **`label`**, **`name`**, `value`, `action` (sends `{value}` beside the payload) |
| `select` | **`label`**, **`name`**, **`options`** (`[{value, label}]` or strings), `value`, `action` |
| `textfield` | **`label`**, **`name`**, `value`, `placeholder`, `multiline` |
| `form` | **`action`**, `children` (its fields), `submit`; sends `{form: {name: value}}` beside the payload |
| `editor` | **`path`** — the file in the app's own editor (saves, merges) |
| `pdf` | **`src`** — the app's PDF viewer |
| `log` | **`src`** — the app's viewer for that file (a `.log` streams from its tail) |
| `diagnostics` | `file` — the problems list from every active plugin's `diagnostics/1`, with **Go to** and **Ask agent** |
| `diff` | `before` + `after`, or `path` + `base` (`head`, `index`, `rev:<ref>`, `output:<path>`); `mode` `prose` (default: words within a changed line) or `code` |

A node this chimaera doesn't know draws its `fallback` (a node, or `"drop"`),
else a quiet "needs a newer chimaera" with its children. Actions a plugin names
but never handles (the app carries them out): `open-file {file, line?}`,
`open-view {view}`, `open-url {url}`, `copy {text}`, `save-to-workspace {from,
to}` (the user's click copies an output file into the workspace; asks before
replacing), `ask-agent {file?, line?, text}`.

Where a view draws: `tab` (its own tab: the card's **Open**, `open-view`, a file
action's `open`), `panel` (the dashboard, after core's sections), `file` (a
claimed file), `status` (a chip in a claimed file's bar), `card` (inside its
Extensions card). The UI renders only on open, on an action, and on
`invalidate`; nothing polls.

### Data surfaces

Data core draws with its own views; `publish` checks the shape:

| Surface | Shape |
|---|---|
| `diagnostics/1` | `{items: [{file, severity (error warning info hint), line (from 1), column?, end_line?, end_column?, message, context?, source?}]}`, ≤ 200 per file, ≤ 2,000 per key |
| `output/1` | `{source, output, state (building ok errors failed), label?, finished_ms?, changed_pages?, log?}` |
| `sourcemap/1` | `{output, files: [path], records: [[file, line, page, x, y, width, height]]}`, ≤ 4 MiB, kept in the output folder |
| `knowledge/1` | the Knowledge snapshot (see the [coordination section](../plugin-platform-plan.md#coordination-the-knowledge-redesign-2026-09-29)), ≤ 4 MiB |

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
([plan](../plugin-platform-plan.md) §6, §8). Either makes it **privileged**: the
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
label, exit, timed_out, cancelled, error, queued_ms, started_ms, finished_ms,
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
versions at most). Nothing outside that folder changes: its `bin` joins only
this plugin's jobs' PATH. Removing the plugin removes its tools.

### Testing a 0.2 plugin

`plugins/test-platform` uses every node, import and event once; the daemon's
`tests/plugin_platform.rs` drives it through the routes (render, actions, file
actions, query, settings, output, surfaces, file events, a restart).
`plugins/test-privileged` does the same for programs and tools
(`tests/plugin_jobs.rs`: a job and its event, the limits, the queue, a tool
served by a local fake host, a wrong checksum, a waiting agent tool). Copy
their shape: pure logic in functions that take data, the host calls in the
daemon's tests.
