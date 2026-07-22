# Chimaera — field notes & verified-component log

> A dated running log of live-verification findings and field deployments, moved
> out of DESIGN.md to keep it a lean spine. Historical record: where it conflicts
> with the current code, the code wins.

## Verified component notes (2026-07-06)

Crate-level verification sweep completed; all six architecture bets confirmed. Locked
component decisions:

- **mimalloc as the daemon's `#[global_allocator]`, unconditionally.** musl's mallocng
  allocator has a confirmed 7–30x multithreaded penalty that hits tokio/axum and terminal
  churn, not just data paging; static-linking mimalloc into musl is a solved pattern.
- **arrow-rs `parquet` + `csv` crates, not polars**, for the paging service (see previews).
- **alacritty_terminal (0.26, actively maintained) as the headless server-side grid**;
  attach/resize via escape-sequence snapshot re-emission, never serialized grid state (see
  transport).
- **portable-pty is acceptable but single-maintainer** (wezterm's release cadence has slowed);
  `pty-process` (async-native, Unix-only — fine, the daemon is Linux-only) is the named
  fallback. Keep the PTY layer behind a small trait either way.
- **cargo-zigbuild for musl cross-compilation** (`cross` as documented fallback). Keep TLS out
  of the daemon's dependency tree entirely (localhost-over-SSH needs none); if a dependency
  drags in rustls, force the `ring` backend, not `aws-lc-sys`.
- **System ssh for tunnels: confirmed correct** (inherits ControlMaster/ProxyJump/Duo).
  Documented limitation: Windows' built-in OpenSSH lacks ControlMaster, so tunnel startup
  pays full handshake+2FA cost there. russh only becomes interesting if a no-external-binary
  Windows client is ever required.

## Verified component notes (2026-07-07/08): child-marked claude persists no transcript — scrub launcher env

Found during live verification of session resurrection, initially misdiagnosed as a
claude 2.1.204 regression, then bisected to its true cause: **claude suppresses
interactive transcript persistence when its environment marks it as a child of another
Claude Code session** (`CLAUDE_CODE_SESSION_ID` / `CLAUDE_CODE_CHILD_SESSION`; both
bisected live against 2.1.204 — same shell, same cwd: markers present → no transcript
ever, markers removed → transcript within 2s of the first prompt). Hooks still *report*
a `transcript_path`; the file never materializes, and `claude --resume <id>` dies with
"No conversation found".

Why chimaera hits this: a daemon started **from inside a claude session** — dev loops
do this constantly ("restart the daemon" typed to an agent, agent-driven verification,
a linked terminal running `chimaera serve`) — inherits those markers and passes them to
every session it spawns, so every agent under it silently loses `--resume`, and even a
`claude` typed by hand into a chimaera shell goes transcript-silent. A daemon started
by the app or a plain terminal is unaffected (which is why normal usage never sees it).

Fixes, both kept even though the root cause is environmental:
- **Launcher-context scrub**: PTY spawn gained `env_remove`; the daemon strips the
  CLAUDE* marker family (`CLAUDECODE`, `CLAUDE_CODE_*`, `CLAUDE_AGENT_*`,
  `CLAUDE_EFFORT`, `AI_AGENT`) from every session it spawns — none of it can describe
  a chimaera session truthfully, and anything the user set in their own profile comes
  back through the login-shell wrap.
- **A resume id is a claim, not a promise.** Everything that mints one — the session
  ledger's boot resurrection, `retire()` into Recents — verifies the transcript exists
  on disk first (hook-recorded path, or the conventional store location for the cwd).
  No transcript → resurrection respawns a fresh TUI carrying the old display title,
  and Recents rows omit `resume` (the row honestly starts fresh). Defense in depth:
  transcripts also vanish via claude's own `cleanupPeriodDays`, old contaminated
  daemons, and whatever comes next.

## Decisions log addendum (2026-07-07): stateful restarts + update surface — SHIPPED

The update-safety chain, three layers, each independently useful:
- **Session ledger + resurrection** (`sessions.json`, reconciled from live state ≤2s
  behind truth, flushed on graceful shutdown): on boot, shells respawn at their last
  polled cwd and claude conversations respawn `--resume` (transcript-verified, above) —
  all **under their original session ids**, which is the single property that lets every
  persisted layout tab, linked-terminal edge, and window rebind with zero client
  migration. Non-resumable agents retire into Recents (also fixes: agents that died
  while the daemon was down used to vanish). `daemon.restoreSessions` opts out;
  opting out still retires conversations into Recents.
- **Restart handoff** (`handoff.json`, written by every graceful stop, consumed once
  within 120s): the successor daemon rebinds the same port with the same token, so ssh
  forwards stay valid and every client — app window or plain browser tab — heals with a
  plain WebSocket reconnect. No re-home, no re-auth. Crashes never leave one, so
  unplanned restarts keep fresh credentials.
- **Window registry** (`windows.json`, app shell): the open window set (host, workspace,
  logical geometry, stable per-window id) is persisted and reopened on launch; the
  stable id rides the window URL as `win=` and seeds the SPA's view-state key, so a
  reopened window IS its predecessor. Closing a window forgets it (macOS convention);
  quitting keeps the set.

Update awareness rides on top: the daemon checks GitHub a few times a day (bounded curl,
off for dev builds, `update.autoCheck` opt-out) and pushes an `update` frame on
`/ws/events`; the app shell separately watches the signed-app updater and exposes its
build id for skew detection against `/health`'s. One toast per window merges the three
signals into the single offer a click can act on there — full app+daemon chain (intent
file carries consent across the relaunch), daemon-only restart, or a "new release
exists" notice in browsers. The toast cannot over-promise resurrection by construction:
the UI is embedded in the daemon, so a daemon too old to have the ledger serves a UI too
old to have the toast. Home screen's version mark now says `daemon dev·<ref>` for dev
builds instead of posing as an ordinary `v0.0.1` (field confusion: an app reinstall
attached to a still-running dev daemon and nothing on screen said so).

## Field notes: dev binary stranded in the real home bricked release connect (2026-07-09)

The day after `connect --dev` shipped (dev-is-dev on both ends: a `0.0.1` build defaults its
state to `~/.chimaera-dev`), the RELEASE connect to the cluster broke: "daemon did not start
within 15s", with daemons piling up on the login node. Chain of causes, and the invariants
they forced:

- **The real home's binary resolution still trusted the `just dist` stash.** The release
  app's update path deployed `~/.chimaera/dist/chimaera-x86_64-linux-musl` — a `0.0.1` dev
  build — into `~/.chimaera/bin/`. Started there with no `CHIMAERA_HOME`, it relocated its
  state to `~/.chimaera-dev` and wrote its manifest THERE, while connect polled
  `~/.chimaera/manifest.json` forever. Invariant: **the real home runs release binaries
  only** — the stash feeds dev connects; `--binary` is the explicit override.
- **"Executable" was the only reuse check on a fresh start**, so once poisoned, the host
  stayed poisoned: every connect reused the stranded dev binary. Invariant: a fresh start
  **version-probes the installed binary** and replaces the `0.0.1` sentinel in the real home.
- **The daemon happily double-started over one state dir.** Each failed-connect retry ran
  another `chimaera serve`, and each one respawned the SAME ledger sessions — triplicate
  claude processes on a shared login node. Invariant: **one daemon per state dir** — `serve`
  refuses to start when the manifest's daemon is provably alive (live pid + an HTTP answer
  on its port; a crash leftover or recycled pid must not block startup).

## Field notes: laptop-sleep reconnect (2026-07-08)

The first real close-the-laptop-overnight cycle against the cluster broke reconnect in
three compounding ways. Findings and the invariants they forced:

- **A dead ssh forward keeps accepting.** After wake, the `-N -L` child's local listener
  still accepts TCP while the connection behind it is gone — so every `TcpStream::connect`
  liveness probe said "up", `connect_host` concluded "already connected" and healed
  nothing, while the window's WebSocket retried a black hole forever. Invariant: **tunnel
  liveness is an HTTP response end-to-end** (`http_alive`: any HTTP status on the loopback
  port within 2s — even a 401 proves the daemon answered). A bare TCP accept is never
  proof of anything. Corollary: chimaera ssh now carries `ServerAliveInterval=15`/
  `CountMax=3` + `ConnectTimeout=15`, so dead masters and forwards exit in ~45s instead
  of holding their listeners (and their lies) for hours.
- **Askpass prompts are state, not just an event.** Startup window restore begins
  connecting before any webview exists; a Duo prompt emitted then reached zero listeners
  and vanished — ssh waited out its 180s timeout with the host stuck "connecting" and
  nothing on screen to answer (the "blue bar, no prompts" bug). Invariant: pending
  prompts are held in the shell (`list_askpass`) and eligible windows fetch them on mount;
  the emit is just the fast path. Each prompt now carries the ssh child's host alias, and the
  shell targets events plus authorizes list/answer commands from its immutable window scope, so
  remote windows can reach only their own host while home remains the startup/first-connect
  fallback. Restore registers that home before starting remote ssh when the persisted local set
  contains only workspaces; otherwise the stricter scope would leave early password/2FA prompts
  with no eligible window. Answering in one matching window targets `ssh-askpass-done` to the same
  scope.
- **Connects coalesce per alias.** A drop used to fan out: every window's reconnect plus
  the home screen plus startup restore each called `connect_host`, the first won and the
  rest bounced with "a connection attempt is already running" (or worse, queued more 2FA
  prompts). Now one flight owns the ssh per alias and every concurrent caller awaits its
  outcome — one Duo push per host, ever. The fresh-port retry after a reused-port failure
  is gated on a typed `TunnelPhaseError`, because re-running the whole connect on an auth
  cancel re-prompted 2FA.
- **Remote windows come back with the first successful connect**, not just the next
  launch: reopening persisted windows rides `connect` itself (dedup'd on stable window
  id), so a host that was unreachable at launch restores its windows the moment a
  home-screen click or auto-reconnect lands.
- **Sessions snapshots wait for resurrection.** The daemon serves while the ledger is
  still respawning, and a window's first `/ws/events` snapshot taken mid-restore read as
  "those sessions died" — the client pruned their tabs out of the restored layout.
  `GET /sessions` fed the same half-truth to the remote update decision ("0 live
  sessions" → safe to replace the daemon → kills the sessions being resurrected). Both
  now gate on restore completion (bounded at 15s so a wedged respawn can't blank the UI).

## Field notes: first cluster deployment (2026-07-06)

M0 `connect` validated end-to-end against a production HPC cluster (CentOS 7.9 login
nodes, Duo 2FA, ControlMaster-only non-interactive ssh). Findings:

- **The static musl binary ran unmodified on CentOS 7.9 (glibc 2.17)** — a full glibc
  generation older than the design's RHEL 8 worst case. Deployment story confirmed.
- **Shell-parse hang (fixed):** `mkdir ... && nohup daemon ... & disown` backgrounds the
  *whole* `&&` list — the daemon runs as the foreground child of a subshell whose
  stdout/stderr are the ssh channel, so sshd never closes the session and `connect` hangs
  forever. Fix: `;` before `setsid nohup ... < /dev/null &`. Only reproducible on real infra.
- **ControlMaster mux forwards (fixed):** with a live master, `ssh -N -L` registers the
  forward with the master and exits 0 — the master owns the forward. The tunnel lifecycle
  must treat zero-exit as mux-delegation (hold, then tear down via `ssh -O cancel -L ...`),
  not as failure.
- **ControlMaster pins the login node:** all multiplexed sessions ride one TCP connection to
  one node (the ssh config pointed at a round-robin login alias, the master landed on one
  specific node, and every subsequent command hit that same node). Round-robin manifest
  discovery matters only *across* master restarts — less scary than the design feared, but
  still needed.
- **`claude` is not in the non-interactive ssh PATH** on the login node even for a user who
  runs it daily — M2's session spawning must resolve the agent binary via a login shell or
  explicit config, never PATH assumptions.
- The cluster's login nodes run Duo + `gssapi-with-mic,password` only (no pubkeys): riding the
  user's ControlMaster isn't just convenient, it's the *only* non-interactive path — the
  design's shell-out-to-system-ssh decision is load-bearing here.
- **On containers as a fallback:** Docker never exists on HPC (no root); Apptainer/Singularity
  does, but adds per-site bind-mount/startup variance. The static binary already solves the
  problem class containers address (old glibc, missing deps) — and the bugs we actually hit
  were ssh/shell semantics that would reproduce identically inside a container. Keep an
  Apptainer recipe as a documented fallback for pathological hosts, not as the plan.
