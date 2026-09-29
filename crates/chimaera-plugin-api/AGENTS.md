# chimaera-plugin-api — the plugin interface

The third public interface Chimaera pins (beside the daemon↔UI wire and the
agent protocols): the `chimaera:plugin` WIT world and the Rust bindings a
plugin author uses. A plain rlib in the root workspace (it compiles natively);
plugins depend on it from their own workspace ([plugins/](../../plugins/AGENTS.md)).
Design and phases: [docs/plugin-system-plan.md](../../docs/plugin-system-plan.md);
writing a plugin against it: [docs/agent-guides/plugins.md](../../docs/agent-guides/plugins.md).
The host that serves it: `crates/chimaera-server/src/plugins/`
([server map](../chimaera-server/AGENTS.md)).

## Files

| File | What |
|---|---|
| `wit/chimaera.wit` | The current world, package `chimaera:plugin@0.2.0`: `types`, `host` (imports as in 0.1: bounded fs, state, sessions, Timeline, emit, now, log), `platform` (imports: output folder, `publish`, `invalidate`, `watch`, `setting-get`, `state-keep`, `roots`, `job-*`, `tool-state`), `plugin` (exports as in 0.1) and `screens` (exports: `render`, `on-action`, `tool-resume`); new events and `tool-result.wait`. The daemon's `bindgen!` (`runtime::v2`) reads this file. |
| `wit-0.1/chimaera.wit` | **Frozen**: `chimaera:plugin@0.1.0`, served beside 0.2 (`runtime::v1`) so a 0.1 component keeps loading. Never edit it; `plugins/api-0.1` builds the 0.1 fixture against it. |
| `src/lib.rs` | `wit_bindgen::generate!` plus the ergonomic layer: the `Plugin` trait (every export has a default, 0.2's `render` / `on_action` / `tool_resume` too, so a 0.1 plugin compiles unchanged), `export!`, `host::*` and `platform::*` wrappers (JSON parsed, fallible calls return `Result<_, String>`), `ui::*` helpers for `ui/1` trees, `ToolDef::new`, `ToolResult::text` / `error` / `wait`, `Snapshot::new`, and `serde_json` re-exported. |

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
  plugin. A new host import does not: a host may offer more than a component
  uses. Bump the package version with any WIT
  change, and the host's `plugins::API` with it (adding the new version to
  `plugins::SERVED_APIS` beside the ones it still serves: a manifest's `api`
  must be one of them to load).
- **WIT keywords need `%`.** `%list` is the function `list`; a type and a
  function may not share a name inside one interface (hence
  `stat as file-stat` in `host`).
- The crate's version tracks the WIT package (0.2.0), not the daemon release.
- **An old WIT is frozen, not deleted.** A bump copies the current `wit/` to
  `wit-<old>/` first; the host binds each served version (`runtime::v1`,
  `v2`) and translates at the edge.
