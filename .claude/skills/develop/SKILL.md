---
name: develop
description: Run Chimaera locally and iterate on it — the daemon + Vite web-UI dev loop, ports, dev-mode auth, the build-embeds-UI gotcha, and where things live. Use when starting to develop or run the app, when a change needs the real daemon/UI to see, or when the dev server won't come up.
---

# Developing Chimaera locally

Chimaera is a Rust daemon (`crates/`) that embeds and serves a Svelte web UI
(`web-ui/`), plus a standalone Tauri app (`crates/chimaera-app`). See
[CLAUDE.md](../../../CLAUDE.md) for the repo map; this skill is the run loop.

## The dev loop (do this to develop the UI + daemon)

Run the daemon and the Vite dev server side by side, then work against
`http://localhost:5173`. The launch configs are in
[`.claude/launch.json`](../../launch.json):

- **chimaerad** → `cargo run -p chimaera -- serve --port 9700`
- **web-ui** → Vite on 5173, proxying `/api`, `/ws`, `/raw` to the daemon on 9700

Prefer the preview tooling to start them (`preview_start` with the `chimaerad`
then `web-ui` config), or `just`:

```sh
just dev-ui        # Vite dev server (assumes a daemon is already running)
# in another shell:
cargo run -p chimaera -- serve --port 9700
```

**Auth is automatic in dev.** Vite exposes the local daemon's
`~/.chimaera/manifest.json` at `/dev/manifest` (dev-only middleware, absent from
production builds), so the page picks up the bearer token itself — don't
hand-copy tokens. Point a second UI at a different daemon with
`CHIMAERA_DEV_TARGET=http://127.0.0.1:<port>` (see the `web-ui-sandbox` config).

## Running the built daemon (production-like)

The daemon **embeds `web-ui/dist` at compile time** (rust-embed). So a non-dev
run needs the UI built first — otherwise you serve a stale or empty bundle:

```sh
just serve         # builds web-ui/dist, then `cargo run -p chimaera -- serve`
# equivalently:
npm --prefix web-ui run build && cargo run -p chimaera -- serve
```

`serve` prints the UI URL and a bearer token. `chimaera status` / `chimaera kill`
inspect and stop a running local daemon.

## Native app

Its own cargo workspace — never built by the root `cargo` commands.

```sh
just app-dev       # cd crates/chimaera-app && cargo run  (builds the UI first)
just app-build     # bundle the .app/.dmg
```

## Remote (HPC / dev server)

```sh
just dist                       # one-time: static musl binaries into ~/.chimaera/dist
cargo run -p chimaera -- connect <host>   # install-if-missing, start-or-attach, tunnel, open UI
```

`connect` shells out to system `ssh`, inheriting `~/.ssh/config`.

## Common gotchas

- **Blank UI on a built daemon** → you didn't rebuild `web-ui/dist`. Use `just serve`.
- **Port already in use** → a daemon is still running; `chimaera kill` or pick another `--port`.
- **UI change not showing** → in the dev loop Vite HMRs; a built daemon won't — rebuild the UI.
- **Editing the app doesn't affect the daemon** and vice versa — separate workspaces.

## Before you claim it works

Verify against the real thing, not just `cargo test`. See the **verify-app** skill.
