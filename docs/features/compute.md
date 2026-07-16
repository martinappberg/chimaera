# Compute (Slurm awareness)

Scheduler detection + a slim **indicator**: when the host a daemon runs on has Slurm, the
rail's bottom bar shows a passive compute chip — a queue glyph plus the user's
queued/running job count (tooltip carries the words). Deliberately NOT a control surface:
no popover, no queue browsing from the rail (maintainer call, 2026-07-15 — that arrives
with the agent dashboard; launching onto compute nodes is the home screen's Mode 2 flow).
On a laptop (no scheduler) the whole surface is invisible. First slice of the M5 HPC
layer's *placement axis*; the deep design (Mode 1 login-node job control, Mode 2
compute-node sessions) lives in the
[architecture guide](../agent-guides/architecture.md#environment-prelude-compute-node-sessions).

**Where it lives (shared):** daemon `crates/chimaera-server/src/compute.rs` (detection +
snapshot service + route). UI `web-ui/src/lib/workspace/compute.ts` (store/poller) and the
indicator chip in `web-ui/src/App.svelte` (the `.daemon` bottom bar, beside the git chip).
Wire: `GET /api/v1/compute` (bearer-authed; `?refresh=true` re-detects).

## Detection & the snapshot

- **What & when.** "Is this an HPC, and what do I have running?" — answered by the daemon
  about its *own* host (a remote daemon detects its own cluster; nothing is probed over ssh
  at connect time). Detection = `command -v sbatch` through the user's login shell (same
  reasoning as git resolution: tools often arrive via a profile-managed PATH), cached for
  the daemon's lifetime; `?refresh=true` re-detects (e.g. after a `module load slurm`).
- **How it's used.** The UI fetches `GET /api/v1/compute` at boot; if `scheduler` is
  `"none"` it never asks again that page-load. On a Slurm host it refetches every 60s while
  the tab is visible. The chip appears only when `scheduler == "slurm"` and is read-only
  orientation; the jobs/partitions detail in the snapshot feeds the coming launch dialog
  and dashboard surfaces.
- **Where it lives.** `ComputeService` (detection + 30s snapshot cache, single-flight —
  concurrent requests coalesce on one refresh); `parse_squeue`/`parse_sinfo` are pure and
  unit-tested against Sherlock-shaped fixtures.
- **Key behaviors / constraints.**
  - **Resource discipline is the design** (sysadmins ban tools that hammer `squeue`):
    every child process gets a 5s kill-on-timeout and an output cap; jobs/partitions are
    capped at 50 rows with a `truncated` flag; snapshots cache ~30s; nothing persists.
  - **Format strings, not `--json`**: `squeue -u <user> --noheader -o "%i|%j|%P|%T|%L|%N"`
    and `sinfo --noheader -o "%P|%a|%D"` work on the old Slurm versions real clusters run,
    and bound the output by construction. `-u <user>` rather than `--me` (older Slurm).
  - **Degrade, never 500**: a wedged controller or parse noise never errors. A failed
    `squeue` CALL (timeout/exit) is distinguished from an empty queue: the snapshot
    carries the previous jobs forward tagged `degraded` — one slow controller round must
    not blank every card (or mint false "ended" tombstones). Unparseable lines are
    skipped (Slurm warning banners precede output on some clusters).
  - The default partition carries sinfo's `*` suffix → surfaced as `"default": true`;
    duplicate (partition, avail) rows are merged, "up" wins.
  - **Test knob:** `CHIMAERA_SLURM_BINDIR` points at a directory of stand-in
    `sbatch`/`squeue`/`sinfo` so the whole surface can be driven live without a cluster
    (the `CHIMAERA_RELEASES_API` pattern).
- **Status: partial (by design).** Detection + the strip, plus the Mode 2 core below.
  Still to come on this seam: job↔session linking and the login-node Slurm skill (Mode 1).

## Mode 2 — chimaera sessions AS Slurm jobs

- **What & when.** Launch a whole chimaera daemon *inside* an allocation and connect to it
  as a first-class entity — "a workspace you connect to, with x compute and hours left."
  The login daemon is the control plane: `POST/GET /api/v1/compute/sessions` +
  `DELETE /api/v1/compute/sessions/{job_id}` (bearer-authed). Launch renders an sbatch
  script (directives charset-validated; body = the environment prelude verbatim + an egress
  probe to `caps.json`; `exec chimaera serve` with an isolated `CHIMAERA_HOME` per jobid on
  the shared FS — same binary, no redeploy). The registry is stateless: `chimaera-`named
  `squeue` rows ⋈ per-job manifests ⋈ launch records, rebuilt on every list — cards survive
  laptop closes and vanish at walltime (login daemon = forever, compute daemon = until
  walltime; the snapshot's `self` block carries the countdown the window's bottom bar wears).
- **How it's used** (placement per the maintainer's first live test, 2026-07-15): the
  **host's own window** (the login daemon's home page) is the compute hub — cards (state
  dot, node, resources pill, time-left, open/cancel) + the "new compute session…" dialog
  (partition picker fed by live partitions, time/cpus/mem/gres, startup commands = the
  launch prelude), self-refreshing (30s visible / 10s while anything is PENDING / instant
  after launch-cancel; management calls go straight to that daemon's routes — only *open*
  crosses the native bridge for the tunnel). The **local home screen** shows just a slim
  "N compute sessions" indicator per connected host. A **compute-node window** identifies
  itself everywhere: title + host label become `{alias} › {node}`, its home page opens
  with an **allocation banner** (node, partition, job id, resources, live ticking
  walltime countdown — warn-toned under ten minutes), and an accent-washed
  **allocation strip** above the rail's bottom bar carries the same countdown +
  `cpus · mem · gres` inside a workspace (daemon-truth via the snapshot's `self` block,
  so windows opened later on the same node inherit it). A fresh launch never flashes
  "ended": launch/cancel invalidate the snapshot cache, and an orphaned launch record
  younger than 120s renders as a PENDING submitted-card (squeue lag), leading the list. Launch **seeds the job daemon's workspace registry
  with the host's whole list** (shared-FS roots are equally valid on the node), so the
  compute window opens on the same ready-to-open workspaces as the login window. A job
  that leaves the queue un-cancelled (walltime, failure) stays visible as a dismissable
  **"ended" tombstone card** built from its launch record (explicit cancels clean up
  silently — the user watched those; tombstones age out after 48h). **Nothing pops in
  from nowhere**: the host page holds the section's seat with a breathing "checking this
  host for a scheduler…" probe line until the first fetch answers (the refresh glyph
  spins during any later fetch), and the local home's indicator reads "checking for
  compute…" → "slurm cluster" (even at zero sessions — *this host has compute nodes* is
  the load-bearing fact) → the live session count, refreshed once a minute per connected
  host while the page is visible. CLI parity + verification harness:
  `chimaera compute list|launch|connect|cancel <host>`.
- **The tunnel ladder** (per connect, honest about defeat): **B1** laptop-ssh end-to-end to
  the node (pam_slurm_adopt clusters) → **B2 chained** — a login-node-resident
  `ssh -N -L` relay to the node's loopback, running as the remote command of the same
  laptop ssh that forwards to it (lifetimes coupled; the path on hostbased-only clusters
  like Sherlock — verified live) → **A** direct login→node forward, only for jobs launched
  `--bind-routable` (opt-in 0.0.0.0, token-gated) → else "not supported on this cluster",
  and the job keeps running for login-node use. Every rung's arbiter is `tunnel_proven`:
  a **bearer-authed 200 through our own forward** (identity, not bare liveness — a stale
  relay or foreign tenant of a shared login-node port can answer a plain probe), from an
  ssh child still alive afterwards; the chained rung's login-side relay port is **always
  randomized** (the daemon's own port number is exactly where a previous connect's relay
  sits — both found live as "sometimes works").
- **Key constraints.** Tokens/ports never reach the home-screen JS (the app shell scrubs
  them and holds tunnels in Rust, keyed `"{alias}#job{job_id}"`); launch-restore skips
  compute windows (the card is the reconnect path); cluster-vintage tooling is assumed
  hostile-old (login-node OpenSSH without `accept-new`, `%C` needing `%%` escaping inside
  ProxyCommand — both found live and handled). **A job window's status identity is its
  composite key** (`{alias}#job{id}`) end-to-end — shell health monitor, `host-status`
  events, the SPA's listener and scope report: matching on the bare login alias made every
  login-tunnel blip re-home job windows onto the login daemon (found live: "opening the
  session just opens Sherlock"), and a compute daemon's home page suppresses the launch
  hub via the snapshot's `self` block (it detects Slurm on its node too — without the gate
  it poses as the login host).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Slurm awareness — why it exists
_Captured 2026-07-15 (from the maintainer; drafted from his design-session words, confirmed by him)._

- **Problem it solves:** the cluster should be visible in the workbench — detect Slurm,
  show your queue. First step of the placement axis: Mode 1 (login-node job control via an
  agent skill) and Mode 2 (own the full session on a compute node — chosen as second-hop
  daemon over srun-prefixing for isolation and ownership) build on this seam, toward the
  premium synced-workspace vision.
- **How settled (intent grade: the invariants are core to this feature; the rest is
  addition):** promises — cluster behavior is **probed per cluster, never assumed**
  ("not all environments are the same"; "no shame in saying not supported"), and the
  compute surface stays invisible/quiet off-cluster. The chip/popover design, polling
  cadence, caps, and wire shape are mechanics, improvable.
- **Non-obvious decision / deliberately left out:** the staging is deliberate — detection
  first, then job↔session linking, the Mode 1 skill, and Mode 2 compute-node sessions.
  "Not supported on this cluster" is an acceptable, honest end state where the tunnel
  ladder finds no path.
- **Do not change:** the probe-and-degrade-honestly posture, and hidden-off-cluster.
  Everything else is an addition, open to improvement.

### Addendum — loopback stays the default; routable is per-launch opt-in
_Captured 2026-07-16 (maintainer decision, after raising the question himself)._

- The maintainer asked whether `--bind-routable` should be the standard ("it is through
  token anyway … and it is our own compute node"), weighed the trade-off, and **decided
  against**: compute nodes are routinely shared, the chained-B rung already gives every
  ssh-reachable cluster a loopback path, and a 0.0.0.0 bind makes the token the only wall
  between the daemon and every co-tenant of the node. Routable stays an explicit
  per-launch flag (dialog checkbox with its exposure warning, CLI flag) for clusters whose
  ladder finds no ssh path; per-host auto-routable memory was deliberately deferred until a
  real cluster defeats rung B.

### Addendum — the chip is an indicator, not a controller
_Captured 2026-07-15 (maintainer, direct feedback after first live use)._

- The rail chip stays a **passive indicator** on login-node workspaces ("there is no
  reason from the login-node workspace right now to show slurms etc. from the left-pane,
  that should just be an indicator"). The queue-browsing functionality is good and will be
  **extended later into the agent dashboard**, not grown in the rail.
- The real destination is **Mode 2 from the native window launcher** — especially when
  browsing a remote host: submit a chimaera session as an HPC job, displayed as its own
  first-class connectable entity ("like a workspace you connect to, which then has a
  certain x compute and hours left").
