# Remote connect (SSH orchestration)

`chimaera connect <host>` brings up a Chimaera daemon on a remote ssh host — a dev server or
an HPC login node — and forwards a local port to it, so the same UI drives work on the machine
that owns it. The daemon is auto-deployed, the ssh multiplexing and auth are handled for you,
and the native app uses the exact same library. This is what makes Chimaera an HPC tool and not
just a local one.

**Where it lives (shared):** the library is `crates/chimaera-remote/src/{lib.rs,hosts.rs}`
(thorough in-code docs); the CLI driver is `crates/chimaera/src/connect.rs`; the native app's
host management wraps it in `crates/chimaera-app/src/shell/connect.rs` + `askpass.rs`. UI:
`web-ui/src/lib/net/native.ts` (the Tauri bridge), `web-ui/src/lib/workspace/{AskpassModal,
ReauthOverlay}.svelte`, and the remote-hosts section of `HomeScreen.svelte`. This crate **can't
be live-verified in CI** (no remote host) — its decision phase is characterization-tested behind
a `RemoteOps` trait. See also [native-app.md](native-app.md) for the windows/host-management UI.

## The connect flow

- **What & when.** Connect to (and stand up a daemon on) a remote host, then open a tunnelled
  window onto it.
- **How it's used (CLI).** `chimaera connect <host> [--local-port N] [--binary PATH] [--no-open]
  [--update-daemon]`. Progress phases (probing / updating / downloading / installing /
  starting / tunneling) stream to the UI. In the native app, "add a host…" on the home screen does
  the same and lists that host's workspaces inline.
- **Where it lives.** `chimaera-remote/src/lib.rs` (`connect`, `resolve_daemon`, `Tunnel`,
  `deploy_binary`, `start_remote`, `fetch_release_binary`, `spawn_tunnel`), `hosts.rs`
  (`HostsStore`, `normalize_alias`).
- **The steps.** (1) **Normalize** the alias (strip a typed `ssh ` prefix, reject flags/whitespace).
  (2) **Probe** (`resolve_daemon`): `cat ~/.chimaera/manifest.json` + `kill -0 pid` over ssh →
  *Reuse* if a matching-build daemon runs; if builds differ, count live sessions and *Update* if
  provably idle (or `--update-daemon`), else *ConnectOutdated*; no daemon → *fresh start*.
  (3) **Resolve the binary** to deploy — explicit `--binary`, else auto-fetch the matching
  musl/darwin release build (sha256-verified); the `~/.chimaera/dist/` stash feeds **dev connects
  only** (a stash build is the `0.0.1` sentinel, which relocates its state to `~/.chimaera-dev` and
  can never serve the real home) — always **before**
  stopping any running daemon (a failed fetch must never strand a host with no daemon). (4) **Deploy**
  via `scp` (staged `.new` + `mv -f`), **start** (`chimaera serve --daemonize`, which forks +
  `setsid(2)`s in-process so it needs no util-linux `setsid`/`nohup` and works on any POSIX remote —
  Linux, macOS, BSD; falls back to `setsid nohup … & disown` for a pre-flag remote binary; poll the
  manifest ≤15s). (5) **Tunnel** (`ssh -N -L` with `ExitOnForwardFailure`, poll the port ≤15s) and
  open `http://127.0.0.1:{port}/#token={token}&host={alias}`.

## Key behaviors & gotchas

- **One ControlMaster per host.** Every ssh/scp call rides one chimaera-owned master
  (`ControlMaster=auto`, `ControlPersist=10m`): the user authenticates **once** (password or 2FA/Duo,
  inherited from `~/.ssh/config` — the ssh client is never reimplemented), and every subsequent
  command/tunnel/window multiplexes it.
- **In-app auth.** ssh has no tty under the native shell, so an `SSH_ASKPASS` relay surfaces the raw
  prompt (password, keyboard-interactive Duo passcode) in `AskpassModal.svelte`. Prompts **queue**
  (ssh asks sequentially). Every ssh/scp child stamps its host alias into the relay, so a remote
  window sees auth only for its own host; a local home window remains the fallback for startup
  restore or a first connection made before any remote window exists. The native shell enforces
  that boundary on targeted events, pending-list reads, and answers using the immutable host scope
  it registered when the window opened — a daemon-served page cannot widen it client-side. Startup
  registers a home fallback before launching restored remote connects when only local workspace
  windows were persisted, so an early password or 2FA prompt always has an eligible surface. A
  window created as local Home retains that shell-owned fallback identity if it opens a workspace
  while SSH is still waiting; mutable workspace reports cannot revoke or grant another host scope.
- **Liveness is an HTTP probe, not a bare TCP connect** — after laptop sleep an ssh forward's local
  listener still accepts while the connection behind it is dead. Initial tunnel polling accepts any
  HTTP response (`http_alive`); once a manifest/token is known, native reuse and health monitoring
  require a bearer-authenticated 200 (`http_alive_authed`), so a 401 or a foreign service on a
  recycled port cannot be mistaken for the intended daemon.
- **TOFU host keys.** `StrictHostKeyChecking=accept-new` lets a windowed app with no tty reach a
  never-seen host (it still refuses a *changed* key). `ServerAliveInterval/CountMax` notice a dead
  link within ~45s.
- **A fresh start version-probes the installed binary** (`ensure_remote_binary` runs
  `~/.chimaera/bin/chimaera --version` over ssh): a dev (`0.0.1`) binary stranded in the real home —
  e.g. deployed by a pre-fix release that trusted the dist stash — is replaced with the release
  instead of reused, because started it would relocate to `~/.chimaera-dev` and the manifest poll
  would time out forever.
- **Never force-kill a remote daemon.** `stop_remote` is SIGTERM-only (a daemon that won't die may
  hold sessions that mustn't be torn out — it errors honestly). `TunnelPhaseError` is
  downcast-distinguished so the app retries *only* tunnel-phase failures on a fresh port (re-running
  connect on an auth failure would re-prompt 2FA). Child control-plane output is collected
  concurrently under 8 MiB stdout / 1 MiB stderr and wall-clock limits; overflow or timeout kills
  and reaps the process. Fetched daemons are cached per triple-and-version.
- **Tunnel teardown cannot hold the app hostage.** Tunnel objects are removed from shared maps
  before any process/network wait, so one dead host cannot block health checks or commands for
  another. Child reaping gets a two-second ceiling; ControlMaster forward cancellation is
  non-interactive (`BatchMode`) with a ten-second outer deadline. Native liveness transitions carry
  a plain-language reason into a compact, non-blocking reconnect status; only an actual reconnect
  failure becomes a modal with Retry. A 401 in a native remote window follows that same scoped SSH
  recovery instead of showing the browser-only "paste a fresh URL" page.
- **A daemon build change is a navigation boundary.** A reconnect reuses its local forward only
  while the daemon source build still matches the app. Replacing the remote daemon gets a fresh
  loopback port, which makes every already-open window re-home onto the new entry bundle instead of
  asking the new server for hashed JavaScript chunks from the previous release. Connected events
  also carry the build as a second guard for same-origin transitions, while the entry document
  carries its own build stamp so the first asynchronous health poll cannot race the handoff. A
  window with unsaved edits or memory-only chat input holds the navigation behind one visible
  notice instead of looping reload prompts or silently discarding local state.

## Dev builds — the isolated dev daemon on a host

- **What & when.** Test THIS checkout's daemon against a real host without touching the daemon
  real users (or your other self) depend on. **Dev is dev, no toggle**: a dev build (the
  never-release-stamped `0.0.1` sentinel, `chimaera_core::is_dev_build`) *always* runs against a
  parallel `~/.chimaera-dev` — on the host (`RemoteHome::current()`) AND locally (a dev build
  with no `CHIMAERA_HOME` defaults its own state to `~/.chimaera-dev`). A release always targets
  `~/.chimaera`. Neither can reach the other's home.
- **How it's used.** Nothing to opt into: run a dev build (`just app-dev-isolated`, or the bare
  CLI) and `connect <host>` / `status <host>` operate on the dev homes; every host row in a dev
  app wears the amber `dev` pill. See the [develop skill](../../.claude/skills/develop/SKILL.md).
- **Where it lives.** `chimaera-remote/src/lib.rs` (`RemoteHome::current` — every remote
  path/command derives from it), `chimaera-core::is_dev_build` + `state_home` (the local
  default).
- **Key behaviors.**
  - **Total scoping.** The probed manifest (`~/.chimaera-dev/data/manifest.json` —
    `CHIMAERA_HOME` relocates the data dir), the installed binary (`~/.chimaera-dev/bin/`), the
    started daemon (`CHIMAERA_HOME=$HOME/.chimaera-dev` env prefix — `chimaera serve` stays a
    literal string), and the reuse/update decision all key off `RemoteHome`. The real daemon is
    never probed, stopped, or replaced.
  - **Never a release binary.** A dev connect deploys your build only: explicit `--binary`, else
    the `just dist` stash (also found at the real `~/.chimaera/dist` when the client runs
    isolated), else a hard error. Fresh starts always redeploy so a stale dev binary can't
    impersonate the build under test. Symmetrically, a dev app never offers release updates
    (`check_app_update` returns none) — an "update" would swap the build under test.
  - **No per-host or per-connect selector exists.** Dev-ness is the build's property, so
    auto-reconnect, window restore, and row clicks land on the same daemon by construction — a
    dev tunnel can never silently heal into the real daemon (this used to be a persisted
    `HostEntry.dev` flag + add-form toggle; leftover `"dev"` keys in hosts.json parse and are
    ignored).

## Remote host management (native app)

- **What & when.** From the home screen: browse a connected host's workspaces, and control its daemon.
- **How it's used.** Connected host rows offer `end sessions` (kill everything on the host; the daemon
  + tunnel stay up), `disconnect` (tunnel down; sessions + daemon keep running), `shut down` (end
  sessions *and* stop the daemon, then drop the tunnel — the real off switch), and forget (`×`). An
  outdated remote daemon offers an inline "update" that reconnects with `updateDaemon=true`.
- **Where it lives.** `crates/chimaera-app/src/shell/commands.rs` (`end_host_sessions`,
  `disconnect_host`, `shutdown_host`, `remote_workspaces`); daemon side `DELETE /api/v1/sessions` and
  `POST /api/v1/shutdown` through the tunnel.

## Reauth overlay

- **What & when.** A blocking overlay when the daemon rejects a window's token (daemon restart / token
  expiry) — nothing behind it is trustworthy until re-auth.
- **Where it lives.** `web-ui/src/lib/workspace/ReauthOverlay.svelte` (`refreshTokenFromHash`, probes
  `GET /api/v1/health`, then a clean `location.reload()` on success). The token normally survives a
  graceful restart via the [handoff](lifecycle-and-persistence.md).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why connect works this way
_Captured 2026-07-09 — drafted from DESIGN.md + code, confirmed live with the maintainer._

- **Problem it solves.** The no-root, single-static-binary, ssh-only deployment *is* the moat —
  stood up like code-server (claude + chimaerad user-side, authenticate once).
- **Deliberate (confirmed).** Reuse the user's own ssh client, never reimplement it; never
  force-kill a remote daemon (SIGTERM-only — it may hold sessions); HTTP-probe liveness, not TCP
  (survives laptop sleep); TOFU host keys for a tty-less app. Replacing a running daemon (possibly a
  colleague's, on a shared node) happens **only when it's provably idle, or explicitly forced** —
  this should stay in place. No E2E relay service (free-ride ssh).
- **Core vs addition.** The no-root ssh deployment is **core**; the exact policies above are
  deliberate and should hold, but like all additions can improve.
- **Do not change:** SIGTERM-only remote stop; resolve-the-binary-before-stopping-any-daemon.

### Dev builds — why it exists
_Captured 2026-07-09 (from the maintainer, in-session)._

- **Problem it solves:** "This is just for local development, not a new feature" — developer
  tooling so a checkout's build can be tested against a real host without endangering the real
  daemon. Not user-facing capability (and gated out of release builds accordingly).
- **How settled it is:** the *why* is settled, and so is **no togglability** (maintainer,
  same-day follow-up: "when you run a dev version it will always be the .chimaera-dev on both
  ends" — the per-host flag/toggle was removed for exactly this). The mechanism
  (`~/.chimaera-dev` layout, one dev home per host, the amber styling) is how it works *for
  now*, free to change.
- **Deliberate (confirmed):** the **dev-builds-only gate** (`is_dev_build`, the `0.0.1`
  sentinel — production clients must never offer or perform dev connects), and **never deploy a
  release binary as "dev"** (failing loudly without a local build beats silently testing the
  wrong code).
- **Do not change:** the isolation (a dev connect must never read, stop, or replace the real
  `~/.chimaera` daemon) and the gate above. Everything else here is an **addition** — improvable
  freely.

### Starting on any POSIX remote — why it works this way
_Captured 2026-07-15 (from the maintainer, in-session)._

- **Problem it solves.** "Runs on the host that owns the work — laptop, dev server, or HPC node"
  only held on GNU/Linux: the remote start line needed util-linux `setsid`/`nohup`, absent on
  macOS/BSD and on minimal Linux containers. The daemon binary was already portable (the same one
  that runs locally on macOS); only the launch incantation wasn't. Now `connect` can bring a daemon
  up on **any POSIX remote**.
- **How settled it is (core bet).** That a daemon **starts on any POSIX remote** is a **promise to
  keep** — host-tool independence in the remote-start path is load-bearing and must not silently
  regress to Linux-only again.
- **Deliberately open / where it may go (addition).** *How* it detaches — `chimaera serve
  --daemonize` forking + `setsid(2)` in-process before the tokio runtime — is the mechanism *for
  now*, not sacred; a future change may detach differently as long as the portability promise holds.
  And macOS/BSD remotes are **"don't gratuitously block them," not first-class**: the primary remote
  is still Linux/HPC login nodes, and no Intel-mac/BSD daemon *assets* are built yet (`release_triple`
  → `None`; connect asks for `--binary` / `just dist` there).
- **Do not change:** the **never-regress-a-provisioned-host** guarantee — the start line keeps the
  proven `setsid nohup … & disown` **fallback** (reached only when a pre-`--daemonize` binary, which
  by definition sits on a Linux host, rejects the flag) as cheap belt-and-suspenders. The portability
  promise above is the core bet; the fork+setsid mechanism and the remote-OS scope are additions,
  improvable freely.
