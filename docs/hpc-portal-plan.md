# HPC clusters — workspaces as jobs, nothing on the login node

Status: **plan** (2026-09-30, branch `claude/chimaera-compute-nodes-373200`).
Nothing here is built yet. It replaces how Chimaera treats an HPC cluster:
today `connect` starts a long-lived `chimaera serve` on the cluster's login
node and that daemon launches compute-node sessions; after this plan, a
cluster's login node only ever sees short commands and an interactive ssh
terminal, and Chimaera runs **inside Slurm jobs**, one per workspace, started
and stopped from the app. Regular remotes (dev servers, lab machines, cloud
VMs) do not change at all. The maintainer's decisions are collected in §12.

Builds on [features/compute.md](features/compute.md) (Slurm awareness and
today's compute sessions), [features/remote-connect.md](features/remote-connect.md)
(the ssh layer this reuses), [features/environment.md](features/environment.md)
(startup commands) and the architecture guide's
[compute-node sessions](agent-guides/architecture.md#environment-prelude--compute-node-sessions)
design, whose "Mode 2" becomes the default and whose "Mode 1" becomes an
explicit override.

## 1. Why

HPC centers share login nodes between thousands of users and reserve them for
light interactive work: editing, inspecting, preparing and submitting jobs.
The common rule — written down by some centers, enforced by all — is that
nothing server-like, unattended, or outliving your interactive session runs
there: no listening daemons, no process that keeps another alive or restarts
it, no continuous file sync, no tunnel or relay left running unattended.
Such processes are killed without notice, and repeated or deliberate cases
put the user's account at risk. Long-lived services belong in a job.

Chimaera's login-node daemon breaks every clause of that rule: it detaches,
listens, keeps agents alive while nobody is there, and the app restarts it on
its own (window restore, reconnect after sleep, a 401, a compute window
healing its login tunnel). A site's admins asked us to stop. The same request
would reach every Chimaera user on every cluster, so the fix is the product's
default, not one user's workaround.

What the same rules explicitly allow, and what this plan is built from:

- short commands on the login node (`sbatch`, `squeue`, `scancel`, `sinfo`,
  reading small files), spaced politely;
- helpers an interactive tool starts for itself and stops with it;
- an `ssh -L` from your own machine, through the login node, to a service in
  your own job, for as long as you use it;
- inside your own allocation, using its CPUs and memory fully — including
  agents working on a bounded task while you are away.

## 2. Principles (review criteria for every PR here)

1. **Nothing of ours keeps running on a login node.** No `chimaera serve`
   (unless the user opts in, §4.6), no detached `srun` client, no relay
   process, no polling loop. Only short commands the app runs while it is
   open, and the user's own interactive terminal.
2. **Nothing site-specific.** No partition names, no site commands (helper
   scripts some centers ship), no site paths or environment variables, no
   hostnames, no per-site branches. Everything comes from standard Slurm, from
   the cluster's own answers (including its refusal messages), or from the
   user. Clusters we test on are test beds, never special cases in code.
3. **Probe, never assume; say "not supported here" plainly.** Every
   per-cluster fact (§5) is discovered or learned, cached, and re-learned when
   the cluster says otherwise. Where a capability is missing (compute nodes
   unreachable, no internet on nodes), the UI says so in one sentence.
4. **A person starts every job.** No automatic restarts, no scheduled starts,
   no requeue (`--no-requeue`), no job that resubmits itself. The compliant
   form of "restart it" is a notification and one click.
5. **Polite to the scheduler.** At least 60 s between scheduler status
   checks, only while something is visible; prefer one combined command to
   several; read files instead of asking the scheduler wherever a file
   answers (§10).
6. **Regular remotes are untouched.** Cluster mode applies only to hosts where
   a scheduler is detected. A host without one behaves exactly as today.

## 3. The model

- A **cluster host** is a remote where a scheduler was detected. Its login
  node gets an interactive terminal and short commands; Chimaera never runs
  there by default.
- A **cluster workspace** is a folder on that cluster's shared filesystem
  plus a remembered **setup** (partition, time, resources, startup commands).
  It is either **running** (a job holds it), **waiting for a node** (the job
  is queued), **starting** (the job runs, Chimaera is booting), or
  **stopped**.
- **One job per workspace.** A running workspace is a Slurm job whose body is
  the startup commands followed by `chimaera serve`. Each project's resources
  and time limit are its own; two jobs never write the same workspace's state.
  (Opening a second folder inside a running job can come later.)
- **"Session" keeps its meaning**: the chats and terminals inside a
  workspace. The job is a property of the workspace ("running on node X ·
  4 CPU · 16 GB · 5d 22h left"), never a list of "sessions".
- **The app is the control plane.** The app (and the CLI) run the cluster
  commands themselves over the existing ssh ControlMaster. Nothing on the
  cluster coordinates anything between your clicks.
- **The job is reached Slurm's ordinary way**: the job's Chimaera listens on
  the node's network address (token-gated), and the app holds a plain
  `ssh -L <port>:<node>:<port> <host>` through the login node for as long as
  the job runs and the app is open (§8.5).

## 4. What the user sees

### 4.1 Home

A cluster host's row reads differently from a server's: a "Slurm cluster"
label and a one-line state ("crc running · 5d 22h left", or "no workspace
running"). A server row keeps "online · 127.0.0.1:port". Cluster workspaces
also appear among the home's workspaces with a cluster mark and their state;
opening one opens it if running, or offers to start it.

### 4.2 The cluster page

Clicking a cluster host opens its page (the remote detail page today):

- **Masthead**: name, "Slurm cluster · Chimaera runs on compute nodes only",
  the login node the app is connected to, **Terminal**, and a `…` menu.
- **Workspaces**, each with its state dot, path, and one line of detail:
  running → node · resources · time left · "3 chats", with **Open** and
  **Stop**; waiting → "waiting for a node · Slurm estimates 14:20" (and
  Slurm's reason when it isn't plain priority), with **Cancel**; stopped →
  why it stopped (time limit, cancelled, failed, preempted, node failure) and
  "chats saved", with **Start**.
- **Add a workspace**: a path on the cluster, checked over ssh, plus a name.
- One quiet line: "Your other jobs on this cluster: 2 running · 9 waiting" —
  a count only, no controls (the compute chip's "indicator, not controller"
  rule).

### 4.3 Starting a workspace

The start sheet is built only from what the cluster reports and what the user
chose before:

- **Saved setups** first (the last one preselected); a first start has none
  and says "this one will be remembered".
- **Partition**: only partitions the user can submit to (§5), each with its
  maximum time; the default partition marked; tags only for facts the
  cluster reported or taught us — "can be preempted", "interactive only ·
  stops when you disconnect".
- **Time**: required (every job states its time limit), as d/h/m boxes,
  checked against the partition's maximum.
- **CPUs, memory, GPUs**: optional — blank means the cluster's defaults.
- **Account, QOS, constraint**: shown only on clusters that use them, filled
  from the user's associations; a field the cluster requires (learned from
  its refusal) becomes required.
- **Startup commands**: the cluster default, the workspace's, and lines for
  this run only, each labelled with where it comes from (§6).
- After **Start**: "Submitted · waiting for a node", with Slurm's start
  estimate; the window opens by itself when Chimaera is up, and a system
  notification says so.

### 4.4 Inside a running workspace

The existing compute window identity stays: title `{host} › {node} —
{workspace}`, the allocation strip (node · time left · resources), the
banner on the home page. New:

- An hour before the end (and again at ten minutes), a non-blocking banner
  plus a notification: "Stops in 58 min. Continue on a new node and your
  chats move with you." **Continue on a new node** queues the next job now
  with the same setup; when it starts, live chats move over (the restart
  handoff), the window re-homes onto the new node, and the old job ends. If
  the old job ends first, the window says so, keeps the chats, and shows the
  waiting job.
- When the job ends while the window is open: "This workspace stopped (time
  limit). Your chats are saved." with **Start again** and **Back to
  {host}**.

### 4.5 Terminal

**Terminal** opens a small window, "{host} · login node", with a terminal
running `ssh {host}` over the app's ControlMaster (no second password or 2FA
prompt), and a quiet line: "Login node — for editing, inspecting and
submitting jobs." It is the user's own interactive session and ends when
closed.

### 4.6 The login-node override

In the cluster page's `…` menu: **Run Chimaera on the login node**, off by
default, per host. Turning it on shows one warning and needs a confirm:
"Chimaera and its agents would keep running on a shared login node after you
disconnect. Many clusters don't allow that. Turn this on only if your
cluster's admins say it's fine." While on, the host row carries an amber
"login node" label and the host behaves as a regular remote (today's flow),
and agents there get the login-node context (§7). Without the override, a
login-node daemon found from before gets a notice with **Shut it down**
(SIGTERM-only, as always).

## 5. What the app learns from the cluster

All discovery runs as one ssh exec through the master, when the start sheet
opens, cached for a day per host, and re-run after a refusal. Learned facts
live in the cluster folder (§8.6) so the CLI and later windows share them.

| Fact | Where it comes from | Fallback |
|---|---|---|
| Scheduler present | login-shell `PATH` walk for `sbatch`/`squeue`/`scancel`/`sinfo`/`srun` (and `qsub`/`qstat`, `bsub`/`bjobs` for PBS and LSF) | not a cluster |
| Partitions, default, max time, node sizes | `sinfo -h -o "%P\|%a\|%l\|%c\|%m\|%G"` | — |
| Partitions the user may use | `sacctmgr -nP show assoc user=$USER format=account,partition,qos` ⋈ `scontrol show partition` (`AllowAccounts`, `AllowGroups` vs `id -Gn`, `AllowQos`) | all partitions; a refusal removes one |
| Accounts, default account | the associations above; `sacctmgr -nP show user $USER format=defaultaccount` | no account field |
| Preemptible partitions | `scontrol show partition` `PreemptMode` | no tag |
| Interactive-only partitions | the cluster's refusal of `sbatch` (its own message is shown) | — |
| Required QOS / constraint / account | the cluster's refusal message, parsed loosely, shown verbatim | — |
| GPU syntax | `--gpus` (Slurm ≥ 19.05), else `--gres=gpu:N` | — |
| Start estimate | `squeue --start -j <id>` | "waiting for a node" |
| Why a job ended | `sacct -nX -j <id> -o State,ExitCode` once | "ended" |
| Compute nodes reach the internet | the job's egress probe (exists: `caps.json`) | agents can't run in jobs here — say so |
| Compute nodes reachable from the login node | the connection ladder (§8.5) | "can't reach compute nodes here" |
| Shared home between login and compute nodes | the job's manifest appears where the app looks | say so |

## 6. Startup commands

Environment preludes stay exactly as designed: opaque shell lines, never
parsed, concatenated in scope order and run before Chimaera and in every
shell and agent it spawns. On a cluster the scopes are **cluster default**
(what the host default is elsewhere) ⊕ **workspace** ⊕ **this run**.

Only storage changes: there is no login-node daemon to hold them. The cluster
default and saved setups live in the cluster's normal config dir on its
shared home (`${XDG_CONFIG_HOME:-~/.config}/chimaera`, or the dev home), which
every job daemon on that cluster already reads as its config dir, so the
Environment settings panel in any running workspace edits them; the
workspace scope lives with the workspace. The app reads and writes the same
files over ssh for the start sheet (small JSON, atomic temp + rename, re-read
before write).

## 7. What agents are told

The existing compute-node context (`compute::agent_context`, delivered once
per session through the hook carrier) becomes, generically:

```text
You are running inside Slurm job <id> on node <node> (cluster <host>,
partition <p>) with <cpus> CPUs, <mem> of memory and <gpus>. The job ends
around <absolute time>; anything still running then is stopped.

- Commands you run execute inside this job. Use its CPUs and memory fully
  (match thread counts to $SLURM_CPUS_PER_TASK).
- For work that needs more time than is left, more resources, or a GPU,
  submit a separate job with sbatch and an explicit --time. Check on it at
  most once a minute, or chain steps with --dependency.
```

followed by the cluster's **rules for agents**:

- A per-cluster setting, **Rules for agents on this cluster**: a file on the
  cluster (some centers publish one) or text the user pastes. Appended
  verbatim when it fits the carrier's size limit; otherwise the agent is told
  to read that file before significant work.
- Without one, a short generic paragraph: explicit `--time` on every job, no
  queue checks more often than once a minute, no polling loops, nothing left
  running on login nodes, nothing that keeps itself alive.

With the login-node override (§4.6), agents get the login-node version: on a
shared login node, not in a job; light editing, inspecting and preparing jobs
only; submit anything heavy; leave nothing running.

Codex, uninformed today, gets the same text as a developer message added
once its chat opens (`thread/inject_items`, over stdin): never argv, which
anyone on a shared node can read, and unlike opening instructions it also
reaches a reopened chat, so a chat continued in a new job learns the new job. Whether `sbatch` works from inside a job
is probed at job start and the second bullet is dropped where it doesn't.

## 8. Mechanics

### 8.1 Detection and cluster mode

- The connect probe (one exec, `remote_probe`) adds the scheduler PATH walk
  to its script; the verdict rides the probe's framed output.
- `resolve_daemon` gains a cluster branch: on a cluster host without the
  override it **never** calls `ensure_remote_binary` + `start_remote`, never
  updates, and never tunnels to a login daemon. It returns "cluster" (with a
  found login daemon reported, not used).
- Every automatic path inherits that branch, because they all go through
  `do_connect`: launch restore (`restore_windows`), a window's reconnect on
  `down` or 401 (App.svelte → `connectHost`), a compute window healing its
  login tunnel (`connect_compute_session`). Restored remote-workspace records
  for a cluster host are dropped and the cluster page opens instead.
- `HostEntry` gains `login_serve: bool` (`#[serde(default)]`). An older build
  that rewrites `hosts.json` drops it, which turns the override **off** — the
  safe direction. The scheduler verdict is not persisted: it is re-probed on
  every connect.
- CLI: `chimaera connect <cluster>` explains cluster mode and exits unless
  `--login-node`; `chimaera status <cluster>` says "cluster".

### 8.2 The control plane on the laptop

A new `chimaera-remote` module (`cluster.rs`) owns every cluster command, each
one bounded ssh exec through the master (`sh_wrap`, output caps, timeouts),
shared by the app and the CLI. Pure parts — `parse_squeue`, `parse_sinfo`,
the time parsers, the script builder, spec validation (extended with account,
QOS, constraint, GPUs) — move from `chimaera-server` into a `slurm` module in
`chimaera-core`, which both the server (the in-job context) and
`chimaera-remote` depend on. The login-daemon routes `/compute/sessions` and
the host page's hub that called them are retired (a deliberate wire removal,
called out in that PR); `GET /compute` stays (the in-job daemon's own view).

The **list** exec, the one that runs repeatedly, combines in one round trip:
`squeue -u $USER -h -o …` (only when the per-host cache is older than 60 s),
`date +%s` (remote clock for every age), and the cluster folder's per-
workspace launch records and manifests (`cat`, no scheduler). The app keeps
one cache per host shared by the home row, the cluster page and every window.

### 8.3 Starting a job

- `sbatch --parsable` with the script on stdin (no adoption by name: sbatch
  returns the id), `--job-name chimaera-<ws-slug>`, `--time`, the optional
  partition/account/QOS/constraint/CPUs/memory/GPUs, `--no-requeue`, and
  `--output` into the workspace's folder. The launch record is written
  atomically before submit and completed with the id after.
- **Interactive-only partitions**: an `srun` that the app's own ssh channel
  holds in the foreground — a helper started and stopped with the app. It
  ends when the app disconnects (laptop sleep, quit); the UI says so up
  front. Never `setsid`/`nohup`/backgrounded.
- The detached-`srun` launcher (`compute_jobs.rs`) is removed.

### 8.4 The job script

`#!/bin/bash -l`, `umask 077`, unset inherited Chimaera prelude variables,
the startup commands verbatim, the egress probe, then
`exec chimaera serve --bind-routable` with:

- **config** = the cluster's normal config dir (shared by all jobs on the
  cluster: settings, the cluster-default startup commands, saved setups,
  plugins);
- **data** = the workspace's own folder on the shared filesystem (§8.6) —
  manifest, ledger, chat journals, timeline, view state — which moves from
  job to job;
- **runtime** = job-local scratch (`$TMPDIR` when Slurm sets one, else
  `/tmp/chimaera-$UID-$SLURM_JOB_ID`), gone with the job.

This needs data/runtime overrides beside `CHIMAERA_HOME` in `chimaera-core`
(today `CHIMAERA_HOME` moves all four dirs together).

### 8.5 Reaching the job

The ladder is reordered and shortened:

1. **Direct** (default): the job binds the node's address (token-gated,
   per-job token) and the app asks its ControlMaster to add
   `-L <local>:<node>:<port>` (`ssh -O forward`). Nothing new runs on the
   login node; its sshd does the forwarding.
2. **Ssh into the node** (fallback, clusters that adopt ssh into a user's
   job): the existing laptop-to-node rung.
3. Otherwise: "can't reach compute nodes from here" — the job keeps running.

The login-node relay rung (`spawn_chained_node_tunnel`, an `ssh -N -L`
process on the login node) is retired. Every rung keeps `tunnel_proven`
(bearer-authed 200 through our own forward). The app holds the forward while
it runs and the job lives, re-adds it after sleep, and cancels it when the
job ends.

### 8.6 The cluster folder

On the cluster's shared home, under the release home (`~/.chimaera/cluster/`;
the dev home for dev builds, via `RemoteHome`), owner-only:

```
cluster/
  workspaces.json        id, name, path, setup id — the cluster's workspace list
  facts.json             learned facts (§5) + when
  workspaces/<ws>/
    data/                the workspace's Chimaera data dir (manifest = lease)
    launch.json          current/last job: id, setup, submitted, ended reason
    job.sh  job.log      the script (0600) and Slurm's output, last run only
```

Saved setups and the cluster-default startup commands live in the config dir
(§6). The **manifest is the lease**: a job's Chimaera refuses to start
serving a workspace whose manifest names another job that is still in the
queue (the app checks; the job waits for the handoff, §8.7).

### 8.7 Moving between jobs

- **Start**: the new job's Chimaera finds the workspace's data dir, resumes
  its chats from the ledger (claude `--resume`, codex threads) **idle** — no
  restart pick-up message on clusters, since a job may start hours after the
  click with nobody there.
- **Continue on a new node**: queue the next job now; when it reports in, the
  app asks the old daemon for its graceful stop (ledger snapshot), the old
  daemon exits (its job ends with it), and the new one takes the lease and
  restores. The restart-handoff code already does the hard half.
- **Stop**: `scancel`; "Invalid job id" is success; the record is marked so
  the stopped line says "stopped by you" rather than inventing a reason.

### 8.8 CLI

`chimaera compute list|start|stop|open <host> [<workspace>]` over the same
`cluster.rs`, no login daemon needed — the verification harness and the
browser-only path.

## 9. Things that stay out

- **Anything automatic that starts or keeps jobs alive** (watchdogs, timers,
  `scrontab`, `--begin`, self-resubmission, requeue). A notification plus one
  click replaces all of them.
- **Relays or tunnels held on a login node.** The forward lives in the
  login node's sshd for as long as the app holds it; nothing else.
- **Syncing a cluster folder to the laptop.** Data doesn't fit, copies drift,
  commands run in the wrong place, and continuous sync is exactly what login
  nodes forbid. Code moves with git; data stays where it is.
- **Site knowledge in code** (§2.2).

## 10. Scheduler and filesystem budget

| What | Command | When | Floor |
|---|---|---|---|
| Workspace states, other-jobs count | `squeue -u $USER` (+ file reads) | home row or cluster page visible | 60 s per host |
| Start estimate | `squeue --start -j` | with the list while a job waits | 60 s |
| End reason | `sacct -nX -j` | once per ended job | once |
| Discovery | `sinfo`, `sacctmgr`, `scontrol show partition` | start sheet opens; after a refusal | 1 day |
| Readiness | read the workspace manifest (no scheduler) | a start is on screen and the job runs | 15 s, ≤ 10 min |
| In-job end time | `squeue -j $SLURM_JOB_ID -o %e` | once at boot; the countdown ticks locally | once (+ 10 min) |

Today's floors this replaces: the host page's 30 s / 5 s polls, launch
adoption's 4 × 1.5 s, and the in-job snapshot's 30 s TTL.

## 11. PRs

1. **Cluster mode** — the safety fix, ships alone. Scheduler detection in the
   probe; the cluster branch in `resolve_daemon` and every automatic path;
   `login_serve` + the override with its warning; the found-login-daemon
   notice with **Shut it down**; the cluster row and page shell with
   **Terminal**; CLI refusal; docs (remote-connect, compute, architecture).
   Verify live: connect, relaunch, sleep/wake against two clusters — no
   process of ours on the login node afterwards (`ps -u $USER`).
   Until PR 3 lands, workspaces can't be started from the app on a cluster.
2. **Agent context on clusters** — the §7 texts, the rules-for-agents
   setting with the generic default, codex through a developer note,
   end time from `%e` once. Server-side; verify with claude chat + TUI and
   codex chat inside a real job.
3. **Starting workspaces from the app** — `chimaera-core::slurm`,
   `chimaera-remote::cluster`, discovery (§5), the start sheet, saved setups,
   `sbatch` + the interactive-only `srun`, the job script and dir overrides,
   the direct forward, the cluster page's workspace states, notifications,
   the other-jobs line, CLI, retiring `/compute/sessions` and the relay rung.
4. **Workspaces across jobs** — the cluster folder, the manifest lease,
   idle resume, **Continue on a new node**, the stopped screen and **Start
   again**, cluster workspaces on the home, **Add a workspace**.
5. **File peek** (optional) — a read-only browser on the cluster page: one
   `ls` per folder click, a file copied to the laptop on open (size-capped)
   into the normal preview.

Later: an HPC workbench plugin (agents probe and record cluster facts, write
setups and rules with the user), launchers for PBS and LSF, opening a second
folder inside a running job.

## 12. Decisions (maintainer, 2026-09-30)

1. No Chimaera daemon on a cluster's login node by default; a per-host
   **Run Chimaera on the login node** override with a blunt warning.
2. Jobs listen on the node's network address by default, token-gated, and
   are reached with a plain `ssh -L` through the login node. **This reverses
   the 2026-07-16 addendum** in features/compute.md ("loopback stays the
   default; routable is per-launch opt-in"); the login-node relay is retired.
3. **One job per workspace**; the UI is workspace-first; "session" keeps
   meaning chats and terminals.
4. **Nothing site-specific** — HPC/Slurm-generic only; sites are test beds.
5. A person starts every job; nothing restarts or resubmits on its own.
6. Startup commands stay, with the cluster default stored on the cluster.
7. Agents are told they are inside a job, its resources and end, to submit
   longer work as separate jobs, and the cluster's rules for agents.
8. Presets are not invented: the start sheet shows discovered facts and the
   user's own saved setups.
9. No sync; an on-demand, read-only file peek is optional.
10. The app holding the job's forward on the user's own machine for the job's
    lifetime is fine.

## 13. Open questions

- **Clusters whose compute nodes have no internet**: agents can't run in
  jobs. Beyond saying so and the proxy line in startup commands, is anything
  else worth offering?
- **The hook carrier's size limit** for appended rules (a published rules
  file can be ~10 KB) — measure before choosing the inline/pointer cutoff.
- ~~**Codex `developer_instructions`**: does `-c` replace a user's own
  value?~~ Moot: the note rides `thread/inject_items` beside the user's own
  instructions (PROTOCOL.md Pass 41).
- **The manifest lease on NFS**: which atomic primitive (O_EXCL create,
  `mkdir`, rename) holds on the filesystems clusters actually use.
- **Partition visibility**: the exact join of associations and partition
  `Allow*` lists, checked against a cluster that enforces accounts and one
  that doesn't.

## 14. Testing

- **CI**: the existing `CHIMAERA_SLURM_BINDIR` stand-ins, extended with modes:
  accounts required, batch refused on one partition, a preemptible partition,
  `sacct` missing, compute nodes without egress, and a cluster with PBS
  commands only.
- **Live**: two clusters of opposite styles — one without accounts, with an
  interactive-only partition and a preemptible one; one with enforced
  accounts and partitions the user can't use listed by `sinfo`. Each PR
  states what was run and observed, including a `ps -u $USER` on the login
  node after the app quits and after a sleep.
