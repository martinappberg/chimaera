# Contributing to Chimaera

Thanks for your interest. Chimaera is early and moving fast — small, focused patches land
best. For anything larger than a bug fix, open an issue first so we can agree on the shape
before you write code.

## Dev setup

You need stable Rust and Node.

```sh
# Rust workspace (daemon, CLI, server, PTY layer)
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings

# Web UI
npm --prefix web-ui install
npm --prefix web-ui run check     # svelte-check
npm --prefix web-ui run build     # emits web-ui/dist, which the daemon embeds

# Native shell (a standalone cargo workspace — the Tauri stack deliberately
# never enters the daemon workspace, so musl/HPC builds stay lean)
cd crates/chimaera-app && cargo check
```

A `justfile` wraps the common flows: `just check`, `just serve`, `just dev-ui`,
`just app-build`, `just release-linux` (static musl builds via cargo-zigbuild).

### Dev loop

The repo ships launch configurations in `.claude/launch.json`:

- **chimaerad** — `cargo run -p chimaera -- serve --port 9700`, the daemon.
- **web-ui** — the Vite dev server on port 5173, proxying `/api`, `/ws`, and `/raw` to the
  daemon on 9700 (override the target with `CHIMAERA_DEV_TARGET`).

Run both and develop against `http://localhost:5173`. The Vite dev server exposes the local
daemon's `~/.chimaera/manifest.json` at `/dev/manifest` (dev-only middleware, never present
in a production build), so the dev page authenticates itself without hand-copying tokens.

## Code style

- `cargo fmt` and `cargo clippy --workspace --all-targets -- -D warnings` must both pass
  clean; CI enforces them.
- Comments state constraints and invariants, not narration. Explain *why* the code must be
  this way ("BGZF is standard multi-member gzip, which MultiGzDecoder decodes sequentially"),
  never what the next line does.
- The daemon has a resource budget (it lives on shared login nodes): keep allocations bounded,
  no unbounded buffers, no busy loops. Treat that as a review criterion, not a nice-to-have.

## Verification culture

Features are verified live before they land — not just unit-tested. If you change behavior,
drive it: run the daemon, attach the UI, and exercise the actual flow (spawn the session,
kill the socket, reattach, resize). Terminal state, reconnect semantics, and agent
integrations have all had bugs that only reproduce against the real thing. PRs should say
what you ran and what you observed, alongside the tests.

Tests still matter: `cargo test --workspace` covers the daemon, and new server behavior
should come with tests at that level.

## Releases and update signing

Pushing a `v*` tag triggers `.github/workflows/release.yml`, which builds the static musl
daemon binaries and the signed macOS app, then opens a **draft** GitHub Release you review
and publish. Published releases carry a `latest.json` the installed app polls to
auto-update itself; the download is verified against a minisign public key embedded in the
app, so only a release signed with the matching private key can ever install.

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
[README](README.md#license)). To keep the dual-licensing model possible, contributions
require a lightweight Contributor License Agreement granting Martin Kjellberg
(mkjberg@gmail.com) the right to relicense contributed code. You keep the copyright to your
contribution; the CLA grants relicensing rights, nothing more.

Your first pull request will get an automated CLA-assistant prompt to sign. Until that bot
is wired up, please add a DCO sign-off to your commits (`git commit -s`) and note in the PR
that you agree to the CLA terms.
