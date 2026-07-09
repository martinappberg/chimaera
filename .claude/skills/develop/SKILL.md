---
name: develop
description: Run Chimaera locally and iterate on it — the daemon + Vite web-UI dev loop, ports, dev-mode auth, the build-embeds-UI gotcha, the isolated per-worktree daemon (chimaerad-isolated), and where things live. Use when starting to develop or run the app, when a change needs the real daemon/UI to see, when working in a git worktree or alongside another daemon, or when the dev server won't come up (port in use, Node version, blank UI).
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

## Isolated per-worktree run (coding agents — use this in a worktree)

The default `chimaerad` / `web-ui` configs hardcode ports 9700 / 5173 and share
`~/.chimaera`. That collides the moment a second worktree or chat is live:
`serve` **writes the manifest on start and REMOVES it on stop**, so a shared
daemon deletes a sibling's manifest and breaks its dev auth. When another
daemon may be up, run **`chimaerad-isolated`** instead — a self-contained
daemon just for this worktree:

```
preview_start  →  config "chimaerad-isolated"
```

It runs [`serve-isolated.sh`](serve-isolated.sh), which sets
`CHIMAERA_HOME=<worktree>/.chimaera-dev` (its own manifest/workspaces/recents,
gitignored) and takes an **auto-assigned free port** (`autoPort`, via `$PORT`).
`CHIMAERA_HOME` moves only the daemon's bookkeeping — spawned shells/agents keep
the real `$HOME`, so `~/.claude` auth still works. No Vite, so no 5173 clash and
no Node needed at run time.

**One-time build first** (the script fails fast if either is missing):

```sh
cargo build -p chimaera                       # builds target/debug/chimaera
nvm use 22 && npm --prefix web-ui ci \
  && npm --prefix web-ui run build            # Node 22 — the nvm default (16) errors
```

Then `preview_start chimaerad-isolated`, read the printed
`http://127.0.0.1:<port>/#token=…` from `preview_logs`, and navigate the preview
there (serve mode has no `/dev/manifest` auto-auth — the token rides the URL
fragment). A **debug** daemon reads `web-ui/dist` from disk per request, so after
a UI change just rebuild the UI and reload the page — no daemon restart.

To reset this worktree's isolated daemon state, delete `.chimaera-dev/`.

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
just app-dev            # cd crates/chimaera-app && cargo run  (builds the UI first)
just app-dev-isolated   # the app on an ISOLATED state dir — use this in a worktree / alongside your real app
just app-build          # bundle the .app/.dmg
```

**Isolated app — the app counterpart of `chimaerad-isolated`.** `app-dev` (and a
released app) share `~/.chimaera` — a dev build would fight your real app over
the manifest, port, saved hosts, and window registry. `app-dev-isolated` runs
[`run-app-isolated.sh`](run-app-isolated.sh), which sets `CHIMAERA_HOME=
<worktree>/.chimaera-dev-app` before launching the built binary. Because the app,
the `--daemon` it spawns (a free port, THIS worktree's build), and the shells
that daemon spawns all inherit `CHIMAERA_HOME`, the whole stack is isolated —
nothing lands in the shared `~/.chimaera`. It opens on an empty home (no saved
workspaces/hosts), which is how you tell it apart from your real window. Use it to
exercise the full binary end to end — the native clipboard command, the reauth
overlay, the daemon changes — which the browser preview can't reach. A debug
daemon reads `web-ui/dist` from disk, so after a UI change rebuild the UI and
reload the window; no app restart. (Verifying a REMOTE flow this way still targets
the host's shared `~/.chimaera` over ssh — mind a running real session there.)

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
