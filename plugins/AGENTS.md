# plugins/ — the plugins the daemon embeds

The first-party plugins live in **their own repositories**, each a Rust
`cdylib` crate built for `wasm32-wasip2` into one portable `plugin.wasm` beside
its `plugin.toml`, and each publishing a GitHub release per version:

- Agent notes: [martinappberg/chimaera-plugin-agent-notes](https://github.com/martinappberg/chimaera-plugin-agent-notes)
- Mycelium, the Knowledge provider: [martinappberg/chimaera-plugin-mycelium](https://github.com/martinappberg/chimaera-plugin-mycelium)

This directory holds what the chimaera repository needs of them: the lock that
pins which release of each ships inside the binary, and the host's test
fixture (its own cargo workspace, never the daemon's). Design:
[docs/plugin-system-plan.md](../docs/plugin-system-plan.md). The API plugins
build on: [chimaera-plugin-api](../crates/chimaera-plugin-api/AGENTS.md).
Writing one: [docs/agent-guides/plugins.md](../docs/agent-guides/plugins.md).

## Map

| Path | What |
|---|---|
| `plugins.lock` | One `[[plugin]]` per first-party plugin: `id`, `version`, `repo` (`owner/name`), `sha256_wasm`, `sha256_toml` (the release's `SHA256SUMS`). What `scripts/build-plugins.sh` downloads (`https://github.com/<repo>/releases/download/v<version>/{plugin.wasm,plugin.toml}`), verifies and lays out in `dist/`, so the daemon embeds exactly those bytes. A bump is a reviewed change to this file. `plugins::tests::every_manifest_parses_and_ids_are_unique` checks every locked id ships and names its lock `repo` as `[release] github`. |
| `plugins.local.toml` | The local override (gitignored, yours to create): `[[plugin]] id = "…" path = "…"` builds that checkout (absolute, `~/…`, or relative to `plugins/`) with `cargo build --release --target wasm32-wasip2` in its own directory (its own `rust-toolchain.toml`) and lays out its `.wasm` and `plugin.toml` instead of the locked release, printing that it did; the version check against the lock is skipped for it, and an id the lock doesn't name is laid out too. For developing a plugin against the daemon. |
| `cache/` | Downloads (gitignored): `<id>-<version>/{plugin.wasm,plugin.toml}`. A second build and an offline build use it; every cached file is re-verified against the lock on every run, never trusted (a mismatch is fetched again, and kept until a verified replacement lands). |
| `Cargo.toml`, `Cargo.lock` | The workspace: `test-fixture` only, `chimaera-plugin-api` by path, a small release profile. |
| `test-fixture/` | The host's test fixture (echo, loop, allocate, panic, read, state, append, recent): each tool pokes one host limit. Built here and laid out in `dist-test/`, which only the daemon's **test** builds embed — never shipped. Its `v2` feature (one more tool, `version`) with `plugin-v2.toml` (0.2.0) is its "next release" for the daemon's update tests: the script also lays that out as `dist-test/test-fixture-v2/`, which the tests serve from a fake releases server (`crates/chimaera-server/src/tests/plugin_updates.rs`). |
| `dist/`, `dist-test/`, `target/` | Build output (gitignored): `<id>/{plugin.wasm,plugin.toml}`, embedded by the daemon like `web-ui/dist`. |

## Build and check

```sh
bash scripts/build-plugins.sh      # or `just plugins`; needs network (or the cache) and the wasm32-wasip2 target
cargo clippy --manifest-path plugins/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path plugins/Cargo.toml
cargo fmt --all --manifest-path plugins/Cargo.toml
```

The script has two jobs. **The locked plugins:** for each lock entry, the
release's `plugin.wasm` and `plugin.toml` from the cache or GitHub (curl,
16 MiB cap, timeouts), both sha256s checked against the lock (a mismatch fails
the build, naming the expected and the actual digest), the manifest's `id`
and `version` checked against the lock's (and its `[release] github`, when it
names one, against `repo`). A build without network needs the cache or an
override for every locked plugin. **The fixture:** its `plugin.toml` checked
against its crate and the WIT (`version` equals the crate's resolved version,
`api` the WIT package's MAJOR.MINOR), then built into `dist-test/`. The layout
is staged and swapped in at the end, so a failed run keeps the last good one.
Linux `sha256sum` or macOS `shasum`, bash 3.2.

Build the plugins before any build of `chimaera-server`: its embed of
`plugins/dist` (and, for tests, `plugins/dist-test`) fails to compile without
them. A debug daemon reads `plugins/dist` from disk when its catalog loads, so
a rebuilt plugin needs a daemon restart, not a cargo rebuild.

## A first-party release, end to end

1. In the plugin's repository: bump `version` in `Cargo.toml` and
   `plugin.toml` together and tag `v<version>`; its release workflow publishes
   `plugin.wasm`, `plugin.toml` and `SHA256SUMS`.
2. Daemons already running offer it on the card (their checker asks every
   plugin whose manifest names `[release]`, embedded ones included), and
   **Update** installs it as an installed copy over the embedded one.
3. Here: set the entry's `version`, `sha256_wasm` and `sha256_toml` from that
   release's `SHA256SUMS`, run the script and the daemon's tests, and review
   the change like code — the next chimaera release ships those bytes.

## Rules

- **No host call in a native test.** The wit-bindgen import stubs abort the
  test binary. Keep pure logic in functions that take data, or behind a trait
  the test implements natively (the plugin repositories do both).
- **A manifest's `provides.mcp_tools` equals the component's `tools()` names**
  — the host refuses the plugin otherwise — and `api` names the WIT version
  (`"0.1"`).
- **`version` is the manifest's, and it is the crate's.** Every manifest
  carries `version` (plain `MAJOR.MINOR.PATCH`) and `api`; the fixture's must
  equal the plugins workspace's (the script refuses a mismatch), and a locked
  plugin's must equal the lock's.
- **A release is three assets.** A plugin that updates on its own names
  `[release] github = "owner/repo"`: releases tagged `v<version>` carrying
  `plugin.wasm`, `plugin.toml` (the same manifest, that version) and
  `SHA256SUMS` (`sha256sum` format, both files). The daemon offers a release
  only when it is strictly newer than what runs and passes the gates,
  installs it only on the user's click (or `chimaera plugin add|update`), and
  verifies both checksums first. The first-party plugins name their own
  repositories, so the copy that ships with chimaera can be updated from them
  too.
- **Nothing but `test-*` crates here.** A first-party plugin ships through
  `plugins.lock` from its own repository; the script refuses any other crate
  in this workspace. `test-*` crates are laid out in `dist-test/`.
- **A plugin never enforces a host limit itself** (rate caps, text caps, path
  confinement): the host does, for every plugin.
