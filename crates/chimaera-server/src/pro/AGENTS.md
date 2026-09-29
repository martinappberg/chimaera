# Optional mirrors and workspace ownership

This module owns daemon-side background mirrors and handoff. Parent:
[server map](../../AGENTS.md). It is inert until the native app provides a scoped,
revocable delegation over the authenticated local API.

| File | Responsibility |
| --- | --- |
| `mod.rs` | Bounded, credential-free persistent state, ownership/import fences and deferred-command policy. |
| `authority.rs` / `authority_tests.rs` | Immutable workspace-bound worker acceptance, credential-free persisted latch, renewal/route/root guards and synthetic side-effect regressions. |
| `routes.rs` | Authenticated configure/status/privacy/profile/power/hydration HTTP handlers. |
| `projects.rs` | Passive bounded cloud-project discovery and explicit per-device local adoption; native-picked folder validation, saved directory identity, retry and legacy-import fences. |
| `projects/tests.rs` | Synthetic loopback HTTP plus real Git transfer, passive-read, conflict, retry, restart and two-device destination checks. |
| `execution.rs` / `execution/` | Negotiated execution leases, independent stop watchdog, durable launch/crash evidence, immutable receipts and stopped same-installation recovery. |
| `execution/mutation.rs` | Bounded file/lifecycle/command commit reservations; account/epoch admission uses short in-memory locks, while clean stop and replacement wait for actual work even if its HTTP caller disappears. Reserved launches fail promptly if configuration is draining them, rather than waiting on themselves. |
| `engine.rs` | Lease renewal (own account-request budget; installs and agent stops run as their own tasks), mirror coordinator, staged hydration (files are installed in place, not transactionally), deadline-bound sleep flush, three-way return and lazy return. |
| `detached.rs` | Owned transfer tasks keyed by (kind, project, epoch): a caller that disconnects never cancels a flush or hydration; repeats join; a completed release is remembered ten minutes. |
| `drain.rs` | `POST/DELETE /pro/drain`: refuse new transfer work and wait for jobs, transfer tasks, project caches and Git helpers before a cloud machine suspends. |
| `continuity_tests.rs` | Loopback account fixture (records requests, scripted grants, delays; Git endpoints refuse connections) for policy, abandoned flush, sleep deadline, own-epoch reacquire, lapsed cloud lease and drain tests. |
| `snapshot_diagnostics.rs` | Fixed snapshot failure categories; no response bodies, paths, identifiers or error text enter diagnostic logs. |
| `handback.rs` | Bounded automatic return coordination across worker wake and ownership changes; lost release replies are resolved by authority reads without repeating ambiguous requests. |
| `release.rs` | Bounded clean-release retry for the account publication fence; changed ownership, account or lease never retries. |
| `provider_gate.rs` / `provider_tests.rs` | Per-agent cloud readiness, bounded blocked-provider status, and staged retry/cancellation tests with a synthetic CLI and real PTY. |
| `protocol.rs` | Additive account contract subset and strict worker host-to-holder identity translation; intentionally no link/TLS dependency in the daemon. |
| `transport.rs` | Bounded external curl/git children; cached mirror-only Git compatibility selection; credentials only in memory, never argv or Git config. |
| `policy.rs` | Mirrored-path policy, credential filtering, size budgets and cloud-profile classification. |
| `mirror.rs` | Separate shadow and repository Git directories, incremental transfer and conservative hand-back. |
| `shadow_cache.rs` | Validated reconstruction of an objectively damaged outgoing shadow, retaining its complete prior store in a bounded no-overwrite quarantine. |
| `repository.rs` | Portable remote/tracking allowlist; bounded ref import, compare-and-swap adoption and index/ref-lock cancellation cleanup. |
| `canonical.rs` | Keeps the user's own version of a conflicting file right beside it (`<name>.mine-<yyyymmdd-hhmm>`) when a return installs the incoming one; bounded per return, never overwrites an earlier copy, never mirrored. |
| `config.rs` | Portable agent configuration export/import, scoped environment-omission diagnostics and destination connection identity preservation. |

One recorded holder and epoch controls shared writes. **Laptop first (D1):** a
personal computer is fenced only by a verified other owner (`Ownership::Remote`,
from an authenticated read) or its own in-progress transfer (`Transferring`,
`Hydrating`, `SettingUp`); `AwaitingVerification` stays writable there. Lease
expiry, account unreachability, sign-out (`disconnect` never stops sessions),
plan changes, the privacy switch and daemon restarts stop publication only. A
verified other owner refuses input at once (`may_write`); the device's agents
then stop at their next safe pause, bounded to five minutes, as an owned task.
A cloud worker (`execution::worker`: `CHIMAERA_WORKER`, a persisted worker
marker, or a Worker runtime) stays strict: its execution needs an unexpired
acquire/renew proof, never a passive GET; a request-start deadline reserves stop
time; clock divergence closes admission; and the watchdog (started only on
workers) fences chat/PTY input and owned agent process groups. Renew before
fencing: when the watchdog sees the process was frozen (a 100 ms tick taking
over 3 s, or wall and monotonic time disagreeing by over 1 s) and a deadline
lapsed across it, it wakes the lease loop, which renews the recorded epoch
(the account keeps a suspended owner's lease: same epoch, no fork, never an
acquire/checkpoint install); input stays admitted meanwhile. Admission and the
lease loop notice a freeze themselves when the watchdog has been silent for
longer than one (`execution::thawed`), so the request that woke the machine is
never refused before the watchdog's next tick. A forwarded viewer scoped to the epoch
being renewed (first socket frame or scoped HTTP) waits for that renewal
(`execution::await_renewal`, woken by the proof change, never past the resume
window) instead of being refused as `workspace_scope_changed`; a refused
renewal or another epoch is refused at once. A worker that re-acquires its own
held epoch (`held_here`, not mid-arrival) continues its own work like a device:
no install, no re-import, no second transfer pickup. A refused renewal
or another verified owner fences at once; no answer within 20 s fences too.
`lease_valid` gates publication and forwarded viewers on every host. Plain shells are never
managed: not signalled by fences, not awaited by stops, never evidence. Sessions
a previous daemon left running wait for this life's lease (`may_restore`); on a
device `resume_unverified` resumes the restart-deferred ones after one minute
when the account cannot confirm, unless another owner was verified meanwhile.
Plain shells never wait at boot. The fallback leaves alone projects the account
answered for this life and projects with a checkpoint install scheduled (fenced
from scheduling, before hydrate's own fence), and re-runs once recorded old
process groups exit.
Clean release waits for observed termination, durable publication and an exact
immutable keeper receipt. Each managed launch persists active execution
evidence, and every state write (plus two writes shortly after each managed
launch) records the live managed agents' process groups with their leaders'
start times (≤64 per project). A graceful stop clears the evidence once they
exit; a same-boot successor after a crash probes the recorded groups and waits
only for survivors (re-probed every lease tick): a group that vanished, belongs
to another user (EPERM), or whose leader started at another time (a reused id)
is gone; without recorded groups a device
proceeds and a worker stays fenced, and a worker never acquires or renews a lease
it could not accept. A separate enrollment latch rejects lost ordinary state or
protocol downgrade; an unreadable `state.json` fails closed (kept as
`state.json.damaged`). `state.json` is written (durably) before the latch.
Failing closed is per project (`execution::uncertain`): a latched project
without its policy stays managed and publishes nothing (a worker also runs
nothing there) until an authoritative read restores its policy. An unenrolled
project never becomes managed on a device (the account itself refuses a legacy
downgrade of an enrolled one); only a cloud machine whose latch or state cannot
be read treats each project with local mirror data as uncertain. Only an
existing `state.json` that does not parse is damage; an I/O error is retried and
then treated as unknown without setting the file aside. A device keeps running
uncertain projects (D1). A project's first enrollment adopts agents already running
there (`execution::adopt_running`): they keep their processes, count as this
life's managed workload and get their groups recorded; nothing stops or restarts.
macOS boot-session UUID and Linux boot ID can distinguish a cold reboot; neither
authorizes takeover by another device.

Strict `managed_processes` retains `expired_takeover:false`. The separately
negotiated `canonical_checkpoint` capability supports automatic native-conversation
fork after a server-authored reconnect grace, preserving the logical session and
single canonical checkpoint authority. It does not claim physical stop of all
old-host descendants or exactly-once external effects. Recovery context tells the
agent to inspect uncertain effects under its existing permissions, without a new
routine human-review gate. Returning local canonical files still waits for local
managed quiescence and retains conflicting unpublished files outside the mirror. Native installation
proofs never enter the daemon. Recovery accepts only a bounded same-installation
mirror/release grant after execution stops, keeps it in memory, publishes the
stopped source and consumes release; it cannot acquire/renew or resume execution.
Failed or ambiguous recovery retains the local files and execution fence. Clean
handoff stops agents before final export and releases only after the mirror and
bundles are durable. A fresh publication retains its account fence for ten seconds; release waits at most fifteen seconds and retries only `mirror_commit_in_progress` while the same server-confirmed holder, epoch and live lease remain valid. An unstarted structured Claude chat with no native transcript is omitted only when a complete bounded startup-only journal, fresh-spawn recipe, and no submitted input or background work prove it empty; snapshots leave its source live, while clean handoff atomically fences input and durably suspends it for local return. Every snapshot publishes the account's continuation policy (`/v1/baton/{w}/policy`: `handoff_enabled`/`offline_takeover` = not hours-exhausted, `has_agents` = an agent was archived) in both protocol versions, while the epoch is owned and before any bytes are pushed; only exported agents enable automatic worker wake. Missing meaningful or ambiguous history still fails the flush and retains local ownership.

Configuration export keeps portable settings and MCP definitions, including
validated `bearer_token_env_var` names, while excluding credential values and
positively device-bound app helpers, outside-project home paths and loopback
services. Paths inside the copied project remain portable. Environment omission
diagnostics come only from declared environment maps in active config files;
redacted plugin metadata and desktop runtime variables are not requirements.
These diagnostics mean “not copied”, not “confirmed missing on the destination”.
Import preserves cloud-local credentials. A same-name MCP with a different
URL, process, arguments, credential reference or supplied environment/header
configuration preserves the entire destination connection rather than retargeting
its credentials. Other preferences merge for matching connections. Conflicts do
not yet have a user-facing report; imported settings must not be described as
having replaced every existing connection.

Project save/upload siblings use the reserved `.chimaera-staging-` prefix. Snapshot policy excludes that namespace even when tracked or explicitly included by an ignore file, so an unfinished body cannot enter a canonical snapshot before its final guarded rename. Ordinary user temporary files keep their existing policy.

No account refresh token or agent credential enters this module. Delegations and
short-lived Git passwords are memory-only. Never log remote response bodies,
credential helpers, or secret-bearing structs. Filesystem work runs off the
reactor. Every directory walk, child output, transfer, queue and state map is
bounded. Shadow commits never touch the user's index or branch. Hand-back never
resets a dirty worktree or rewrites a divergent branch.

Hand-back fetches never overwrite `FETCH_HEAD`. Active-branch fast-forward holds
the real index reservation and a prepared Git ref transaction before touching
the working tree. Its bounded finalizer survives caller cancellation and installs
the matching index after a committed ref; ambiguous failures retain the prepared
index. Prepared transactions serialize so their helper cannot deadlock on the
two-child transport budget. Git selection is probed once asynchronously with
credential-free, output-capped two-second helpers. The bounded 30-second wait for
a helper slot is retryable and never caches a transient capacity failure. Account and keeper requests use their own six-request budget, never the two Git helper slots. On macOS only, an older or
unknown PATH Git falls back to `/usr/bin/git` if that binary reports at least
2.45 (the upstream curl POST-size reuse fix); modern PATH Git and other platforms
keep their existing selection. This affects only mirror helpers, not ordinary
workspace Git settings. Failed HTTP transfers with an older/unknown selected
Git give static upgrade guidance without exposing stderr. Failed helpers emit only fixed diagnostic categories and a fixed operation name; stderr, URLs, paths and credentials never enter logs. There is no enlarged
POST buffer, automatic failed-push replay, or weakened publication check. The
one retried failure is a busy mirror: its 503 (with `Retry-After`) admits
nothing, so a fetch or push is repeated up to three times after the documented
10 s, growing per attempt, plus jitter (Git hides the header itself).
Another worktree's branch is retained separately. Unsupported
transaction support preserves a cloud ref instead. Network Git has a finite
16-minute deadline; ordinary helpers retain short deadlines. The remote
repository and shadow histories are quota-bound by the account; local shadow
history is retained and not yet pruned; neither is silently rewritten.

Cloud discovery is independent of power state and the obsolete global projects
folder. `GET /api/v1/pro/projects` returns `{projects,error}`; each row has
`workspace_id`, `name`, `host_id`, `host_alias`, `local_root`, `available`, and
`error`. Refreshes are serialized, cached for 30 seconds, bounded to ten seconds,
eight workers and 128 rows. They use ordinary cached worker GETs: no wake intent,
mkdir, Git fetch, workspace registration or baton mutation. The worker's explicit
`cloud_internal` setup-workspace marker excludes provider-login scratch projects
from both discovery and automatic mirroring.

`POST /api/v1/pro/projects/open` accepts `{workspace_id,destination_root?,expected_account_id,expected_endpoint}` and
returns `{workspace_id,root,name}`. The native shell supplies the chosen final
folder; webview arguments contain only a workspace ID. A fresh folder must
already exist, be writable and empty, and lie outside another project/repository.
The selection is checked before cloud hand-back and immediately before install.
Recorded directory identity prevents missing/replaced folders from being silently
recreated. The local configure request carries `account_id`: it is required
whenever execution is negotiated (every current device and worker
configuration, `execution::validate_configuration`); only a legacy v1 configure
may omit it. Each new adoption binds its
folder to the account endpoint and account ID, so signing into another account
cannot reuse a colliding project's local path. Discovery/configuration snapshots
pair runtime and generation under the configuration lock. Unstarted failed choices can be replaced explicitly; started imports
remain pinned to their saved folder. Old `import_roots` entries migrate only to
pending-ID fences, never to permission to import. A partially registered legacy
project needs explicit selection of its original folder before recovery.

Normal lazy return only handles registered projects without a pending adoption.
Moving live cloud work waits for the settle gate (awake on power for five
minutes) in both protocol versions. Work the cloud is not running returns at
once: a cloud release (holder none), a lapsed cloud lease (the device takes it
from the last acknowledged checkpoint; the account's reconnect grace answers
409 `takeover_grace`, treated as a quiet wait), or this device's own unfinished
return (`Hydrating` held by it), with backoff from two minutes doubling to thirty.
A bare 409 from the worker's handoff is final for the pass unless the worker woke
into a new epoch. A cloud machine asleep with ownership (placement
`suspended`) reads expired too, but the account refuses anyone else's acquire
(409 `held`): the device reads placement (passive) and, once settled, wakes it
by POSTing its `/pro/handoff` through the keeper with `X-Chimaera-Wake:
interaction`; it never fetches or acquires from under it. The woken worker's
handoff waits (≤20 s) for its own lease renewal first. Re-acquiring the epoch this device itself held
(`execution::held_here`: its clean release or its lapsed lease) skips
hydration and never forks; a worker renewal after a same-epoch fence resumes
what the fence preserved.
A return attempt rechecks ownership immediately after a worker wakes, rather than
waiting for the next mirror pass. The verified source epoch is persisted before
release; ambiguous responses are resolved by reading ownership, without repeating
that epoch's request. Handoff responses have a 90-second transport budget within
a 105-second preparation deadline. Generic ownership polling leaves a previously
remote, now-unowned project fenced until hydration installs its current history.
Worker polling also fences unowned projects whose intervening device tenure was
missed during sleep. A durable worker restart shortcut requires a freshly renewed
matching owner and epoch, including the HTTP entry point; a busy-job no-op or changed owner cannot skip import.
Negotiated checkpoint execution retains its separate canonical hydration path.
Existing laptop projects retain their original roots. Worker hand-back and explicit
adoption compare the raw account holder against the typed keeper worker identity
(`worker-{holder}`), while requests retain the complete route ID. A real-Git
regression returns a completed synthetic conversation through that route, retaining
its session/native identity and history without starting a new model turn. Hydration checks account
generation at ownership, filesystem and session-install boundaries; signing out
cannot finish an old transfer as a fresh local ownership grant. HTTP transfer is
bounded to nineteen minutes. `/pro/handoff` and `/pro/hydrate` run as owned
tasks (`detached.rs`), so a caller that gives up never cancels them; a daemon
crash can still leave a persisted Hydrating fence and partial files, and a
retry resumes at the saved destination. Snapshot and hydration first clear
interrupted-helper leftovers (ref/index/packed-refs locks, `index-*`, `tmp_*`
packs, `stage-*`/`hydrate-*` copies) under the project's cache guard, and the
daemon sweeps them per project at start. Before a snapshot builds on the
outgoing shadow, each existing tip must name a readable tree; on real damage the
shadow is set aside (`working-tree.damaged`, one slot) and rebuilt from the
published remote. No worker
project is adopted merely because this daemon starts or becomes suitable for work.

Required worker setup runs before any imported agent resumes. Persisted
`SettingUp` ownership fences ordinary writers and ledger restore while its
explicit daemon setup task alone runs the setup command (a background login-shell
child in the project root, 10-minute bound, output tail in `<pro root>/<ws>/setup.log`;
no terminal session). Only the user-confirmed `setup_command` runs; an agent's
proposal (`pending_setup_command`) never does. Failure (`cloud_setup_failed`)
keeps that fence and exposes an attention error; a hydrate retry runs the updated
setup against already installed files. Laptop-only deferred steps stay in the
profile as instructions for the returning agent under its usual permissions;
the daemon never replays those commands automatically.

Cloud resume additionally checks the providers named by actual deferred ledger
agents after setup, using fresh bounded worker-local readiness probes, per
session: a provider must be installed and signed in to resume its own sessions;
another provider's login, unknown provider ids, timeouts and missing evidence
cannot satisfy it. A provider that is not ready holds back only its sessions
(`provider_gate::waits_for_provider`): the project becomes `Local` and every
other session, terminal and idle conversation resumes; the waiting sessions
stay paused rows with an additive `blocked_provider` naming it. The local status
row exposes additive `blocked_providers: [{id,state,reason}]` and its mirror
error is `cloud_provider_not_ready`; the same bounded rows feed cloud-provider
onboarding (`handoffs`, now also for a `Local` project, with its epoch) and
session-scoped MCP guidance without probes. Nonsecret blocked rows survive a
daemon restart beside a persisted `SettingUp` or restart-verification fence;
cached readiness never grants permission to resume. After sign-in the page's
`POST /pro/hydrate {workspace_id, expected_epoch}` re-checks (fresh) and
resumes the now-ready sessions (`provider_gate::resume_ready`); nothing is
fetched or reinstalled. One session failing to resume never stops the others.
Live cloud work moves home only after this computer has been awake on power
for five minutes (`lazy_handback`); a development build may shorten that with
`CHIMAERA_PRO_SETTLE_SECS` for the loopback harness, release builds ignore it.
A worker asked to hydrate the epoch it already verifiably holds (same holder,
same epoch, managed or not) only re-verifies it with the account: its running
agents are not stopped and nothing is reinstalled; a managed project with
uncertain or unproven old processes still installs the checkpoint.
For a project still in `SettingUp`, an explicit hydrate retry against the
recorded epoch reuses staged
files and repeats setup/readiness without fetching another snapshot. It never
starts authentication or transfers provider credentials. Account replacement,
ownership changes and cancellation retain the fence. Personal-device and
ordinary SSH/free workspace behavior is unchanged.

Returns merge three ways against `published_tree`, the working-tree commit of
the last acknowledged publication (advanced to the installed tree after a
return); a file only one side changed takes that side; if both changed, the
incoming version takes the path and the user's own version is kept right beside
it as `<name>.mine-<yyyymmdd-hhmm>` (a name the mirror never publishes), counted
in the mirror row's additive `kept_both`, with `kept_paths` naming those copies.
`report_return` records it (persisted in `state.json`'s `kept_both`, a 64 KiB
share: counts always, names while they fit) and raises one `kept_both` notice
per return that kept anything (`notices::push_kept_both`). The return's kept
`@cloud` branches reach that notice only once `engine::hydrate_scoped` passes
`repository::receive`'s result to `report_return` (today it calls
`return_report`, files only).
A baseline file absent from the incoming snapshot is deleted only when the
manifest's additive `left_out` inventory (≤4096 paths the sender omitted by
policy, size, symlink, credential content or `.chimaeraignore`) is present and
does not list it. Fast-forwards set identical untracked files aside and keep
the cloud branch separate on a differing one; only `refs/heads` get `@cloud`
copies.

The sleep flush (`/pro/sleep {deadline_ms?}`) preempts the periodic pass, flushes
projects in parallel as owned tasks (live agents first), releases within the
remaining deadline only, and reports `pending` flushes that continue after it
answers. A sleep flush that did not hand over (release out of time, or a failed
publication) marks the project `release_pending`: the lease loop leaves it alone
(no renewal, no resume) so the lease lapses. `/pro/wake` advances
`sleep_generation` and turns every `Transferring` project into
`AwaitingVerification` (writable on a device) at once; a running flush then
stops no further sessions, keeps its publication, skips release and resumes the
sessions it stopped; a `release_pending` project resumes its deferred sessions
locally, without the account. Sign-out does the same for `Transferring`, and on
a device also for its own `Hydrating`/`SettingUp` return and for every project
whose `Local` ownership it drops that still holds deferred sessions (a
return's resume runs as its own task, so aborting the mirror task never cuts
it; `ledger::resume_one` gives each session one resumer at a time). At boot a
returned session still deferred on a device (`interrupted_return`) waits like
a restart-deferred one instead of answering "moved" forever. A device's own
unfinished return retries after 15 s, doubling to two minutes. Failures and
refusals carry stable codes (`routes::error_code`; mirror row `error_code`).

Structured pause checks accept authoritative completed-turn/idle agent state even when a provider emits no textual idle status, but reject queued input, active turns, and background work (explicit permission/action waits remain safe pause points). Terminal agents (`agent_state::tui_at_pause`): Claude hook states decide (idle, finished, needs permission, errored and rate limited are pauses; running is not); a Codex TUI, which has no hook state, is at a pause once its authenticated `agent-turn-complete` notify arrived with no output after it, or once its terminal has been quiet for 10 s with the agent itself in the foreground.

The distinct authenticated `POST /api/v1/pro/configure/workspace` accepts only a
worker delegation bound to one workspace/revision, explicit account identity,
no keeper URL and exactly baton/mirror scopes. Its versioned nonsecret ack and
status field must match the supervisor's registered root before enabling work.
Legacy configure never accepts such a binding. The accepted origin/account/root
and directory identity survive disconnect/restart; invalid records fail closed.
Pro operations reject foreign workspace IDs before filesystem/network work and
hydrate only into the accepted root. Renew cannot drop/change the binding or
holder or add keeper scope. This does not replace service-side authorization,
project namespaces, or project-bound routing of generic daemon/MCP APIs. Those
remain prerequisites to enabling selected-project secrets.

Snapshot failures emit only a fixed operation phase, fixed error category and clean/snapshot boolean, including failures recovered by resuming an idle session. Recovery does not turn that failure into a successful handoff; response and retry behavior are unchanged.

Mirror helpers require known Git 2.36 or newer and explicitly harden committed objects, refs and pack metadata with full fsync. A failed local shadow fetch only becomes eligible for repair when strict object validation confirms a missing or damaged object in the existing shadow; healthy divergence and ordinary transport failures retain their failure. Incoming and replacement object graphs must pass complete strict Git validation. The previous working-tree baseline is extracted before replacement. Both replacement and preserved evidence files are boundedly fsynced before the final authority check and swap. The entire damaged repository, including unpublished refs, reflogs and objects, is retained in one fixed same-volume quarantine; a second replacement cannot overwrite it. An interrupted swap with a missing current shadow uses only the preserved previous repository as its local baseline and rebuilds from the newly authenticated incoming snapshot. Per-workspace owned cache guards serialize snapshot, preflight fetch and hydration without blocking other projects; weak registry entries retain the same mutex while detached cleanup holds it. Managed Git process-group cleanup retains the guard and child capacity until observed quiescence; unknown cleanup keeps only that workspace unavailable for the daemon lifetime. The blocking rebuild cleanup and finalizer retain the guard across caller cancellation, and final replacement also holds configuration exclusion while checking exact ownership and synchronizing directories. No user repository ref or forced publication is involved.
