# Claude Code cloud sessions

How Chimaera is set up for Claude Code **cloud sessions** — the Anthropic-hosted VMs
behind [claude.ai/code](https://claude.ai/code), the desktop app's *Continue in →
cloud*, `claude --cloud`, and scheduled routines. Each session is a fresh Ubuntu
24.04 x86_64 VM (~4 vCPU / 16 GB / 30 GB) holding a fresh clone of this repo.
These are the Anthropic-hosted defaults, with approximate ceilings that can
change; self-hosted environments have their own image and network. Provider
setup checked on 2026-10-01 against the official
[cloud environments documentation](https://code.claude.com/docs/en/cloud-environments).

## One-time setup (maintainer, on claude.ai)

1. **Connect GitHub.** Install the Claude GitHub app on `martinappberg/chimaera`
   during web onboarding (or run `/web-setup` from a local `claude` to hand it your
   `gh` token).
2. **Create an environment** (claude.ai/code → cloud environment selector → *Add cloud
   environment*), e.g. `chimaera`:
   - **Network access: Trusted** (the default). It already allows everything the
     build needs — crates.io + `static.rust-lang.org`, the npm registry, GitHub
     (incl. `raw.githubusercontent.com`), and the Ubuntu apt mirrors.
   - **Environment variables:** none needed. They're readable by anyone using the
     environment, so never put secrets there. `gh` is authenticated by the GitHub
     proxy with no token. Optional: `CHIMAERA_CLOUD_WARM=0` turns off the
     background cargo warm-up (below).
   - **Setup script:** paste the contents of
     [`scripts/cloud-env-setup.sh`](../../scripts/cloud-env-setup.sh). Re-paste when
     that file changes — the environment doesn't read it from the repo.
3. **Smoke-test the first session**: ask it to run `check-tools`, `rustup show`,
   `just --version`, and `cat target/cloud-bootstrap.log`, then `just check`.

## What runs, and when

| Step | Where it lives | When |
|---|---|---|
| Setup script: pinned Rust toolchain + musl/WASM targets, `just` | the environment (source: `scripts/cloud-env-setup.sh`) | once per environment cache (~7 days, or when the script / network settings change); later sessions start from its filesystem snapshot |
| `cloud-bootstrap.sh`: `npm ci`, build `web-ui/dist`, and build plugin test assets, then a background `cargo clippy` + `cargo test --no-run` warm-up | [`.claude/hooks/cloud-bootstrap.sh`](../../.claude/hooks/cloud-bootstrap.sh), SessionStart `startup\|resume` | every cloud session; a no-op locally (`CLAUDE_CODE_REMOTE` gate) |
| `session-orient.sh` | SessionStart | every session; prints a cloud-specific verify line when `CLAUDE_CODE_REMOTE=true` |

`web-ui/dist` must exist before workspace build, clippy, or test commands that
compile `chimaera-server`: rust-embed derives from it at compile time. Formatting
and checks confined to other crates do not need the bundle. The bootstrap builds
it synchronously. The
warm-up log is `target/cloud-bootstrap.log`; a cargo command started while it runs
waits on cargo's build lock and then reuses its work.

Everything committed under `.claude/` (hooks, skills, agents, rules) and `AGENTS.md` /
`CLAUDE.md` loads in a single-repo cloud session. What does **not** reach the cloud:
the user-level `~/.claude` (personal memory, skills, plugins), and — in a
multi-repo session — the repo's hooks.

## Verifying "live" in the cloud

The repo rule is *verify live, don't just unit-test*. In the cloud:

- **Works:** `just check`; `npm --prefix web-ui run check` / `test` / `build`; the
  repo scripts (`check-doc-links`, `check-agent-assets`, `check-workflow-security`,
  `guard-bash.test.sh`); and a real daemon, run headless on an isolated state dir
  and driven over HTTP/WS:

  ```sh
  cargo build -p chimaera
  PORT=9741 bash .claude/skills/develop/serve-isolated.sh >target/daemon.log 2>&1 &
  ```

  Wait until `target/daemon.log` shows the new daemon's token URL before using
  its manifest. Then:

  ```sh
  node scripts/smoke-daemon.mjs     # workspace → shell → WS round trip → cleanup
  jq -r '"url = \"http://127.0.0.1:\(.port)/api/v1/sessions\"\nheader = \"Authorization: Bearer \(.token)\""' \
    .chimaera-dev/data/manifest.json | curl --silent --show-error --fail --config -
  ```

  [`scripts/smoke-daemon.mjs`](../../scripts/smoke-daemon.mjs) is the pattern to
  extend for a change-specific check: REST calls take the manifest token as a
  bearer header; the WebSockets (`/ws/sessions/{id}`, `/ws/chat/{id}`,
  `/ws/events`) take it as the first text frame `{"type":"auth","token":…}`, and on
  a terminal socket binary frames are PTY bytes both ways (protocol:
  `crates/chimaera-server/src/ws.rs`). Redirect the daemon's stdio as above — a
  daemon whose stdout is gone fails every request that logs.
  The curl example sends its token on stdin rather than exposing it in process arguments.
- **Local-only** (say so in the PR instead of claiming it): visual checks in the
  browser pane (there is none; whether the image carries a headless Chrome is
  unverified), `just chat-smoke` (needs authenticated `claude`/`codex` CLIs and bills
  real turns), HPC hosts over SSH (`connect`, real clusters), the Tauri app and WebKit
  harnesses, and the musl `zigbuild` release builds (CI covers those).

## Git and PRs

The clone's remote is **`origin`**, as everywhere else. The GitHub proxy lets a
session push **only its own working branch**; fetch, `gh`, and PR creation work
normally. To continue a cloud session on your machine: `claude --teleport`
(needs a clean tree and the branch pushed).
