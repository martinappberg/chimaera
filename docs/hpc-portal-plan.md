# HPC clusters — Chimaera in Slurm jobs, workspaces open inside them

Status: **plan, revision 2** (2026-10-01, branch
`claude/chimaera-compute-nodes-373200`, PR #236, not merged). Revision 1
(one job per workspace) is built on that branch; this revision keeps its
foundations and changes the model the user sees: **you start jobs, and you
open workspaces inside them** — several jobs at once if you want, several
workspaces per job, and a workspace can move from one job to another with
its chats. §11 says what carries over, what changes and what goes. The
maintainer's decisions are in §12.

Regular remotes (dev servers, lab machines, cloud VMs) do not change. Builds
on [features/compute.md](features/compute.md),
[features/remote-connect.md](features/remote-connect.md) and
[features/environment.md](features/environment.md).

## 1. Why

HPC centers share login nodes between thousands of users and reserve them for
light interactive work: editing, inspecting, preparing and submitting jobs.
The common rule — written down by some centers, enforced by all — is that
nothing server-like, unattended, or outliving your interactive session runs
there: no listening daemons, no process that keeps another alive or restarts
it, no continuous file sync, no tunnel or relay left running unattended.
Long-lived services belong in a job. A site's admins asked us to stop running
Chimaera on login nodes; every Chimaera user on every cluster would get the
same request, so the fix is the product's default.

What the same rules allow, and what this plan is built from: short commands
on the login node, spaced politely; an `ssh -L` from your own machine through
the login node to a service in your own job; and inside your own job, using
its CPUs and memory fully — including agents working while you are away.

The shape users already know is Open OnDemand's RStudio or VS Code server:
you ask for a job with some resources and time, wait for it, and work inside
it. Chimaera on a cluster works the same way.

## 2. Principles (review criteria for every change here)

1. **Nothing of ours keeps running on a login node.** No `chimaera serve`
   (unless the user opts in, §4.9), no detached `srun`, no relay, no polling
   loop. Only short commands — `squeue`, `sbatch`, `scancel`, `sinfo`, and
   the read-only `chimaera browse` (§8.7), each of which exits at once —
   and the user's own interactive terminal.
2. **Nothing site-specific.** No partition names, site commands, site paths
   or variables, hostnames, or per-site branches in code, tests or docs.
   Everything comes from standard Slurm, the cluster's own answers (its
   refusal messages included) or the user.
3. **Probe, never assume; say "not supported here" plainly** in one sentence.
4. **A person starts every job.** No automatic restarts, scheduled starts,
   requeue (`--no-requeue`) or self-resubmission. The compliant form of
   "restart it" is a notification and one click.
5. **Polite to the scheduler.** At least 60 s between status checks, only
   while something is visible or a job this app started is alive; one
   combined command rather than several; read files where files answer.
6. **Regular remotes are untouched.**
7. **Plain words.** The UI says cluster, job, workspace, chats and
   terminals (§4.10). A user who has never used Chimaera on a cluster must be
   able to tell from the page alone what is running, where, until when, and
   what each button will do. If a screen needs an explanation, the screen is
   wrong.

## 3. The model

| On screen | What it is | Lives |
|---|---|---|
| **Cluster** | a remote where Slurm was detected | Home |
| **Job** | a Slurm job running Chimaera; workspaces open inside it | the cluster's page |
| **Workspace** | a folder on the cluster, with its chats and terminals | inside a job, or "not open" |
| **Chats, terminals** | the sessions inside a workspace (unchanged) | the workspace window |

- A job has a setup (partition, time, CPUs, memory, GPUs, startup
  commands), a node once it runs, and an end time. **Several jobs can run at
  once** on one cluster — a long CPU job and a short GPU job, say.
- A workspace is **open in at most one job at a time**. Open in a job, its
  chats and terminals run on that job's node and share that job's
  resources. It can **move** to another job; its chats come with it, because
  a workspace keeps its chats in its own folder on the cluster, not in the
  job.
- A job can be **empty** (started, nothing opened yet) — like starting
  RStudio before choosing a project.
- **The app is the control plane.** It runs every cluster command itself over
  the existing ssh connection. Nothing on the cluster coordinates anything
  between your clicks except inside your own jobs.

## 4. What the user sees

### 4.1 Home

Home lists hosts and workspaces, never files. A cluster's row:

| State | Row text |
|---|---|
| nothing running | `Slurm cluster · no jobs running` |
| one job | `1 job running · ends in 5d 22h` |
| several | `2 jobs running · next ends in 3h 40m` |
| only waiting | `1 job waiting for a node` |
| login-node override on | `Slurm cluster · Chimaera on the login node` (amber) |

Clicking the row opens the cluster's page. A workspace from a cluster that is
open in a running job also appears among Home's recent workspaces and opens
directly.

### 4.2 The cluster's page

Header: the cluster's name, `Slurm cluster`, **Terminal**, a `…` menu
(**Startup commands…**, **Rules for agents…**, **Run Chimaera on the login
node…**) and **Start a job**.

**First visit** (no workspaces, no jobs):

> **Work on {cluster} in Slurm jobs**
> Chimaera runs inside jobs you start, never on the login node. Add a folder
> to work in, then start a job to open it.
> [**Add a workspace…**] [Start a job]

**Jobs**, one card each, newest first:

- Title: the saved setup's name ("Long", "GPU"), else `{partition} · {time}`.
- One status line:
  - running: `On node-12 · 8 CPUs · 32 GB · ends in 5d 22h`
  - starting: `Starting on node-12…`
  - waiting: `Waiting for a node · Slurm estimates 2:20 pm`, plus Slurm's
    reason in plain words when it isn't ordinary priority
    (`· waiting for free GPUs`, else its own word verbatim)
  - interactive-only partition: `· stops if this app disconnects`
- Actions: running → **Continue in a new job…**, **Stop**; waiting →
  **Cancel**.
- Inside the card, the workspaces open in it: name, folder (muted), what's
  happening (`2 chats working`, `idle`), **Open**, and a `…` menu:
  **Move to {other job}** (one entry per other running job), **Close**,
  **Startup commands…**.
- A waiting job lists the workspaces that `open when it starts`.
- Last row: **+ Open a workspace here** (a menu of the not-open workspaces
  and **Add a workspace…**).

**Not open**: every other workspace on this cluster — name, folder, `last
open 2 days ago · chats saved`, **Open**, and `…` (**Startup commands…**,
**Remove from this list** — "Your files and chats stay where they are").

**Open** never makes the user think about jobs they don't need to:

| Jobs running | Open does |
|---|---|
| none | opens the start sheet with this workspace ticked |
| one | opens the workspace in that job |
| several | a small menu: `In Long` · `In GPU` · `In a new job…` |

Below everything, **+ Add a workspace…**, then one quiet line: `Your other
Slurm jobs: 37 running · 4 waiting` (a count, no controls).

A job that ended stays as one line until dismissed: `Long ended 2 h ago — it
hit its time limit. Chats are saved.` with **Start again** and `×`.

### 4.3 Starting a job

A sheet titled `Start a job on {cluster}`, built only from what the cluster
reports and what the user chose before:

- **Saved setups** as chips, the last one selected; **New**. A first start
  has none and says `Your choices are remembered for next time.`
- **Partition** (only the ones the user may submit to, each with its maximum
  time; `can be preempted` / `interactive only` tags only when the cluster
  said so), **Time** (required), **CPUs**, **Memory**, **GPUs** (only on a
  cluster with GPUs), **Account** / **QOS** (only where used).
- **Open when it starts**: the not-open workspaces as checkboxes, the one
  the user clicked already ticked.
- **Startup commands for this job** (collapsed): `Run before every chat and
  terminal in this job, after the cluster's and each workspace's.`
- **Save as** (optional name) to keep the setup.
- **Start job**. The sheet closes; the job card appears as `Waiting for a
  node`. A refusal shows Slurm's own message and keeps the sheet open.

### 4.4 Adding a workspace

The only file browsing outside a workspace window. A picker titled `Choose a
folder on {cluster}`:

- Starts in the home folder; quick links to the folders that hold existing
  workspaces. A breadcrumb, then **folders only**, each marked `git` or
  `already added` where true.
- A small **Go to** field for typing a path (`~` and `$VARS` expand on the
  cluster).
- **Add** (or **Add and open** while a job runs — it opens in that job, or
  asks which).
- Optional name; defaults to the folder's name.

Before any job runs, each folder is listed by one `chimaera browse` call on
the login node (§8.7); inside a job, by that job.

### 4.5 Inside a workspace window

- Title `{workspace} — {cluster}`; the job strip (built) reads `In job on
  node-12 · ends in 5d 22h · 8 CPUs · 32 GB`.
- An hour before the end and again at ten minutes, a banner: `This job ends
  in 58 min. Continue in a new job and your chats come with you.`
  **Continue…**
- When the job ends: `This job ended — it hit its time limit. Your chats are
  saved.` **Back to {cluster}** · **Close window**.
- When the workspace moves to another job, the window follows by itself and
  says `Moved to GPU` for a moment.

### 4.6 Notifications

Always sent, whatever the user is looking at; windows never open by
themselves.

| When | Title | Body | Action |
|---|---|---|---|
| a job starts | `Your job on {cluster} is ready` | `crc-joint-fold is ready to open.` | Open |
| 1 h left | `Your job on {cluster} ends in 1 hour` | `Continue in a new job to keep working.` | Continue |
| 10 min left | `… ends in 10 minutes` | same | Continue |
| it ended | `Your job on {cluster} stopped` | `It hit its time limit. Chats are saved.` | — |
| refused | `Slurm didn't accept your job` | Slurm's message | — |

### 4.7 Moving, continuing, stopping

- **Move to {job}**: no confirmation (nothing is lost). The workspace closes
  in one job — its chats are saved — and opens in the other; its windows
  follow. The row shows `Moving…` meanwhile.
- **Continue in a new job…**: the start sheet, prefilled with this job's
  setup (ask for more time or CPUs if you like), listing `Moves:
  crc-joint-fold, sc-atlas`. **Start new job**. The old card says
  `Continuing in a new job — waiting for a node`; when the new job starts,
  the workspaces move and the old job stops itself. If the old job ends
  first, its workspaces show `waiting for the new job`.
- **Stop**: `Stop this job? crc-joint-fold and sc-atlas close. Their chats
  are saved.` **Stop job**.
- **Cancel** (waiting job): no confirmation.

### 4.8 Terminal (built)

**Terminal** opens a terminal-only window, `{cluster} · login node`, running
`ssh {cluster}` over the app's connection. Never listed as a workspace, never
restored; its session ends when the window closes.

### 4.9 The login-node override (built)

In the `…` menu, off by default, per cluster, with one warning and a
confirm. While on, the cluster behaves as a regular remote and agents get the
login-node context.

### 4.10 Words

| Say | Don't say |
|---|---|
| job | server, session (for a job), allocation, daemon, instance |
| workspace | cluster workspace, project slot |
| open in a job, move, continue in a new job | attach, migrate, handoff, lease, resubmit |
| ends in 5d 22h | walltime, time limit remaining |
| waiting for a node | pending, queued (in the status line) |
| chats are saved | ledger, journal, resumed |

Slurm's own words appear only where Slurm is the speaker (partition names,
its refusal messages, the reason it gives for waiting).

## 5. What the app learns from the cluster

Discovery (built) runs as one ssh exec when the start sheet first opens,
cached a day per cluster in the cluster folder, re-run after a refusal.

| Fact | Source | Fallback |
|---|---|---|
| Scheduler present | login-shell `PATH` walk | not a cluster |
| Partitions, default, max time, node sizes, GPUs | `sinfo` | — |
| Partitions the user may use | `sacctmgr` associations ⋈ `scontrol show partition` `Allow*` | all; a refusal removes one |
| Accounts, default account | associations | no account field |
| Preemptible partitions | `PreemptMode` | no tag |
| Interactive-only partitions | the cluster's refusal of `sbatch` | — |
| Required account / QOS / constraint | the refusal message, shown verbatim | — |
| GPU syntax | Slurm version | `--gres=gpu:N` |
| Start estimate, why waiting | `squeue --start` / `%r` | `waiting for a node` |
| Why a job ended | `sacct` once | `ended` |
| Compute nodes reach the internet | the job's egress probe | say agents can't run here |
| `sbatch` works inside a job | probed by the job at start | drop that advice for agents |

The same facts feed what agents are told (§7).

## 6. Startup commands

Environment preludes stay as designed: opaque shell lines, never parsed,
concatenated in scope order, run before every chat and terminal. On a cluster
the scopes are:

| Level | Stored | Edited from |
|---|---|---|
| **Cluster** | `env-profiles.json` host scope, in the cluster's shared config dir | cluster page `…` → **Startup commands…**, or Environment settings in any of its workspace windows |
| **Workspace** | `env-profiles.json` workspace scope (same file) | the workspace's `…` → **Startup commands…**, or its window's Environment settings |
| **This job** | the job's folder (§8.2) | the start sheet |

Cluster and workspace are the same scopes the Environment settings already
edit: every Chimaera on a cluster shares that cluster's config dir, so one
file, two editors, no duplicates. The app edits the file over ssh (re-read,
change one scope, atomic rename). Order: cluster ⊕ workspace ⊕ this job ⊕
the per-launch line, so a workspace that needs `R/4.3` and one that needs
`python/3.12` can share a job.

## 7. What agents are told

Delivered once per chat (claude: the hook carrier; codex: a developer note
added when its chat opens — built, PROTOCOL.md Pass 41), and refreshed when a
chat reopens in a new job. Generic text, filled from the job and §5's facts:

```text
You are working inside a Slurm job on <cluster>: job <id> on node <node>,
<cpus> CPUs, <mem> of memory<, <gpus> GPUs>. It ends at <time, date>;
anything still running in it then is stopped. Other workspaces may be open
in the same job and share these resources.

Use this job for interactive work and short runs, and use its CPUs fully
(match threads to $SLURM_CPUS_PER_TASK). For anything that needs more time
than is left, more resources, or a GPU this job doesn't have, submit your own
job with sbatch — it keeps running after this one ends:

  sbatch --time=<always> [--partition=…] [--cpus-per-task=…] [--mem=…] [--gpus=…] job.sh

Chain steps with --dependency=afterok:<id>. Check on your jobs at most once a
minute (squeue -j <id>, sacct -j <id>), never in a tight loop; or tell the
user the job id and stop. Write results somewhere the user can find them.

What you can submit to here:
  partition  max time   largest node            note
  normal *   7-00:00    64 CPUs · 256 GB        default
  gpu        2-00:00    32 CPUs · 512 GB · 4 GPUs
  dev        2:00:00    16 CPUs · 64 GB         interactive only
<Account: … | No --account needed.> <GPUs: --gpus=N.>
```

followed by the cluster's **rules for agents** (built: a file on the cluster
and/or the user's own text, else a short generic set). The partition table
is capped (usable partitions first, at most 12 rows). Where `sbatch` doesn't
work inside jobs, the second paragraph says to ask the user instead. On the
login node (override), agents get the login-node text (built).

## 8. Mechanics

### 8.1 Detection and cluster mode (built)

Scheduler detection rides the connect probe; on a cluster without the
override nothing is ever started on the login node, on any automatic path.

### 8.2 The cluster folder

On the cluster's shared home (`~/.chimaera/cluster/`; dev builds
`~/.chimaera-dev/data/cluster/`), owner-only:

```
cluster.json        workspaces [{id, name, path}], saved setups,
                    rules for agents, learned facts (§5)
w/<wid>/data/       a workspace's Chimaera data: chats, history, timeline,
                    ledger. Its manifest is the lease: one job at a time.
j/<jid>/job.json    setup, name, open-on-start list, `replaces`, Slurm id
j/<jid>/job.sh      the script (0600); job.log Slurm's output
j/<jid>/startup.sh  this job's startup commands
j/<jid>/host.json   written by the job: node, port, token (0600)
```

`jid` is our own id (`j-` + 8 hex), minted before `sbatch`; the Slurm id is
added after. Settings and `env-profiles.json` stay in the cluster's normal
config dir, which every Chimaera on the cluster already shares.

### 8.3 The job: `chimaera job-host`

The job script (`#!/bin/bash -l`, `umask 077`) runs the egress probe and
`exec chimaera job-host --job <jid>`. job-host is the job's main process and
the only long-lived thing per job:

- binds the node's address on a free port, token-gated; writes `host.json`
  atomically; opens the job's open-on-start workspaces;
- **`GET /status`**: the job (Slurm id, node, resources, end) and its
  workspaces (`starting` · `open` · `closing` · `failed`, with each one's
  port);
- **`POST /workspaces/{wid}/open`**: starts that workspace's Chimaera inside
  the job (below). `409` naming the holder when another live job holds the
  workspace;
- **`POST /workspaces/{wid}/close`**: SIGTERM to that workspace's Chimaera;
  it saves its chats and exits;
- if `job.json` says it `replaces` a job: once its first workspace finds the
  lease held by that job, it `scancel`s that job once (today's self-stop,
  moved here), and the workspaces take over as the old ones save and exit;
- on SIGTERM (Stop, the time limit): SIGTERM to every workspace, wait for
  them (bounded under Slurm's kill wait), exit;
- a workspace that exits unexpectedly is reported `failed` with its last log
  lines and never restarted on its own.

No polling; it waits on its children and its socket.

### 8.4 A workspace inside a job

One `chimaera serve --bind-routable` per open workspace, started by
job-host with: data dir `w/<wid>/data`, runtime dir on node-local `/tmp`
(per job, per workspace), the shared config dir, `CHIMAERA_HOST_PRELUDE_FILE`
= the job's `startup.sh`, the rules and facts files. Everything below is
built: the manifest lease (waits while another job holds it), chats resumed
**idle** (no restart pick-up turn — a job can start hours after the click),
agent context, the hidden-workspace and ledger rules.

Memory: each open workspace is a Chimaera process (about 50–150 MB) inside
the job's memory; the start sheet's memory hint says so.

### 8.5 Reaching jobs and workspaces

The app adds one `ssh -O forward` per job (to job-host) and one per open
workspace (to its Chimaera) on its connection to the login node — the built
direct rung, with the ssh-into-node fallback. Windows talk to their
workspace's Chimaera exactly as today.

### 8.6 Status

One ssh exec per cluster serves Home, the cluster page and every window: `date
+%s`, `squeue -u $USER` (when the cache is older than 60 s) and `chimaera
browse --state` (§8.7). Workspace activity (`2 chats working`) comes from
job-host's `/status` through the job's forward, fetched only while the page
is visible.

### 8.7 `chimaera browse`

A read-only subcommand of the binary already deployed to the cluster. It
reads, prints JSON and exits; it never writes, locks, logs to a file or starts
anything.

- `chimaera browse --state`: `cluster.json`, each job's `job.json` and
  `host.json` (token stripped), each workspace's manifest (which job holds
  it). Replaces the shell parsing in today's list exec.
- `chimaera browse --dir <path>`: one folder's subfolders (capped), each
  marked `git` / `workspace`. `~` and `$VARS` expand from its own environment,
  never through a shell.

### 8.8 The sequences

- **Start a job**: write `j/<jid>/` (job.json, job.sh, startup.sh), `sbatch
  --parsable --no-requeue`, record the Slurm id. The card shows `Waiting for a
  node` from then on.
- **Ready**: the status exec sees `host.json`; the app adds the forward and
  notifies.
- **Open**: `POST …/open` → the workspace's manifest appears → forward →
  window.
- **Move A → B**: `POST A …/close`, then `POST B …/open`; B's Chimaera waits
  for the lease; windows re-home.
- **Continue**: start a job with `replaces: A` and A's open workspaces as
  open-on-start; when it runs, it stops A (8.3) and takes over.
- **Stop / Cancel**: `scancel`; "Invalid job id" is success; marked `stopped
  by you`.
- **Ended**: the next status exec finds the job gone; `sacct` once for why;
  windows get the ended screen; a notification.

### 8.9 CLI

`chimaera compute jobs | start | stop | open <workspace> [--job <jid>] |
move | add <path>` over the same code, plus `chimaera browse` (§8.7) — the
verification harness and the browser-only path.

## 9. Things that stay out

- Anything automatic that starts or keeps jobs alive; relays or tunnels on a
  login node; syncing cluster folders to the laptop; site knowledge in code.
- **File browsing on Home or the cluster's page.** Files live in workspace
  windows; the folder picker (§4.4) is the only exception.
- **Windows that open by themselves.** Notifications only.
- **One workspace open in two jobs at once.** It moves instead.

## 10. Scheduler and filesystem budget

| What | Command | When | Floor |
|---|---|---|---|
| Jobs, waiting reasons, other-jobs count | `squeue -u $USER` | Home row or cluster page visible, or a job this app started is alive | 60 s per cluster |
| Folder state | `chimaera browse --state` | with the above | 60 s |
| Start estimate | `squeue --start -j` | with the above, while waiting | 60 s |
| End reason | `sacct -nX -j` | once per ended job | once |
| Discovery | `sinfo`, `sacctmgr`, `scontrol` | start sheet; after a refusal | 1 day |
| Workspace activity | job-host `/status` (no scheduler) | cluster page visible | 15 s |
| Folder picker | `chimaera browse --dir` | per click | — |

## 11. From revision 1

**Carries over** (built and verified live on #236): detection and cluster
mode; the login-node override; the terminal window; discovery and the start
sheet's resource fields; `sbatch` and the interactive-only `srun`; the direct
forward; a workspace's own data folder, the manifest lease, idle resume and
the continuing job stopping the one it replaces; agent context and rules for
agents; the codex developer note; notifications and the in-window banner and
ended screen; the hidden workspace.

**Changes**: launch records move from per workspace to per job (`j/`); the
job script execs `job-host` instead of `chimaera serve`; the cluster page and
start sheet as in §4; status through `chimaera browse --state`; startup
commands become the `env-profiles.json` scopes plus this job's; agent context
gains the Slurm setup (§7); Continue and Stop act on a job.

**Goes**: the file peek (`ClusterFiles`, `cluster_list`, `cluster_peek`, the
peek folder); the cluster-default and workspace startup fields in
`cluster.json`; one job per workspace.

**Order of work** (all in #236): job-host, `chimaera browse` and the folder
layout, verified from the CLI on a real cluster; the app's job commands
(start, open, close, move, continue, stop) and status; the cluster page,
start sheet and folder picker with §4's words; agents' Slurm setup; docs; a
live pass on a real cluster with two jobs; the maintainer's click-through.

## 12. Decisions

2026-09-30:

1. No Chimaera on a cluster's login node by default; a per-cluster override
   with a blunt warning.
2. Jobs listen on the node's address, token-gated, reached by a plain `ssh
   -L` through the login node (reverses the 2026-07-16 loopback addendum).
3. ~~One job per workspace~~ — superseded by 11.
4. Nothing site-specific.
5. A person starts every job; nothing restarts or resubmits on its own.
6. Startup commands stay.
7. Agents are told they are in a job, its resources and end, to submit longer
   work as separate jobs, and the cluster's rules for agents.
8. The start sheet shows discovered facts and saved setups, no invented
   presets.
9. No sync.
10. The app holding a job's forward for the job's lifetime is fine.

2026-10-01:

11. **Jobs host workspaces.** Several jobs per cluster; a workspace is open in
    at most one job and moves between jobs with its chats; a job can be
    empty.
12. **No file browsing on Home or the cluster page.** The folder picker when
    adding a workspace is the only browsing outside a workspace window; the
    file peek is dropped.
13. **`chimaera browse`**: read-only, exits at once, the only Chimaera
    command run on the login node.
14. **Notifications always; windows never open by themselves.**
15. **Startup commands at three levels** — cluster, workspace, this job —
    all editable from the cluster page; cluster and workspace are the
    Environment settings' own scopes.
16. **Agents know the cluster's Slurm setup** and how to submit and watch
    their own jobs.
17. **Plain words** (§4.10); the UI must explain itself.

## 13. Open questions

- **Compute nodes without internet**: agents can't run in jobs there. Beyond
  saying so and a proxy line in startup commands, anything else?
- **Carrier size**: the facts table plus a published rules file against the
  hook carrier's limit — measure, then choose inline vs "read this file".
- **Shutdown time**: several workspaces saving at once against Slurm's kill
  wait (often 30 s) — measure with three open workspaces.
- **Agent preferences** (last model and effort) live in each workspace's
  data folder today; share them per cluster through the config dir?
- **The lease on NFS**: which atomic primitive holds on the filesystems
  clusters actually use.

## 14. Testing

- **CI**: the `CHIMAERA_SLURM_BINDIR` stand-ins (accounts required, batch
  refused, preemptible, no `sacct`, no egress); job-host with fake
  workspace processes (open, close, 409, SIGTERM fan-out, a child that dies);
  `chimaera browse` against fixture folders.
- **Live**, on a real cluster: two jobs at once; open, move between them,
  continue, stop, and a 5-minute job reaching its time limit; `ps -u $USER`
  on the login node after each, after the app quits and after a sleep.
