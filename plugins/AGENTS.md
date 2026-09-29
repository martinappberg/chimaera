# plugins/ — the plugins lock and the host's test fixture

The first-party plugins live in **their own repositories**, each a Rust
`cdylib` crate built for `wasm32-wasip2` into one portable `plugin.wasm` beside
its `plugin.toml`, and each publishing a GitHub release per version:

- Agent notes: [martinappberg/chimaera-plugin-agent-notes](https://github.com/martinappberg/chimaera-plugin-agent-notes)
- Mycelium, the Knowledge provider: [martinappberg/chimaera-plugin-mycelium](https://github.com/martinappberg/chimaera-plugin-mycelium)
- LaTeX (privileged: latexmk, TinyTeX): [martinappberg/chimaera-plugin-latex](https://github.com/martinappberg/chimaera-plugin-latex)
- Typst (privileged: typst): [martinappberg/chimaera-plugin-typst](https://github.com/martinappberg/chimaera-plugin-typst)

This directory holds what the chimaera repository needs of them: the lock
that names them and pins one release of each (the only plugin data the daemon
embeds — never a plugin's bytes), and the host's test fixture (its own cargo
workspace, never the daemon's). Design:
[docs/plugin-system-plan.md](../docs/plugin-system-plan.md). The API plugins
build on: [chimaera-plugin-api](../crates/chimaera-plugin-api/AGENTS.md).
Writing one: [docs/agent-guides/plugins.md](../docs/agent-guides/plugins.md).

## Map

| Path | What |
|---|---|
| `plugins.lock` | One `[[plugin]]` per first-party plugin: `id`, `name` and `summary` (what an available card shows before any install), `version` (the pinned release), `repo` (`owner/name`), `sha256_wasm`, `sha256_toml` (that release's `SHA256SUMS`), and `tier` + `caps` — what the maintainers approved it to do (`sandboxed` / `privileged`, and its capability digest; `chimaera plugin caps plugin.toml` prints both for a release's manifest). A sandboxed update keeps the badge only while its digest is `caps`; a privileged plugin is verified only at the pin (docs/plugin-platform-plan.md §2). The daemon embeds it (`include_str!`, parsed with toml in `crates/chimaera-server/src/plugins/mod.rs`): each entry is listed as available until installed, and a first-party install fetches `https://github.com/<repo>/releases/download/v<version>/{SHA256SUMS,plugin.toml,plugin.wasm}` and refuses bytes that don't match both the release's `SHA256SUMS` and these sha256s. A bump is automatic — `.github/workflows/plugin-lock.yml` (below) — and CI-gated. `plugins::tests::every_locked_release_is_what_the_lock_says` checks each locked release (downloaded into `dist-test/`) against it: both sha256s, id, version, name, `[release] github` equal to `repo`, the gates, that it says what it adds, and that `tier` and `caps` are its manifest's. |
| pre-release pins | A pinned release can be a GitHub **pre-release** that isn't "latest" (Agent notes 0.1.4 and Mycelium 0.2.1, 2026-09-29: their `[access]` is a table a released daemon can't parse). This lock downloads it by tag; every daemon's release checker and the lock bot read `releases/latest` and don't see it — the bot only moves a pin forward. Once the chimaera that pins it is released, mark it latest: `gh release edit vX.Y.Z --repo <repo> --prerelease=false --latest`. The plugin repos' release workflow publishes one by hand (`workflow_dispatch`, `prerelease: true`). |
| `revoked.json`, `revocation-keys.txt` | The kill switch (`crates/chimaera-server/src/plugins/revoke.rs`): the list of blocked plugin builds (id, versions and/or `plugin.wasm` sha256s, `hard` or `soft`, a reason), embedded in every build, and the Ed25519 public keys (plus `threshold`) whose signatures make the live copy — this file on `main`, fetched daily — count. With no key listed only the embedded list counts. Block a build: add its entry and raise `serial` (a host refuses a list older than its own), then `node scripts/revocations.mjs sign --key <your private key>` (writes `revoked.sig`), commit both, and ship a release for hosts that never fetch; `keygen` makes a maintainer's key (its private half never enters the repository), `verify` checks the current signatures. |
| `Cargo.toml`, `Cargo.lock` | The workspace: `api-0.1`, `test-fixture`, `test-platform`, `test-privileged`; `chimaera-plugin-api` by path; a small release profile. |
| `api-0.1/` | **Frozen**: the 0.1 guest API (`chimaera-plugin-api-v0-1`, reading `crates/chimaera-plugin-api/wit-0.1`), which `test-fixture` depends on renamed as `chimaera-plugin-api` — the host's proof that a 0.1 plugin still loads. Never edit it. |
| `test-platform/` | The 0.2 platform fixture: every `ui/1` node, every `platform` import, the new events, five views (one per slot), a file kind (`*.fixture`), a file action, five settings. Laid out as `dist-test/test-platform/`; driven by `crates/chimaera-server/src/tests/plugin_platform.rs`. |
| `test-privileged/` | The programs-and-tools fixture (0.2, privileged): `[[programs]]` `sh`, `echo`, `sleep` and `fixture-tool`; `[[tools]]` `fixture-tool` 1.0.0, whose archive `fixture-tool-1.0.0.tar.gz` (a `bin/fixture-tool` shell script) sits beside it with its sha256 in the manifest and a setup step; a `jobs` view whose actions start jobs, a long agent tool `build` (`wait` → `tool_resume`), and `job-finished` recorded for the tests. Laid out as `dist-test/test-privileged/`; driven by `crates/chimaera-server/src/tests/plugin_jobs.rs`, which serves the archive from a fake host. |
| `test-fixture/` | The host's test fixture (echo, loop, allocate, panic, read, state, append, recent): each tool pokes one host limit. Built here into `dist-test/`, which only the daemon's **test** builds embed — never shipped. Its `v2` feature (one more tool, `version`) with `plugin-v2.toml` (0.2.0) is its "next release" for the daemon's update tests: the script also lays that out as `dist-test/test-fixture-v2/`, which the tests serve from a fake releases server (`crates/chimaera-server/src/tests/plugin_updates.rs`). |
| `dist-test/`, `target/` | Build output (gitignored). `dist-test/` holds the fixture (and its v2), embedded by test builds, and each locked release as `dist-test/<id>/{plugin.wasm,plugin.toml,SHA256SUMS}`, which tests install by path. |

## Build and check

```sh
bash scripts/build-plugins.sh      # or `just plugins`; test-only; needs the wasm32-wasip2 target (and network the first time)
cargo clippy --manifest-path plugins/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path plugins/Cargo.toml
cargo fmt --all --manifest-path plugins/Cargo.toml
```

The script is for the tests only; building or running the daemon doesn't need
it. It has two jobs. **The fixtures:** each `plugin.toml` checked against its
crate and the WITs (`version` equals the crate's resolved version, `api` a
served WIT package's MAJOR.MINOR — `wit/` or a frozen `wit-*/`), then built
into `dist-test/` (and the 0.1 fixture's v2). **The
locked releases:** for each lock entry, the release's `plugin.wasm`,
`plugin.toml` and `SHA256SUMS` from GitHub into `dist-test/<id>/`, both
sha256s checked against the lock (a mismatch fails the run, naming the
expected and the actual digest) and the manifest's id, version, name and
`[release] github` against the lock's. They stay there and are re-verified on
every run, so a second run fetches nothing (and a run without network needs
them there already). The layout is staged and swapped in at the end, so a
failed run keeps the last good one; a leftover `plugins/dist` or
`plugins/cache` from before the daemon stopped embedding plugins is removed.
Linux `sha256sum` or macOS `shasum`, bash 3.2. `just check` and the CI rust
job run it first: the daemon's test builds embed `dist-test/` and fail to
compile without it.

A debug daemon starts with no plugins. Install one from the Extensions tab,
with `chimaera plugin add <id>` (a first-party one, at the lock's version) or,
for a build of your own, with `chimaera plugin add --path <dir>` (a directory
holding `plugin.wasm` and `plugin.toml`, and optionally `SHA256SUMS`; the same
version again replaces it in place).

## A first-party release, end to end

1. In the plugin's repository: bump `version` in `Cargo.toml` and
   `plugin.toml` together and tag `v<version>`; its release workflow publishes
   `plugin.wasm`, `plugin.toml` and `SHA256SUMS`.
2. Daemons that have it installed offer it on the card (their checker asks
   every installed plugin whose manifest names `[release]`), and **Update**
   installs it; it stays first-party and verified past the pin.
3. Here, automatically: within the hour, `.github/workflows/plugin-lock.yml`
   (`.github/scripts/plugin-lock.mjs`, tested by `plugin-lock.test.mjs`)
   sees the newer release, downloads it, checks `SHA256SUMS` against the
   bytes and the manifest's id, version and `[release] github` against the
   lock, fetches every tool download the manifest names (`[[tools.artifacts]]`)
   and compares it with its sha256, rewrites the entry (`version`, both sha256s,
   `name` / `summary`) and
   opens `fix: update <Name> to <version>`. It auto-merges (squash) only when
   the plugin is sandboxed and the release's capability lines (`api` and every
   table but `[adds]`, `[release]` and `[detect]`) equal its pinned release's,
   so `tier` and `caps` carry over. Otherwise the PR waits for a maintainer:
   review the release, set `tier` and `caps` to what `chimaera plugin caps`
   prints for its `plugin.toml` (CI stays red until they match), merge. CI
   installs the release against the new lock; the merge cuts a patch release,
   and from it **Install** fetches that version. Run it by hand with **Actions →
   plugin-lock → Run workflow**, or locally without writing:
   `node .github/scripts/plugin-lock.mjs`. One bump PR at a time: a newer
   release waits for the open one, which the workflow keeps up to date with
   `main`, and one that can no longer land (it conflicts with a lock edited
   on `main`, or `main` already has its change) is closed as "Superseded:"
   and redone from `main`'s lock. Closing a bump PR unmerged yourself turns
   that version down for good; the workflow never reopens it. A release
   still uploading its files is picked up by the next run. A release that fails a
   check leaves the lock alone and fails the run (every hour, until a good
   release supersedes it).

   **One-time setup — the `PLUGIN_LOCK_TOKEN` secret.** A PR opened with the
   workflow's own `GITHUB_TOKEN` triggers no workflows (no CI, no `cla`
   status, so auto-merge never fires) and a merge made with it doesn't start
   `release.yml`, so the workflow writes with a token of the maintainer's: a
   fine-grained personal access token on the maintainer's account (so the PR
   author passes the CLA allowlist), repository access *only*
   `martinappberg/chimaera`, permissions **Contents: Read and write** and
   **Pull requests: Read and write**, saved as the repository secret
   `PLUGIN_LOCK_TOKEN`. Without it a run only reports the bump it would make
   (a warning). Replace it before it expires. The same secret lets
   `.github/workflows/pr-auto-update.yml` bring pull requests with
   auto-merge up to date with `main` (update-branch needs the same two
   permissions).

## Rules

- **No host call in a native test.** The wit-bindgen import stubs abort the
  test binary. Keep pure logic in functions that take data, or behind a trait
  the test implements natively (the plugin repositories do both).
- **A manifest's `provides.mcp_tools` equals the component's `tools()` names**
  — the host refuses the plugin otherwise — and `api` names the WIT version
  it was built against (`"0.1"` or `"0.2"`; the script checks it is one the
  host serves).
- **`version` is the manifest's, and it is the crate's.** Every manifest
  carries `version` (plain `MAJOR.MINOR.PATCH`) and `api`; the fixture's must
  equal the plugins workspace's (the script refuses a mismatch), and a locked
  plugin's must equal the lock's (a first-party install refuses anything
  else).
- **A release is three assets.** A plugin that updates on its own names
  `[release] github = "owner/repo"`: releases tagged `v<version>` carrying
  `plugin.wasm`, `plugin.toml` (the same manifest, that version) and
  `SHA256SUMS` (`sha256sum` format, both files), which the daemon keeps beside
  the installed copy and re-checks at every load. The daemon offers a release
  only when it is strictly newer than what runs and passes the gates,
  installs it only on the user's click (or `chimaera plugin add|update`), and
  verifies both checksums first. A first-party plugin's `[release] github`
  must be its lock `repo`, or its copy isn't first-party.
- **Nothing but `test-*` crates here.** A first-party plugin lives in its own
  repository and reaches a host through its release; the script refuses any
  other crate in this workspace.
- **A plugin never enforces a host limit itself** (rate caps, text caps, path
  confinement): the host does, for every plugin.
