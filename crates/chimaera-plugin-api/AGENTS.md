# chimaera-plugin-api — the plugin interface

The third public interface Chimaera pins (beside the daemon↔UI wire and the
agent protocols): the `chimaera:plugin` WIT world and the Rust bindings a
plugin author uses. A plain rlib in the root workspace (it compiles natively);
plugins depend on it from their own workspace ([plugins/](../../plugins/AGENTS.md)).
Design and phases: [docs/plugin-system-plan.md](../../docs/plugin-system-plan.md).
The host that serves it: `crates/chimaera-server/src/plugins/`
([server map](../chimaera-server/AGENTS.md)).

## Files

| File | What |
|---|---|
| `wit/chimaera.wit` | The world, package `chimaera:plugin@0.1.0`: `types`, `host` (imports: bounded fs, state, sessions, Timeline, emit, now, log), `plugin` (exports: tools, instructions, call-tool, knowledge, query, on-event). The daemon's `bindgen!` reads this same file. |
| `src/lib.rs` | `wit_bindgen::generate!` plus the ergonomic layer: the `Plugin` trait (every export has a default), `export!` (wires a `Plugin` to the generated `Guest`), `host::*` wrappers (JSON parsed, fallible calls return `Result<_, String>`), `ToolDef::new`, `ToolResult::text`/`error`, `Snapshot::new`, and `serde_json` re-exported. |

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
  plugin. A new host import (`exec`, `watch` in 0.2) does not: a host may
  offer more than a component uses. Bump the package version with any WIT
  change, and the host's `plugins::API` with it (adding the new version to
  `plugins::SERVED_APIS` beside the ones it still serves: a manifest's `api`
  must be one of them to load).
- **WIT keywords need `%`.** `%list` is the function `list`; a type and a
  function may not share a name inside one interface (hence
  `stat as file-stat` in `host`).
- The crate's version tracks the WIT package (0.1.0), not the daemon release.
