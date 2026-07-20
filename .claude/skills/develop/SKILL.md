---
name: develop
description: Run Chimaera locally and iterate on it — the daemon + Vite web-UI dev loop, ports, dev-mode auth, the build-embeds-UI gotcha, the isolated per-worktree daemon (chimaerad-isolated), and where things live. Use when starting to develop or run the app, when a change needs the real daemon/UI to see, when working in a git worktree or alongside another daemon, or when the dev server won't come up (port in use, Node version, blank UI).
---

# Developing Chimaera locally

Chimaera is a Rust daemon (`crates/`) that embeds and serves a Svelte web UI
(`web-ui/`), plus a standalone Tauri app (`crates/chimaera-app`). See
[AGENTS.md](../../../AGENTS.md) for the repo map; this skill is the run loop.

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
~/.chimaera-dev-app/<worktree-key>` before launching the built binary. The key
includes a stable checksum of the absolute worktree path because linked
worktrees commonly all end in `chimaera`. Because the
app, the `--daemon` it spawns (a free port, THIS worktree's build), and the
shells that daemon spawns all inherit `CHIMAERA_HOME`, the whole stack is isolated
— nothing lands in the shared `~/.chimaera` — yet still per-worktree.

The launcher also runs a generated `chimaera-dev` identity rather than
`chimaera` (on macOS, a small `target/debug/chimaera-dev.app` wrapper with its
own bundle id; elsewhere, a `target/debug/chimaera-dev` hard link). It also
builds with a per-worktree Tauri identifier: the single-instance plugin keys
its socket/service from compiled configuration, not wrapper metadata. This keeps
the isolated GUI process distinct in Activity Monitor and Computer Use while
the released app is open; for live automation, target **`chimaera-dev`**, never
the user's `chimaera`.

**Why `~/…` and not `<worktree>/.chimaera-dev-app`:** the state dir holds unix
sockets (the askpass relay, the ssh ControlMaster) whose *full path* must fit the
~104-byte `sun_path` limit. A CHIMAERA_HOME inside the deep worktree path
overshoots it — the socket binds fail (askpass can't reach the app, so ssh auth
dies; ssh mux fails with "ControlPath too long"). Anchoring under a short `~`
base keeps every socket path legal.

It opens on an empty home (no saved workspaces/hosts), which is how you tell it
apart from your real window. Use it to exercise the full binary end to end — the
native clipboard command, the reauth overlay, the daemon changes — which the
browser preview can't reach. A debug daemon reads `web-ui/dist` from disk, so
after a UI change rebuild the UI and reload the window; no app restart.
(For a REMOTE flow, pair it with a **dev host** — the next section — so the
host side is isolated too.)

## Remote (HPC / dev server)

```sh
just dist                       # one-time: static musl binaries into ~/.chimaera/dist
cargo run -p chimaera -- connect <host>   # install-if-missing, start-or-attach, tunnel, open UI
```

`connect` shells out to system `ssh`, inheriting `~/.ssh/config`.

## Remote isolated dev daemon (dev is dev — no toggle)

Dev-ness is the BUILD's property: a dev build (never release-stamped —
`chimaera_core::is_dev_build()`, the `0.0.1` sentinel) targets
`~/.chimaera-dev` on BOTH ends, always. Locally, a dev build with no
`CHIMAERA_HOME` defaults its own state to `~/.chimaera-dev` (the isolated-rig
script still overrides with its per-worktree home); remotely, every `connect`
from a dev build runs against the host's `~/.chimaera-dev`. A release always
targets `~/.chimaera`. There is no flag, no per-host toggle — neither world
can reach the other's daemon:

```sh
just dist                                  # musl-build THIS worktree into ~/.chimaera/dist
cargo run -p chimaera -- connect <host>    # dev build ⇒ deploy + start under ~/.chimaera-dev
cargo run -p chimaera -- status <host>     # dev build ⇒ the dev daemon's manifest/port/pid/build
```

The whole remote side effect is scoped to `~/.chimaera-dev` on the host: the
binary (`~/.chimaera-dev/bin/chimaera`), the daemon (started under
`CHIMAERA_HOME=~/.chimaera-dev`, so its manifest and state live in
`~/.chimaera-dev/data/`), and the probe/reuse/replace decision. The real daemon
is never probed, stopped, or replaced — the two run side by side. A dev connect
never downloads a release: no local build (`just dist` stash or `--binary`) is
a hard error, so a release binary can't silently impersonate your build. The
same principle runs the other way: a dev app never OFFERS a release update
(`check_app_update` reports none) — an "update" would swap the build under
test.

In the native app every host row wears the amber `dev` pill (the build is dev,
so every connection is). The isolated app (`just app-dev-isolated`) + any host
is the full end-to-end rig — your app build tunneling to your daemon build on
a real cluster — which is how you exercise remote-only paths like OSC 52
clipboard from a remote TUI or the reauth overlay on a tunnel drop.

Gotchas:

- **Same-commit rebuilds look identical**: build ids compare the git hash, so
  a rebuilt (even dirty) tree won't auto-replace a running dev daemon. Force it
  with `--update-daemon` / the row's "update" action.
- **A dev build cannot attach to the real `~/.chimaera` daemon at all** (and a
  release can't attach to a dev one). To exercise the real daemon, use a
  release build.
- **Old hosts.json entries with a `"dev"` key** (from the toggle era) still
  parse; the key is ignored.

## Handing the human a live app to test (the flow to prompt them with)

When you finish a change and want **the human** to test it in the real app
(not your headless preview), give them a running isolated app — and if they want
to test a **remote host**, stage the cross-built daemon first so their in-app
connect Just Works. This is the flow to walk them through.

**1. Launch the isolated native app** (a real window, isolated state, won't touch
their real `~/.chimaera` app). `just app-dev-isolated`, or when `just` is absent,
its two steps — the last one **in the background** (it's a long-running GUI):

```sh
nvm use 22 && npm --prefix web-ui run build          # UI is embedded/served from dist
bash .claude/skills/develop/run-app-isolated.sh      # builds + runs in background — opens the window
```

State lives under `CHIMAERA_HOME=~/.chimaera-dev-app/<worktree-key>`; the script
scrubs `CLAUDE*`/`ANTHROPIC*` env (else the app's spawned agents die on start)
and launches the generated `chimaera-dev` app identity. Computer Use must
target `chimaera-dev`, which prevents it from driving the user's released app.
Then tell the human: open a folder → start an agent → here's what to click.

**2. To test against a REMOTE host from that isolated app** (e.g. Sherlock): a
dev connect deploys **your** build, never a release, so a musl daemon of this
branch must exist where THIS app looks. The trap: the in-app hint says
`~/.chimaera/dist`, but an **isolated** app reads its own
`$CHIMAERA_HOME/data/dist` — cross-build and stage there by the exact
`chimaera-<arch>-linux-musl` name (`dist_name`, `chimaera-remote`):

```sh
# needs zig + cargo-zigbuild; arch = the host's (Sherlock = x86_64)
cargo zigbuild --release --target x86_64-unknown-linux-musl -p chimaera
HOME_DIR="$(bash .claude/skills/develop/run-app-isolated.sh --print-home)"
mkdir -p "$HOME_DIR/data/dist"
cp target/x86_64-unknown-linux-musl/release/chimaera \
   "$HOME_DIR/data/dist/chimaera-x86_64-linux-musl"
```

Then tell the human: **click the host row (it wears the amber `dev` pill) → connect.**
The app deploys your build to the host's `~/.chimaera-dev`, starts the remote
daemon, tunnels, and opens the remote workspace; an ssh password/Duo prompt
surfaces in the app's askpass overlay. Now they can exercise remote-only paths
(uploads streaming to the host, remote terminal links, reconnect/reauth).

## Which build is actually running? (debug workflow)

- **Health = ground truth.** Read `port` + `token` from the manifest —
  `$CHIMAERA_HOME/data/manifest.json` for an isolated daemon/app, the plain
  `~/.chimaera/manifest.json` otherwise — then
  `curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:$PORT/api/v1/health`.
  Its `build` must match your HEAD (`git rev-parse --short HEAD`, `-dirty` if
  the tree is). Run the same check against the app's TUNNEL port to verify a
  remote dev daemon end to end; `chimaera status <host>` (from the dev build) asks the host
  directly.
- **Logs.** The app logs to the terminal that launched it; its local daemon to
  `$CHIMAERA_HOME/data/logs/serve.log`; a remote dev daemon to
  `~/.chimaera-dev/data/logs/serve.log` on the host (real: `~/.chimaera/logs/`).
- **Sockets are health.** While the app runs, its askpass relay socket must
  EXIST — `$CHIMAERA_HOME/run/askpass.sock` (missing = the bind failed and ssh
  auth will die promptless). For a too-deep home, both it and the ssh
  ControlMaster fall back to a short `/tmp/chimaera-<hash>/` dir to stay under
  the ~104-byte `sun_path` limit — the app's "askpass ready on …" log line
  says where it actually bound.
- **Build skew is by design, not an error:** connect Reuses a matching build,
  Updates a mismatched idle daemon, and attaches "outdated" to a mismatched
  busy one (the row/CLI then offer the explicit update).

## Common gotchas

- **Blank UI on a built daemon** → you didn't rebuild `web-ui/dist`. Use `just serve`.
- **Port already in use** → a daemon is still running; `chimaera kill` or pick another `--port`.
- **UI change not showing** → in the dev loop Vite HMRs; a built daemon won't — rebuild the UI.
- **Editing the app doesn't affect the daemon** and vice versa — separate workspaces.

## Before you claim it works

Verify against the real thing, not just `cargo test`. See the **verify-app** skill.
