# CLAUDE.md

Orientation for coding agents. Read this first — it exists so you don't have to
re-derive the project every session. **[DESIGN.md](DESIGN.md) is the source of
truth** for architecture and rationale (`## Architecture` especially); this file
is the fast map and the working rules.

## What Chimaera is

An **agent workbench, not an IDE**. One static Rust binary (`chimaera`) is the
whole server: it owns your agent sessions as persistent, daemon-owned PTY
processes on whatever host owns the work — your laptop, a dev server, or an HPC
login node — and serves a workspace-first web UI around them (file previews,
terminals, git state). Agents run as the real interactive TUIs (`claude` and
friends) in daemon-owned PTYs, so they look, behave, and bill exactly like they
do in any terminal. Close the laptop mid-run and nothing dies — windows are just
views onto the daemon. A native Tauri app wraps the same UI in real windows.

The name is lowercase **chimaera** everywhere it's a binary, product, or dock
label; "Chimaera" as the capitalized proper noun in prose.

## Repo layout

Rust workspace (the daemon) + a Svelte web UI it embeds + a separate Tauri app.

| Path | What it is |
|---|---|
| `crates/chimaera` | The binary. CLI + daemon entrypoint: `serve`, `connect <host>`, `status`, `kill`, `doctor`. |
| `crates/chimaera-core` | Shared types, version/build-id helpers, shell integration. |
| `crates/chimaera-pty` | The persistent terminal engine. PTY sessions mirrored into a headless `alacritty_terminal` grid so full screen state survives with zero attached clients; `attach` returns a snapshot escape stream that rebuilds the terminal in a fresh xterm.js. |
| `crates/chimaera-remote` | SSH orchestration for `connect`: host discovery, musl-binary install, daemon start, port-forward tunnels. Never reimplements the ssh client — inherits `~/.ssh/config` (ProxyJump, ControlMaster, 2FA). |
| `crates/chimaera-server` | The daemon's HTTP/WS surface and all business logic: `workspaces`, `agents`, `launcher`, `runtimes`, `fs`/previews, `links` + `mcp` (linked terminals), `settings`, `quickopen`, `recents`, `naming`, `view_state`, `ws`, `api`, `assets`. Embeds `web-ui/dist`. |
| `crates/chimaera-app` | The Tauri 2 native shell. **Its own standalone cargo workspace** — Tauri deliberately never enters the daemon workspace so musl/HPC builds stay lean. |
| `web-ui/` | The client: Svelte 5 + Vite, xterm.js terminals, file previews (image/markdown/csv/pdf/html), the workbench layout. The daemon serves the built `dist/`. |

## Build, test, run

`rust-toolchain.toml` pins the compiler channel and musl targets; `.nvmrc` pins
Node (UI build needs >= 18). `just` wraps the common flows.

```sh
# Full local gate (matches CI): fmt + clippy + test
just check
#   = cargo fmt --all --check
#     cargo clippy --workspace --all-targets -- -D warnings
#     cargo test --workspace

# Web UI
npm --prefix web-ui install
npm --prefix web-ui run check      # svelte-check
npm --prefix web-ui run build      # emits web-ui/dist, which the daemon embeds

# Native shell (separate workspace)
cd crates/chimaera-app && cargo check      # or: just app-build
```

**The daemon embeds `web-ui/dist` at compile time (rust-embed).** Any *release/
production* daemon run needs the UI built first — `just serve` and `just ui` do
this for you. In *dev* you don't rebuild: run the daemon + Vite and let the proxy
serve live UI (below).

### Dev loop

`.claude/launch.json` ships two configs — run both, develop against
`http://localhost:5173`:

- **chimaerad** — `cargo run -p chimaera -- serve --port 9700` (the daemon).
- **web-ui** — Vite on 5173, proxying `/api`, `/ws`, `/raw` to the daemon on 9700
  (override target with `CHIMAERA_DEV_TARGET`).

Vite exposes the local daemon's `~/.chimaera/manifest.json` at `/dev/manifest`
(dev-only middleware, never in a production build), so the dev page authenticates
itself — no hand-copying bearer tokens.

See the **[develop](.claude/skills/develop/SKILL.md)** skill for the full loop.

## Working rules

- **Verify live, don't just unit-test.** Terminal state, reconnect semantics, and
  agent integrations have all had bugs that only reproduce against the real thing.
  If you change behavior, drive it: run the daemon, attach the UI, exercise the
  flow (spawn a session, kill the socket, reattach, resize). PRs say what you ran
  and observed. See the **[verify-app](.claude/skills/verify-app/SKILL.md)** skill.
- **The daemon lives on shared login nodes.** Bounded allocations, no unbounded
  buffers, no busy loops, hard ceilings on preview extraction. Target ~150 MB RSS,
  <1 core steady-state. Resource discipline is a review criterion, not a nicety.
- **No SQLite anywhere near NFS/Lustre.** Durable state is append-only,
  size-capped JSONL under `~/.chimaera`; hot state (`$XDG_RUNTIME_DIR` / `/tmp`)
  must be treated as reconstructible (it gets night-scrubbed).
- **Terminal state is server-side.** Never serialize the `alacritty` `Term` grid
  across the wire (the client is xterm.js, not Rust) — re-emit an escape-sequence
  snapshot on attach/resize. Resize/resync has subtle invariants; read DESIGN.md
  `## Architecture` → "Resize repaint refinement" before touching that path.
- **Comments state constraints and *why*, not narration.** Explain why the code
  must be this way ("BGZF is standard multi-member gzip"), never what the next
  line does. `cargo fmt` and `clippy -D warnings` must pass clean.
- **UI quality is an acceptance criterion.** Curated light/dark themes, the brand
  mark (`web-ui/src/lib/BrandMark.svelte`), and a real workbench feel — hold the
  bar.

## Releases & how to skip one

CI and releases are automatic; know which knob you're touching.

- **`ci.yml`** — every PR + push to main: svelte-check + UI build, then
  `cargo fmt`/`clippy`/`test`, plus musl cross-builds. Branch protection on `main`
  requires it green.
- **`app.yml`** — PR-only build-check for the Tauri bundle. Runs *only* when
  `crates/chimaera-app/**` or `web-ui/**` change (macOS runners are expensive). It
  builds without updater artifacts (`createUpdaterArtifacts` off), so it needs **no
  signing key** — signing is a release-only concern (`release.yml` on `main`). The
  PR bundle carries the `0.0.1` sentinel version; harmless, it's never published.
- **`release.yml`** — **every merge to `main` cuts a PUBLISHED GitHub release**
  (daemon musl + macOS binaries + signed app). The installed app auto-updates
  from it. The version is derived from the last git tag and bumped by the
  **squash-merge commit message** (Conventional Commits): `feat:` → minor,
  `BREAKING CHANGE` / `!:` → major, else patch. So prefix commits correctly.

### `[skip release]`

To land a PR **without** cutting a release (docs, chores, tooling — anything that
shouldn't ship a version), put **`[skip release]`** in the squash-merge commit
message. The squash subject defaults to the **PR title**, so the reliable way to
tag a no-release PR is to **put `[skip release]` in the PR title** (and/or the
body). `release.yml`'s `version` job is gated on
`!contains(head_commit.message, '[skip release]')`, so the whole release is
skipped.

## PR checklist

1. `just check` green; if you touched `web-ui/**` or the app, expect `app.yml` too.
2. Verified live (not just tests) — note what you ran and observed.
3. Conventional-Commit prefix so the auto version bump is right, **or**
   `[skip release]` in the PR title for no-release changes.
4. First-time contributors: CLA sign-off (see [CONTRIBUTING.md](CONTRIBUTING.md)).

## Project skills

Under `.claude/skills/` — invoke with `/<name>`:

- **develop** — run Chimaera locally and iterate (daemon + Vite dev loop, ports, auth, gotchas).
- **verify-app** — drive a change end-to-end against the real daemon + UI + PTY.
- **ship-pr** — open a PR here: CI gates, Conventional-Commit version bump, `[skip release]`.
