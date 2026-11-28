# chimaera-plugin-api — the plugin interface

The third public interface Chimaera pins (beside the daemon↔UI wire and the
agent protocols): the `chimaera:plugin` WIT world and the Rust bindings a
plugin author uses. A plain rlib in the root workspace (it compiles natively);
plugins depend on it from their own workspace ([plugins/](../../plugins/AGENTS.md)).
Design and phases: [docs/design/plugin-system-plan.md](../../docs/design/plugin-system-plan.md);
writing a plugin against it: [docs/agent-guides/plugins.md](../../docs/agent-guides/plugins.md).
The host that serves it: `crates/chimaera-server/src/plugins/`
([server map](../chimaera-server/AGENTS.md)).

## Author entry point

For a new workbench plugin, begin with the guide's
[complete API 0.2 starter](../../docs/agent-guides/plugins.md#minimal-api-02-plugin)
and [author checklist](../../docs/agent-guides/plugins.md#author-checklist).
Keep the plugin crate in its own repository/workspace; the fixtures here are
host-test inputs. Pin this SDK to a commit for a release, make manifest `api`
match the component's WIT, and declare `[access]` explicitly (0.2 omitted
access means none). A workbench plugin supplies semantic `ui/1` trees through
this API; agent-native plugins/skills have their agent's own packaging.

To choose a capability or example, see the guide's
[extension seams](../../docs/agent-guides/plugins.md#start-here-choose-the-extension-seam)
and [worked examples](../../docs/agent-guides/plugins.md#worked-examples-and-acceptance-checks).
Programs and downloaded tools make a plugin privileged: native jobs run
outside WASM with the daemon user's access. The sandbox protects the guest
component; job resource limits do not isolate native filesystem/network access.

## Files

| File | What |
|---|---|
| `wit/chimaera.wit` | The current world, package `chimaera:plugin@0.4.0`: 0.3 plus three `platform` imports — `web-call` (a call id at once; the window showing the node answers, else the headless host), `web-call-result` (the answer, none while it waits, or why it failed) and `reveal` (a view or claimed file beside the calling session) — and `tool-result.wait` naming a page call too ([calls and reveal](../../docs/agent-guides/plugins.md#asking-your-page-and-opening-views-beside-the-agent-api-04)). 0.3 was the 0.2 world plus `host.read-at` (a workspace file from an offset) and the `web` export (`web-bundle() -> list<u8>`: a tar of the plugin's own pages and libraries, [web views](../../docs/agent-guides/plugins.md#web-views-api-03)). As in 0.2: `types`, `host` (imports as in 0.1: bounded fs, state, sessions, Timeline, emit, now, log), `platform` (imports: output folder, `publish`, `invalidate`, `watch`, `setting-get`, `state-keep`, `roots`, `job-*`, `tool-state`), `plugin` (exports as in 0.1) and `screens` (exports: `render`, `on-action`, `tool-resume`); new events and `tool-result.wait`. The daemon's `bindgen!` (`runtime::v4`) reads this file. |
| `wit-0.3/chimaera.wit` | **Frozen**: `chimaera:plugin@0.3.0`, served beside 0.4 (`runtime::v3`, `invoke_v3`; its `host`/`platform` imports adapted in `hostfns.rs` `v3_host`) so a 0.3 component keeps loading. Never edit it; `plugins/api-0.3` builds the 0.3 fixture against it. |
| `wit-0.2/chimaera.wit` | **Frozen**: `chimaera:plugin@0.2.0`, served beside 0.3 (`runtime::v2`, its `host`/`platform` imports adapted in `hostfns.rs` `v2_host`) so a 0.2 component — every first-party plugin built before 0.3 — keeps loading. Never edit it. |
| `wit-0.1/chimaera.wit` | **Frozen**: `chimaera:plugin@0.1.0`, served beside 0.2 (`runtime::v1`) so a 0.1 component keeps loading. Never edit it; `plugins/api-0.1` builds the 0.1 fixture against it. |
| `src/lib.rs` | `wit_bindgen::generate!` plus the ergonomic layer (0.4: `platform::web_call` / `web_call_with` (`WebCall`) / `web_call_result` / `reveal`, `ToolResult::wait_call`): the `Plugin` trait (every export has a default, 0.2's `render` / `on_action` / `tool_resume` and 0.3's `web_bundle` too, so an older plugin compiles unchanged), `export!`, `host::*` and `platform::*` wrappers (JSON parsed, fallible calls return `Result<_, String>`), `ui::*` helpers for `ui/1` trees, `ToolDef::new`, `ToolResult::text` / `error` / `wait`, `Snapshot::new`, and `serde_json` re-exported. |

## Rules

- **No host call in a native test.** Built natively, every host import is a
  wit-bindgen stub that aborts the whole test binary. Keep a plugin's pure
  logic (parsers, addressing, formatting) in functions that take data, and
  test those; the integration runs in the daemon's tests
  (`crates/chimaera-server/src/tests/plugin_host.rs`).
- **JSON for open-ended payloads.** Tool arguments and results, Timeline
  entries, the Knowledge snapshot, UI events and state values cross as JSON
  text: they already have JSON shapes on the daemon↔UI wire, and adding a
  field to JSON never breaks the ABI. Small fixed things are typed records.
- **The exports are the complete set; imports are additive.** Removing or
  changing an export (or a record a plugin returns) breaks every built
  plugin targeting that world. A host may offer imports an old component
  does not use; preserve that component's frozen world and bindings rather
  than assuming a trait default makes a compiled component compatible.
  Bump the package version with any WIT
  change, and the host's `plugins::API` with it (adding the new version to
  `plugins::SERVED_APIS` beside the ones it still serves: a manifest's `api`
  must be one of them to load).
- **WIT keywords need `%`.** `%list` is the function `list`; a type and a
  function may not share a name inside one interface (hence
  `stat as file-stat` in `host`).
- The crate's version tracks the WIT package (0.4.0), not the daemon release.
- **An old WIT is frozen, not deleted.** A bump copies the current `wit/` to
  `wit-<old>/` first; the host binds each served version (`runtime::v1`
  … `v4`) and translates at the edge.
