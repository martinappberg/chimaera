# Clusters (HPC + Slurm)

On a cluster — a host whose login shell reaches a batch scheduler — **nothing of
Chimaera's keeps running on the login node**. You start Slurm **jobs**, and open
**workspaces** inside them: each job runs `chimaera job-host` on its compute node, which
runs one `chimaera serve` per open workspace. The app (or the CLI) starts, lists, opens and
stops things with short ssh commands; the login node only ever sees those, a read-only
`chimaera browse` that exits at once, and the user's interactive terminal. Regular remotes
(dev servers, lab machines, cloud VMs) are unaffected. Design and the maintainer's
decisions: [docs/hpc-portal-plan.md](../hpc-portal-plan.md).

**Where it lives:** detection and the cluster outcome in `crates/chimaera-remote/src/lib.rs`
(`sh_scheduler`, `resolve_daemon`, `ClusterHost`, `connect_compute_node`); every cluster
command and the job-host client in `crates/chimaera-remote/src/cluster.rs`; the shared
vocabulary (parsers, launch spec, job script, the cluster folder's records, `browse`) in
`crates/chimaera-core/src/{slurm,cluster}.rs`; job-host in
`crates/chimaera-server/src/job_host.rs`; the native app's commands, windows, moves and
notifications in `crates/chimaera-app/src/shell/cluster.rs`; the CLI in
`crates/chimaera/src/compute.rs`; the UI's cluster page, start sheet and folder picker
under `web-ui/src/lib/workspace/`; a workspace chimaera's side (its job, agent context,
startup commands, the lease) in `crates/chimaera-server/src/{compute,lifecycle,environment}.rs`.

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
- **The override.** "Run Chimaera on the login node" (the cluster page's ⋯ menu; per
  host, `hosts.json` `login_serve`, off unless set; the CLI's `--login-node`) — for
  clusters whose admins allow it. Turning it on shows one warning and needs a confirm;
  the host row then connects to a login-node daemon like any remote, and the cluster
  page (jobs) stays one click away. An older build that rewrites `hosts.json` drops the
  field, which turns the override off.
- **Not a cluster.** For a host that only looks like one (Slurm's tools on a
  workstation's `PATH`): "This isn't a cluster…" in the cluster page's ⋯ menu, with a
  confirm (`hosts.json` `not_cluster`; it replaces the override). The host then connects
  like any remote, its row loses the cluster page, and a daemon started there runs with
  `CHIMAERA_NOT_A_CLUSTER=1`, so its agents aren't told they're on a shared login node.
  The row's "it's a cluster" undoes it (disconnects, then connects onto the cluster
  page). `chimaera connect` honors it too.
- **The CLI.** `chimaera connect <cluster>` explains cluster mode and exits;
  `chimaera status <cluster>` says it is a cluster.

## The cluster page

- **What & how.** The local home shows a cluster row ("Slurm cluster" and a one-line
  summary: "1 job running · ends in 5d 22h", "1 job waiting for a node", …). Clicking it
  connects (nothing starts) and opens the cluster page in place: one card per job —
  **running** (node · resources · time left), **starting**, or **waiting for a node**
  (Slurm's start estimate and its reason when not plain priority) — listing the
  workspaces open in it (open · opening · closing, and what their chats are doing);
  ended jobs as one line each (why: time limit, cancelled, failed, preempted, its node
  failed, out of memory — asked of `sacct` once and kept); the workspaces not open
  anywhere; one quiet count of the user's other Slurm jobs. Masthead: Home, Terminal (a
  terminal-only window, "<host> · login node", running `ssh <host>` over the app's
  ControlMaster — never listed as a workspace, never restored, its session ends when the
  window closes), ⋯ (startup commands, rules for agents, refresh partitions, the
  override, not a cluster) and **Start a job**. The app's own binary install on the
  cluster shows on the Home row while it runs and ends with a `done` progress event.
- **Workspaces.** Added with a folder picker (`chimaera browse --dir`: folders only, git
  and already-added marks) — the only browsing outside a workspace window. It types like
  the workspace folder picker: the box filters this folder (names that start with what
  you typed, then containing it, then with its letters in order), or, starting with `/`,
  `~` or `$`, is a path that completes from its folder's listing (one `browse` per folder,
  debounced). ↑↓ pick, ↩ steps in (or goes to the path as typed; a name not among a
  capped listing's first 2000 is tried as a folder here), ⌫ on an empty box goes up, ⌘↩
  adds the folder you're in.
  A workspace is open in at most one job; a job may hold several, or none. **Open** goes
  where it is open. Anywhere else is the user's pick, never a silent default: each
  running job (its node, time left, and what's already open in it, since it shares
  them), "When <job> starts" for each waiting job (added to that job's start list; the
  app opens it through job-host once the job runs, in case the job read its list a
  moment before), and "In a new job…"; with no job, it opens the start sheet with it
  ticked. Paths under the cluster's home folder show as `~/…`. **Move to** another
  running job closes it there (its chimaera saves the chats) and opens it here; its
  window follows.
- **A workspace's window.** It opens straight into its workspace (`ws=` with the
  cluster's id, which its chimaera registered before it listened). Its chimaera knows
  only that one workspace, so the window's way to the rest of the cluster is the cluster
  page: the sidebar's workspace button (and ⌘O) shows it over the workspace, with this
  window's row marked "this window"; Open there, or the back button, returns to it. A
  job window that lands on its home shows the same page. A folder is never opened on a
  job's own chimaera.
- **Fresh state.** The page, Open/Close/Move and the watcher ask each running job's
  job-host what it holds (over the forward the app already has; a read never builds
  one) and lay that over the cluster folder, which can show an open or a close on the
  login node a minute late.
- **Wire.** Tauri commands `cluster_*` (see `web-ui/src/lib/net/native.ts`); events
  `cluster-changed` and `host-status` (`ended` with a reason for a workspace window:
  Slurm's state, `stopped`, `closed`, `workspace-failed`, or `moving`). Ports and tokens
  never reach a page.

## What the app learns from the cluster

One exec when the start sheet opens, cached a day: `sinfo` (partitions, default, limits,
GPU nodes), `sacctmgr` associations and the default account, `scontrol show partition`
(`Allow*`/`Deny*` lists and `PreemptMode`), the user's groups, and the Slurm version
(`--gpus` vs `--gres=gpu:N`). The sheet offers only partitions the user can submit to. What
no command says is learned from the cluster's own refusals and remembered in the cluster
folder: partitions that take only interactive jobs, and required account/QOS/constraint.
Nothing site-specific is ever coded.

## Jobs

- **Start.** The sheet: partition, time, resources, saved setups, which workspaces to
  open when it starts, and this job's startup commands (after the cluster's and each
  workspace's). `sbatch --parsable --no-requeue --time …` with the job folder
  (`j/<id>/`: `job.sh`, `job.json`, `startup.sh`, `agent-rules.md`, `facts.json`) written
  on the cluster — every file rides the ssh exec's stdin, never argv. Partitions that take
  only interactive jobs get an `srun` held in the foreground by the app's own ssh
  connection (`ssh -tt`): it stops when the app disconnects, and the UI says so.
- **The job script** probes whether the node reaches the internet and `exec`s
  `chimaera job-host --job-dir …`: job-host is the job, so walltime and `scancel` stop it
  with SIGTERM first. It writes `host.json` (node, port, token; 0600), keeps
  `workspaces.json` (what it is opening, has open, is closing, or lost) current, opens the
  start list, and serves a token-gated API (`GET /api/v1/job`,
  `POST /api/v1/job/workspaces/{id}/open|close`). Each workspace's chimaera runs over the
  workspace's own data folder (`w/<id>/data`, so its chats move from job to job) with a
  node-local runtime dir; its manifest is the workspace's lease. On SIGTERM job-host
  closes every workspace (25 s each to save its chats) and exits.
- **Reaching it.** The app holds plain `ssh -L <port>:<node>:<port> <host>` forwards
  through the login node — one to job-host, one per open workspace window — for as long
  as the job runs and the app is open. Fallback: ssh into the node; else "can't reach
  compute nodes here". Every rung is proven by an authenticated 200 through our forward.
- **Stop.** `scancel`; "Invalid job id" is success; the record says "stopped by you" only
  when the stop took.
- **Continue in a new job.** Queues the next job with the same setup and workspaces now;
  once it runs, its job-host stops the old job (once, by the Slurm id in its record); each
  workspace's chimaera waits for the old lease to go, then resumes its chats idle. The
  old windows say "moving" and reopen in the new job. Nothing restarts on its own: a
  person starts every job. An attached job can't continue (the app holds it); its window
  says when it ends without offering to (job-host sets `CHIMAERA_JOB_ATTACHED` on its
  workspaces' chimaeras, and their `/compute` `self` block carries `attached`).
- **Notifications** (native app, even when looking elsewhere; windows never open by
  themselves): ready, an hour left, ten minutes left (only for a job longer than the mark,
  worded with the time actually left; the watcher wakes at those marks), stopped, and
  "didn't start" with Slurm's own words for an attached job that ended before it ran. An
  attached job's windows are found by the Slurm id the app saw it run under (its record
  has none), which its record also learns at its end, so accounting can say why. A background check runs only while a job this app knows of is alive: the queue
  at most once a minute while something waits, a starting job's record every 15 s (no
  queue question — "ready" comes within seconds of job-host being up), every five
  minutes while things only run. The cluster page says so, with a way to turn them on,
  when the OS has notifications off while a job is alive.

## A workspace chimaera's side

- **`GET /api/v1/compute`** — the daemon's own view (any daemon): scheduler detection, the
  user's queue snapshot (cached 60 s, single-flight, never a 500; a failed `squeue` carries
  the previous jobs forward as `degraded`), and the `self` block when it runs inside a job
  (the window's countdown and allocation strip). `DELETE /api/v1/compute/self` scancels
  its own job.
- **Agents are told where they are** (claude through the hook carrier, codex through a
  developer note added once its chat opens, which also reaches a reopened chat, so a chat
  continued in a new job learns the new job): inside job N on node X with these resources
  until an absolute time, shared with any other workspaces open in it; the cluster's
  partitions, limits, GPUs and accounts; how to submit longer or bigger work as separate
  jobs (an `sbatch` template, `--dependency`, checking at most once a minute). Then the
  cluster's rules for agents — a file on the cluster the user pointed at and/or their own
  text — or, without any, a short generic set. A daemon on a login node (the override)
  tells its agents to keep to light work there.
- **Startup commands** at three levels — the cluster's and each workspace's are the
  Environment settings' own `env-profiles.json` scopes on the cluster, plus this job's —
  reach every shell and agent a workspace chimaera spawns, as the outermost prelude.
- **The rail chip** stays a passive indicator: the user's queued/running job count.

## Constraints

- **Polite to the scheduler:** at least 60 s between status checks everywhere, only while
  something is visible or a job this app started is alive; one combined exec per refresh;
  files are read instead of asking the scheduler wherever a file answers.
- **Owner-only on a shared filesystem:** the cluster folder is written under `umask 077`;
  manifests and `host.json` carry tokens.
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
  and terminals. **Superseded 2026-10-01** (below).
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

### Addendum — jobs host workspaces (revision 2)
_Captured 2026-10-01 (maintainer, in-session, after trying revision 1's one-job-per-workspace
page and its file peek)._

- **You start jobs, and open workspaces inside them** — like a long-running RStudio or
  VS Code server job: "some people will just launch a Chimaera serve job and look at it,
  but I may request a long running job and open things from there." Several jobs per
  cluster; a workspace is open in one job at a time and moves between jobs with its
  chats, and switching it must be easy.
- **No file browsing on Home or the cluster page** ("that defeats the whole purpose");
  choosing a workspace's folder is the only browsing there. The file peek is gone ("no
  one would EVER use that").
- **Notifications always**, also when looking at something else; windows never open on
  their own.
- **Startup commands stay modifiable per cluster**, at cluster, workspace and job level.
- **Agents know the full Slurm configuration** and are smart about submitting their own
  long-running jobs.
- **Keep it simple:** "SUPER careful with the UI / UX — this can't be advanced, needs to
  be short on jargon and super clear for the user on how everything works."
- **The login-node override stays** for clusters that allow it.
- **Not settled (additions):** the card layout, wording and cadence of notices, how saved
  setups are presented.
