# Clusters (HPC + Slurm)

On a cluster — a host whose login shell reaches a batch scheduler — **nothing of
Chimaera's keeps running on the login node**. Each workspace runs as its own Slurm job:
the app (or the CLI) starts, lists, opens and stops it with short ssh commands, and the
job's own `chimaera serve` is the workbench for that workspace until its time runs out.
The login node only ever sees those commands and the user's interactive terminal.
Regular remotes (dev servers, lab machines, cloud VMs) are unaffected. Design and the
maintainer's decisions: [docs/hpc-portal-plan.md](../hpc-portal-plan.md).

**Where it lives:** detection and the cluster outcome in `crates/chimaera-remote/src/lib.rs`
(`sh_scheduler`, `resolve_daemon`, `ClusterHost`, `connect_compute_node`); every cluster
command in `crates/chimaera-remote/src/cluster.rs`; the shared vocabulary (parsers, launch
spec, job script, the cluster folder's records) in `crates/chimaera-core/src/{slurm,cluster}.rs`;
the native app's commands, job windows, handoff and notifications in
`crates/chimaera-app/src/shell/cluster.rs`; the CLI in `crates/chimaera/src/compute.rs`;
the UI's cluster page and start sheet under `web-ui/src/lib/workspace/`; the job daemon's
side (its own allocation, agent context, startup commands, the lease) in
`crates/chimaera-server/src/{compute,lifecycle,environment}.rs`.

## Detection and cluster mode

- **What.** The connect probe (one ssh exec) also walks the login shell's `PATH` for
  `sbatch`/`squeue`/`scancel`/`sinfo` (Slurm), `qsub`/`qstat` (PBS) or `bsub`/`bjobs`
  (LSF) — a walk, never `command -v`, because clusters wrap these tools in profile
  functions. The `PATH` comes from the user's login shell when it takes `-lc`, else
  `sh -l` (tcsh refuses `-lc`), so profile-managed scheduler paths count.
- **What it does.** A cluster without the override never gets a daemon: `connect`
  answers a typed `ClusterHost` (with any daemon an earlier connect left on the login
  node), and nothing is started, updated, attached to, or routed to another login node.
  Every automatic path inherits this because they all connect through the same flight:
  launch-time window restore, a window's reconnect after sleep or a 401, a job window
  healing its connection. Saved windows onto an old login-node daemon are forgotten.
- **The override.** "Run Chimaera on the login node" (per host, `hosts.json`
  `login_serve`, off unless set; the CLI's `--login-node`) — for clusters whose admins
  allow it. Turning it on in the app shows one warning and needs a confirm. An older
  build that rewrites `hosts.json` drops the field, which turns the override off.
- **The CLI.** `chimaera connect <cluster>` explains cluster mode and exits;
  `chimaera status <cluster>` says it is a cluster.

## The cluster page

- **What & how.** The local home shows a cluster row ("Slurm cluster" and a one-line
  summary). Clicking it connects (nothing starts) and opens the cluster page in place:
  the cluster's workspaces, each **running** (node · resources · time left), **starting**,
  **waiting for a node** (Slurm's start estimate and its reason when not plain priority),
  or **stopped** (why: time limit, cancelled, failed, preempted, its node failed, out of
  memory — asked of `sacct` once and kept); a Terminal button (a local window running
  `ssh <host>` over the app's ControlMaster); add / remove a workspace; a read-only file
  peek; startup commands and rules for agents; and one quiet count of the user's other
  jobs.
- **One job per workspace.** A workspace is either running or not; the job is a detail
  of it. "Session" keeps its meaning (chats and terminals inside a workspace).
- **Wire.** Tauri commands `cluster_*` (see `web-ui/src/lib/net/native.ts`); events
  `cluster-changed` and `host-status` (`cluster`, and `ended` for a job window whose job
  left the queue). Ports and tokens never reach a page.

## What the app learns from the cluster

One exec when the start sheet opens, cached a day: `sinfo` (partitions, default, limits,
GPU nodes), `sacctmgr` associations and the default account, `scontrol show partition`
(`Allow*`/`Deny*` lists and `PreemptMode`), the user's groups, and the Slurm version
(`--gpus` vs `--gres=gpu:N`). The sheet offers only partitions the user can submit to. What
no command says is learned from the cluster's own refusals and remembered in the cluster
folder: partitions that take only interactive jobs, and required account/QOS/constraint.
Nothing site-specific is ever coded.

## Starting, stopping, continuing

- **Start.** `sbatch --parsable --no-requeue --time …` with the job script written to the
  workspace's folder on the cluster (the script and every file ride the ssh exec's stdin,
  never argv). Partitions that take only interactive jobs get an `srun` held in the
  foreground by the app's own ssh connection (`ssh -tt`): it stops when the app
  disconnects, and the UI says so. Saved setups and the last setup per workspace are
  remembered.
- **The job script** points the job daemon's data dir at the workspace's folder (so its
  chats and history move from job to job), its runtime dir at node-local `/tmp`, exports
  the startup-commands and rules files, probes whether the node reaches the internet, and
  `exec`s `chimaera serve --bind-routable` — the daemon is the job; `scancel` and walltime
  stop it with SIGTERM first, so it saves the chats.
- **Reaching it.** The app holds a plain `ssh -L <port>:<node>:<port> <host>` through the
  login node for as long as the job runs and the app is open — nothing extra runs on the
  login node. Fallback: ssh into the node; else "can't reach compute nodes here". Every
  rung is proven by an authenticated 200 through our own forward.
- **Stop.** `scancel`; "Invalid job id" is success; the record says "stopped by you".
- **Continue on a new node.** Queues the next job with the same setup now; once it has a
  node, the old job is stopped; the new job's daemon waits for the old manifest to go (the
  manifest is the workspace's lease), then resumes the chats idle; the window moves to the
  new job. Nothing restarts on its own: a person starts every job.
- **Notifications** (native app): ready, about an hour left, ten minutes left, stopped.
  A background check runs only while a job this app knows of is alive: the queue at most
  once a minute while something waits or starts, every five minutes while things only run.

## The job daemon's side

- **`GET /api/v1/compute`** — the daemon's own view (any daemon): scheduler detection, the
  user's queue snapshot (cached 60 s, single-flight, never a 500; a failed `squeue` carries
  the previous jobs forward as `degraded`), and the `self` block when it runs inside a job
  (the window's countdown and allocation strip). `DELETE /api/v1/compute/self` scancels
  its own job.
- **Agents are told where they are** (claude through the hook carrier, codex through its
  developer instructions): inside job N on node X with these resources until an absolute
  time; use the allocation fully; submit longer or bigger work as separate jobs with an
  explicit `--time`, checking at most once a minute. Then the cluster's rules for agents
  — a file on the cluster the user pointed at and/or their own text — or, without any, a
  short generic set. A daemon on a login node (the override) tells its agents to keep to
  light work there.
- **Startup commands** (cluster default, workspace, this run) reach every shell and agent
  the job daemon spawns, as the outermost prelude scope.
- **The rail chip** stays a passive indicator: the user's queued/running job count.

## Constraints

- **Polite to the scheduler:** at least 60 s between status checks everywhere, only while
  something is visible or a job this app started is alive; one combined exec per refresh;
  files are read instead of asking the scheduler wherever a file answers.
- **Owner-only on a shared filesystem:** the cluster folder is written under `umask 077`;
  manifests carry tokens.
- **Test knob:** `CHIMAERA_SLURM_BINDIR` for the daemon's own detection; the client side is
  unit-tested against canned command output and real local shells (sh, dash, bash, zsh,
  tcsh, fish).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Slurm awareness — why it exists
_Captured 2026-07-15 (from the maintainer; drafted from his design-session words, confirmed by him)._

- **Problem it solves:** the cluster should be visible in the workbench — detect Slurm,
  show your queue. First step of the placement axis toward owning a session on a compute
  node, and toward the premium synced-workspace vision.
- **How settled (intent grade: the invariants are core to this feature; the rest is
  addition):** promises — cluster behavior is **probed per cluster, never assumed**
  ("not all environments are the same"; "no shame in saying not supported"), and the
  compute surface stays invisible/quiet off-cluster. The chip design, polling cadence,
  caps, and wire shape are mechanics, improvable.
- **Do not change:** the probe-and-degrade-honestly posture, and hidden-off-cluster.
  Everything else is an addition, open to improvement.

### Addendum — the chip is an indicator, not a controller
_Captured 2026-07-15 (maintainer, direct feedback after first live use)._

- The rail chip stays a **passive indicator** ("there is no reason from the login-node
  workspace right now to show slurms etc. from the left-pane, that should just be an
  indicator"). Queue browsing belongs elsewhere, not in the rail.

### Addendum — loopback stays the default; routable is per-launch opt-in
_Captured 2026-07-16. **Superseded 2026-09-30** (below)._

- The maintainer decided against a routable default then: compute nodes are shared, and
  a login-node relay gave every ssh-reachable cluster a loopback path.

### Addendum — clusters: nothing on the login node, workspaces as jobs
_Captured 2026-09-30 (maintainer, in-session, after a cluster's admins asked him to stop
running the daemon on their login nodes)._

- **Nothing of ours keeps running on a cluster's login node.** No daemon, no detached
  `srun`, no relay. A per-host, warned override ("Run Chimaera on the login node") exists
  for clusters whose admins allow it.
- **"I reverse my July decision."** Jobs listen on the node's network address by default
  (token-gated) and are reached with a plain `ssh -L` through the login node — the setup
  the admins themselves described; the login-node relay is retired.
- **One job per workspace**, and the UI is workspace-first; "session" keeps meaning chats
  and terminals.
- **Nothing site-specific** — "it needs to be HPC / Slurm specific": no partition names,
  site commands, site paths or hostnames in code, UI, docs or tests; clusters we test on
  are never named in public.
- **A person starts every job:** no automatic restarts, scheduled starts or requeues; a
  notification plus one click is the compliant form.
- **Startup commands and module loads stay** ("they are great and should still be
  there"), and agents must know they are inside a job, its limits, that they can still
  submit longer work as separate jobs, and the cluster's rules for agents.
- **Not settled (additions):** the page layout, notification wording and cadence, the
  file peek, how saved setups are presented.
