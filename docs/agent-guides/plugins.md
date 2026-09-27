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
  host may offer more than a component uses, so WIT 0.2 adds `exec` and
  `watch` without breaking a 0.1 plugin.
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
api = "0.1"                     # the chimaera:plugin WIT version it targets (MAJOR.MINOR)

[detect]                        # workspace-relative; ANY present ⇒ detected
any = [".living/INDEX.md", "MYCELIUM.md"]   # empty/omitted ⇒ always present (on = active)

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
| `detect.any` | footprint → "active here" (no path component may be a symlink) | `plugins::detect_blocking` |
| `requires.agent_plugins` | a genuine hard requirement (no plugin has one today): per-agent install state (asked of the agents), "Requires the <agent> plugin <id>" on the card for agents installed here, and an install button running the agent's own `plugin marketplace add` + `install`/`add` in a visible terminal; the attach sheet's step 1; codex hook trust | `agent_probe.rs`, `plugins::install_requirement` |
| `recommends.agent_plugins` | the same shape and the same install route, attach-sheet step and hook trust, for an agent-side plugin that makes this one more useful to the agents the user runs but is never needed: the card's **Agent-side plugin** box, one row per agent installed here ("claude · installed 0.7.2", "codex · not installed [Install]") | `agent_probe.rs`, `plugins::install_requirement` |
| `requires.summary`, `recommends.summary` | optional: one plain sentence saying what the agent-side plugin is for; the box and the attach sheet's step 1 say it above the agents' rows (`requires_summary` / `recommends_summary` on the wire; a plain fallback when absent) | `manifest_json` |
| `requires.chimaera` | a gate: this daemon's version must match | `plugins::gate` |
| `setup.prompt` | a new chat session of the user's chosen agent, sent this prompt | `plugins::setup_workspace` |
| `provides.knowledge` | the plugin is the Knowledge provider; its `knowledge` export feeds the view and `GET /workspaces/{id}/knowledge` | `knowledge.rs`, `runtime::knowledge` |
| `provides.mcp_tools` | tools served by the chimaera MCP where active, plus the `instructions` paragraph; pre-allowed at spawn. Unique names, 1–64 ASCII letters, digits, underscores, dots or dashes; built-in names are reserved | `plugins/tools.rs`, `runtime::offer` |
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
paragraph at 8 KiB, a hook line at 1 KiB. A component whose `tools()` names
differ from its manifest's `provides.mcp_tools` is refused.

Knowledge stamps and Timeline baselines are scoped to the provider id and
component digest: an update, rollback or provider change gets a fresh snapshot
even when its input files are unchanged.

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
  list the maintainers review — and the installed copy's `[release] github`
  matches the lock's `repo` (case-insensitive). The bytes must also match the
  lock or come from a release install whose source the host recorded in
  `source-github`. An update from that repository keeps the badge. Local
  installs do not copy that marker, so a rebuilt copy cannot assert the badge.
- **"local build"**: the copy was installed from a directory (`--path`;
  `local_path` on the wire).
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
3. In the chimaera repository, set the entry's `version`, `sha256_wasm` and
   `sha256_toml` in `plugins/plugins.lock` from that release's `SHA256SUMS`
   (and `name` / `summary` if they changed), run
   `bash scripts/build-plugins.sh` (it downloads the new release into
   `plugins/dist-test/`) and `just check`, and review the change like code:
   from the next chimaera release, **Install** fetches that version.

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
