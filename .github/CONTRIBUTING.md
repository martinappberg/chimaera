# Contributing to Chimaera

Thanks for your interest. Chimaera is early and moving fast — small, focused patches land
best. For anything larger than a bug fix, open an issue first so we can agree on the shape
before you write code.

## Dev setup

Use Node 22 (`.nvmrc`) and the Rust toolchain pinned in `rust-toolchain.toml`
(currently 1.96.0). Rustup selects that toolchain inside the repo. Build the UI
before Rust checks: `chimaera-server` embeds `web-ui/dist`.

```sh
# Web UI, then Rust and plugin test assets
nvm use 22
npm --prefix web-ui ci
npm --prefix web-ui run check     # svelte-check
npm --prefix web-ui run test      # targeted Vitest suites
npm --prefix web-ui run build     # emits web-ui/dist
just check                       # builds plugin test assets, then fmt/clippy/test

# Native shell: a separate cargo workspace, kept out of musl/HPC builds
just app-check
```

Without `just`, first run `bash scripts/build-plugins.sh`, then the daemon
workspace's fmt/clippy/test commands and the same checks with
`--manifest-path plugins/Cargo.toml` (see `justfile`). Plugin builds are test
prerequisites; running the daemon itself requires no plugin build.

A `justfile` wraps the common flows: `just check`, `just serve`, `just dev-ui`,
`just app-build`, `just release-linux` (static musl builds via cargo-zigbuild).

### Dev loop

In a worktree, use **chimaerad-isolated** from `.claude/launch.json` after the
one-time UI and `cargo build -p chimaera` builds. Its script gives this checkout
its own state dir and a free port. Open the printed `#token=…` URL. A debug daemon
reads `web-ui/dist` from disk, so UI edits need a rebuild and reload. See the
[develop workflow](../.claude/skills/develop/SKILL.md) for the full loop and native
app isolation.

For Vite HMR, run the **chimaerad** config on port 9700 and **web-ui** on 5173;
`CHIMAERA_DEV_TARGET` overrides the proxy target. Vite's dev-only `/dev/manifest`
reads `~/.chimaera/manifest.json`, while unstamped debug builds default to
`~/.chimaera-dev`. Open Vite with the debug daemon's printed token fragment.
Changing the proxy target does not change the manifest path.

## Code style

- `cargo +1.96.0 fmt --all --check` and
  `cargo clippy --workspace --all-targets -- -D warnings` must pass clean;
  CI uses the pinned toolchain. `just check` covers both Rust workspaces.
- Comments state constraints and invariants, not narration. Explain *why* the code must be
  this way ("BGZF is standard multi-member gzip, which MultiGzDecoder decodes sequentially"),
  never what the next line does.
- The daemon has a resource budget on shared hosts and cluster allocations: keep allocations bounded,
  no unbounded buffers, no busy loops. Treat that as a review criterion, not a nice-to-have.

## Verification culture

Features are verified live before they land — not just unit-tested. If you change behavior,
drive it: run the daemon, attach the UI, and exercise the actual flow (spawn the session,
kill the socket, reattach, resize). Terminal state, reconnect semantics, and agent
integrations have all had bugs that only reproduce against the real thing. PRs should say
what you ran and what you observed, alongside the tests.

Tests still matter: `cargo test --workspace` covers the daemon, and new server behavior
should come with tests at that level.

For documentation and agent guidance changes, run the relevant repository checks:

```sh
node scripts/check-doc-links.mjs
node scripts/check-agent-assets.mjs
node scripts/check-workflow-security.mjs  # if workflow guidance or configuration changed
git diff --check
```

These changes do not need a live daemon unless they also change runtime behavior.
Extension authors can start with the verified scaffold and author checklist in the
[plugin development guide](../docs/agent-guides/plugins.md).

## Releases and update signing

**A releasing merge to `main` cuts a published release.** `.github/workflows/release.yml`
derives the next version from the last git tag, bumped by the largest bump the squash-merge
**subjects** since that tag ask for
(`feat:` → minor, `fix:`/`perf:`/`revert:` → patch, `!` → major; `refactor:`/`chore:`/`docs:`/… and
`[skip release]` request **no** release), builds Linux musl and macOS daemon
binaries plus the macOS, Linux and Windows native apps, and **publishes** the
GitHub Release. A docs-only merge can still trigger a pending release requested
by an earlier merge since the last tag. Published releases carry a `latest.json`
for the app's update checks; the download is verified against
a minisign public key embedded in the app, so only a release signed with the matching private
key can ever install.

The full version mapping and how to skip a release live in
**[docs/agent-guides/releases.md](../docs/agent-guides/releases.md)** (the single source).

Two repo secrets sign updates:

- `TAURI_SIGNING_PRIVATE_KEY` — the minisign private key (generate once with
  `npx tauri signer generate`; keep it out of the repo).
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — its password (empty string if you generated
  without one).

The matching public key lives in `crates/chimaera-app/tauri.conf.json` under
`plugins.updater.pubkey` and is safe to commit. Rotating the private key means bumping the
public key there, and clients on the old key will stop auto-updating until they reinstall.
Until an Apple Developer ID is configured the bundles are unsigned by Apple (Gatekeeper),
which is independent of update signing; see the note in `.github/workflows/app.yml`.

## License and CLA

Chimaera is licensed under the AGPL-3.0 and dual-licensed commercially (see
[README](../README.md#license)). To keep the dual-licensing model possible, contributions
require a lightweight Contributor License Agreement granting Martin Kjellberg
(mkjberg@gmail.com) the right to relicense contributed code. You keep the copyright to your
contribution; the CLA grants relicensing rights, nothing more. The full text is in
[CLA.md](../CLA.md).

Your first pull request gets an automated comment from the CLA bot with a link to the
agreement. If you agree, sign by replying to the PR with a single line:

```
I have read the CLA Document and I hereby sign the CLA
```

The bot records your signature and the check goes green; you only sign once, and it
remembers you for every future PR. Signing is self-hosted — it runs entirely in GitHub
Actions using the repository-owned `.github/scripts/cla.mjs` gate, with signatures stored
on the `cla-signatures` branch, so no third-party CLA service ever sees your data. The gate
keys signatures to GitHub's immutable user id and checks every linked commit co-author.
