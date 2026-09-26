# plugins/ — first-party plugins (WASM components)

Each plugin is a Rust `cdylib` crate built for `wasm32-wasip2` into one
portable `plugin.wasm`, plus its `plugin.toml` manifest. This directory is its
**own cargo workspace** (never the daemon's): a plugin crate does not link for
the native target on macOS (its component export names contain `#`), and the
daemon's lockfile stays free of the guest-side tooling. Later each plugin moves
to its own repository; the build script then downloads a pinned artifact.
Design: [docs/plugin-system-plan.md](../docs/plugin-system-plan.md). The API
they build on: [chimaera-plugin-api](../crates/chimaera-plugin-api/AGENTS.md).

## Map

| Path | What |
|---|---|
| `Cargo.toml` | The workspace: members, `chimaera-plugin-api` by path, a small release profile (`lto`, `strip`, `opt-level = "s"`) — every plugin is embedded in the daemon binary. |
| `agent-notes/` | Agent notes: `post_note` / `read_notes`, the addressing rule, unread + read cursors (host state), the "N unread notes" hook line. `src/notes.rs` is the pure logic, unit-tested natively; `src/lib.rs` wires it to the host. The texts agents read are pinned by `crates/chimaera-server/src/tests/plugins.rs`. |
| `mycelium/` | Mycelium, the Knowledge provider: `src/reader.rs` is the read-only reader of mycelium 0.7.2's `.living/` findings/decisions/learnings, `todo/TODO_REGISTRY.md` and the `.mycelium` handoff (fence-aware, capped per file/field/read, symlinks refused), producing the `Knowledge` snapshot whose `Serialize` shape IS the Knowledge view's wire; `plan` (metadata only) gives the `Stamp` (`{"files": [[path, mtime_ms, len]], "refused": [path]}`) the host caches by and attributes with. `src/fs.rs`: the `Fs` trait the reader reads through — `HostFs` (`lib.rs`) in the component, `StdFs` (`std::fs` with the host's refusals, in its words) in native tests only; the reader tells a symlink refusal from a missing file by the host's wording. `src/tools.rs`: `knowledge_search` / `knowledge_get` and the instruction paragraph (pure, over a snapshot). `src/lib.rs`: the exports — `knowledge(cx, known)` answers `None` for its own stamp, and the last read stays in instance memory for the tools. The route JSON and tool texts are pinned by `crates/chimaera-server/src/tests/knowledge.rs`. |
| `test-fixture/` | The host's test fixture (echo, loop, allocate, panic, read, state, append, recent): each tool pokes one host limit. Laid out in `dist-test/`, which only the daemon's **test** builds embed — never shipped. |
| `dist/`, `dist-test/`, `target/` | Build output (gitignored): `<id>/{plugin.wasm,plugin.toml}`, embedded by the daemon like `web-ui/dist`. |

## Build and check

```sh
bash scripts/build-plugins.sh      # or `just plugins`; needs the wasm32-wasip2 target (rust-toolchain.toml lists it)
cargo clippy --manifest-path plugins/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path plugins/Cargo.toml
cargo fmt --all --manifest-path plugins/Cargo.toml
```

Build the plugins before any build of `chimaera-server`: its embed of
`plugins/dist` (and, for tests, `plugins/dist-test`) fails to compile without
them. A debug daemon reads `plugins/dist` from disk when its catalog loads, so
a rebuilt plugin needs a daemon restart, not a cargo rebuild.

## Rules

- **No host call in a native test.** The wit-bindgen import stubs abort the
  test binary. Pure logic lives in modules that take data (`agent-notes/src/notes.rs`),
  or reads through a trait the test implements natively (`mycelium/src/fs.rs`).
- **A manifest's `provides.mcp_tools` equals the component's `tools()` names**
  — the host refuses the plugin otherwise — and `api` names the WIT version
  (`"0.1"`).
- **A plugin never enforces a host limit itself** (rate caps, text caps, path
  confinement): the host does, for every plugin. A plugin may pre-check for a
  friendlier message.
- A crate named `test-*` is test-only: the script lays it out in `dist-test/`.
