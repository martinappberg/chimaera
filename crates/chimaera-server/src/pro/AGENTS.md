# Optional mirrors and workspace ownership

This module retains daemon-side mirror and handoff authority, effects and recovery. Optional private runtime policy uses the same captured owners. Parent:
[server map](../../AGENTS.md). It is inert until the native app provides a scoped,
revocable delegation over the authenticated local API.

| File | Responsibility |
| --- | --- |
| `mod.rs` | Bounded, credential-free persistent state and ownership/import fences; `synced` says whether a project's agents get the where-you-run note (`mcp/cloud_context.rs`). |
| `authority.rs` / `authority_tests.rs` | Fixed-identity workspace-bound worker acceptance, startup-only validated revision advancement, credential-free persisted latch, renewal/route/root guards and synthetic side-effect regressions. |
| `routes.rs` | Authenticated configure/status/privacy/profile/power/hydration HTTP handlers. Profile GET returns an account-generation-bound ETag; PUT optionally checks one exact If-Match under the same preference lock as replacement (412 on change). Older unconditional PUT remains supported. Accepted writes retain configuration/job reservations through durable persistence even if the caller disconnects. `profile_tests.rs` covers stale confirmation, generation changes and disk failure. `/pro/status` rows carry additive `parked`, `leave`, `working_agents` and `cloud_handoff`, and the top level `leaving` (see Leaving below). `hand_over` is the one flush coordinator behind `/pro/sleep` and leaving. |
| `projects.rs` / `projects/catalog.rs` | Passive published-account discovery (negotiated `/v2/projects`, at most 128 rows/pages; legacy capability absence or 404 falls back to passive worker discovery), explicit copy/takeover routes, native-picked folder validation and inode/account-bound retry. Catalog rows infer no host or execution authority; errors retain cached rows and destination bindings. Legacy `/open` refuses rather than transferring execution. Nine original shared guard cases remain public; five actual runtime project compositions live privately, including four original ignored companion integrations run by the required private companion job. |
| `project_copy.rs` / `project_copy/tests.rs` | Immutable read-only checkpoint copies with the existing file/Git transaction, independent durable copy enrollment, exact pending baselines, counted admission and explicit post-commit role promotion. Copy selects its receipt through passive `/v2/baton` GET; the legacy v1 response has no checkpoint and is never a fallback. Missing negotiated receipt refuses enrollment/install. No agent/session/configuration restore or copied-edit publication. |
| `projects/tests.rs` | Nine shared destination/account/cache, refusal and legacy recovery tests. Paid real Git copy/return and worker roundtrip compositions live with the optional private runtime. |
| `execution.rs` / `execution/` | Negotiated execution leases, independent stop watchdog, durable launch/crash evidence, immutable receipts and stopped same-installation recovery. |
| `execution/supervisor.rs` / `supervisor_tests.rs` | Optional Linux startup-only cleanup receipt from a trusted fixed launcher, exact account/project/root/revision/boot binding, persisted launch generation and authenticated acknowledgment. The superseded namespace-idle descriptor/channel is retired; retained provider startup protections remain separate. Cleanup clears process uncertainty only; missing policy and execution authority remain fenced. |
| `execution/provider_protection.rs` / `provider_protection_tests.rs` | Retained fixed provider-launch process protections: matching nonzero UID/GID, no supplementary groups or retained capabilities, inherited no-new-privileges and positively checked dumpable-zero. The original process and descriptor-exclusion tests remain explicit Linux gates. No namespace-idle channel. |
| `execution/maintenance_store.rs` / `legacy_parking_fixture.rs` | Bounded historical before-image reader restores exact native chat identity as manual-only on boot; unknown/malformed records fence automatic restore. Legacy disk tests retain the original capped writer only under tests. Active custom-secret maintenance uses shared WorkspaceHost; the former inherited namespace-idle actor is retired. |
| `execution/installer.rs` / `installer_tests.rs` | Exact pre-wait dispatch and shared setup-group/durable pending admission for owned noninteractive and legacy PTY installers. Final spawn and cleanup stay counted, authority loss fences promptly, and incomplete crash evidence never relaunches an installer. |
| `execution/provider_startup.rs` / `provider_startup_tests.rs` / `provider_ready.rs` / `provider_ready_tests.rs` | Optional protected one-shot provider startup pipe: distinct bounded read-only FIFO, zeroizing4096-byte payload plus EOF within the original cleanup deadline, exact launch correlation and pre-child closure. After the first successful real Configure, one owned Ready continuation uses the fixed Unix socket and its original five-second budget; only an exactly correlated Inactive can retry the same request. Ordinary polls and same-identity refresh never reset it. Replacement/disconnect/drain/shutdown retire authority before waiting for actual IO cleanup; work stays counted until closure. Staged/Checking/Verified/Closed all retain the execution/restore fence: provider consumers and production enablement remain absent. Synthetic Configure/socket fixtures do not prove protected Linux startup or the private supervisor service. |
| `execution/provider_fixture_host.rs` / `provider_test_fixture.rs` | Explicit nondefault protected fixture context and actual Configure/Ready test owner. Context pins original Pending, exposes no AppState/credential/socket setter, and refuses replacement before admission. Private daemon owns the fixed vendor consumers and diagnostic receipt policy. Public startup/refusal/current/root/quota/actual process cleanup guards remain loaded independently; normal execution remains fenced. |
| `execution/mutation.rs` | Bounded file/lifecycle/command commit reservations; account/epoch admission uses short in-memory locks, while clean stop and replacement wait for actual work even if its HTTP caller disappears. Reserved launches fail promptly if configuration is draining them, rather than waiting on themselves. `Dispatch` captures exact ownership and this project's account participation (including accepted scoped authority) before asynchronous communication reads, then reserves the final actor enqueue. Only participating projects pin account generation: an unrelated Configure/sign-out cannot cancel a free local installer, while first enrollment, ownership/copy-only transitions and managed account replacement still fence; worker proofs remain mandatory, while a device's expired publication lease does not block its own local work. Public bundle recovery also fences `may_write`/`may_execute`; `ImportGuard::into_resume` releases only configuration and transfers its counted original workspace/account generation into a sealed `ResumeGuard`; import recovery remains strict even for an otherwise free project, with task-local import identity checked around ordinary admission at actual PTY/chat registration. |
| `companion.rs` | Host-only compatible artifact capture: resolve the original trusted user data anchor once, pin fixed no-follow installed directories, validate closed schema1/version/build/OS+CPU/wire/length/digest metadata and bounded actual executable bytes. Only wholly absent managed selection permits the fixed packaged layout; malformed or ambiguous selected installation refuses. Capture uses the same two retained helper/cache slots and original ten-second admission budget, and observer loss cannot release unsettled blocking work. The opaque non-Clone image is consumed by original config dispatch without pathname recapture; it carries no workspace authority. Resolver/cancellation and pre-stop regressions cover compatible-image admission before disruptive effects. |
| `engine.rs` / `engine/coordinator_host.rs` | Public authority/effect host for snapshot, reconciliation, staged hydration and live return. Private runtime owns coordinator timing, snapshot/reconcile policy and move/release loops over the same captured generation and ProjectOwner. Public `engine::start` retains the spawned task. Without a runtime no coordinator/account loop starts; durable enrollment, ownership and recovery still load. |
| `install.rs` / `install/tests.rs` | Durable, account/epoch/checkpoint-bound return file intents: private before/after blobs, descriptor-relative no-follow replacement, exact restart roll-forward, checkout invariants and refusal to overwrite newer user edits. Planning opens enumerated images nonblocking beneath pinned roots; a missing or nonregular image refuses rather than silently changing the write intent. |
| `detached.rs` | Owned transfer tasks keyed by (kind, project, epoch): a caller that disconnects never cancels a flush or hydration; repeats join; a completed release is remembered ten minutes. |
| `drain.rs` | `POST/DELETE /pro/drain`: refuse new transfer work and wait for jobs, transfer tasks, project caches and Git helpers before a cloud machine suspends; on a cloud machine it first publishes each project it holds (the copy a computer takes when a phone acts while it sleeps). |
| `moves.rs` / `engine/project_host/move_host.rs` | Single bounded pull/answered/acted/yielding/leaving stores, original task admission/outcome cleanup, explicit Take over and exact captured fixed move effects. Private optional policy owns the original pull/read-retry/hydrate and pause/last-actor handover loops over the same ProjectOwner; original generation flows through request and hydration, and the same yield tuple retires only after completion. No-runtime pull refuses and releases its original request. Phone90s/computer300s selection, renewal target/dedup selection and passive readiness query remain public paid behavior alongside shared custody; this is a bounded reduction, not a claim that the whole family is free. Original older-wire test stays public; private move tests cover original policy, observed epoch and replacement before mutation. |
| `continuity_tests.rs` | Eight retained original free/admission cases: hidden workspace eligibility, boot shells, sign-out recovery, drain custody/refusal, stable failure codes, ended-return presentation, thaw admission and release settle bounds. Shared loopback fixtures remain for original public custody tests. The 24 original paid continuity journeys now live in private `pro-daemon-runtime` under `orchestration::continuity_tests`, using the actual optional Runtime and original host effects; five original companion-prerequisite annotations remain and require exact execution with a normally installed compatible helper. The required companion job runs the complete private runtime suite and every historical companion selector; migration adds no ignores. |
| `snapshot_diagnostics.rs` | Fixed snapshot failure categories; no response bodies, paths, identifiers or error text enter diagnostic logs. |
| `handback.rs` | Forwards the original captured project owner to optional private return policy; original live authority and cleanup stay host-owned. |
| `release.rs` | Shared `UpgradeRequired` classification for the account publication fence. A legacy 409 `continuity_upgrade_required` switches the next reconcile to v2 and presents `checkpoint_pending`; the same refusal to a v2 request is an ordinary failure. The bounded release/retry loop lives in the optional private runtime. |
| `provider_gate.rs` / `provider_tests.rs` | Shared per-agent readiness and bounded blocked-provider status/refusal tests. Actual paid staged retry/cancellation journeys use the optional private runtime with a synthetic CLI and real PTY. |
| `protocol.rs` | Additive account contract subset and strict worker host-to-holder identity translation; intentionally no link/TLS dependency in the daemon. |
| `transfer_dispatch.rs`, `transfer_types.rs`, `transfer_host.rs` | Eighteen typed original repository operations, immutable existing DTOs and one original source/cache/generation owner. Trusted private policy can use only fixed Git with captured roots/clean environment/sealed original mirror grants. The original daemon data/home cache anchor resolves once, while fixed Pro/project suffixes remain no-follow; cache descriptions bind to that pinned canonical inode, never a later alias. Real filesystem checks run in retained blocking work; staged output and checkout lock cleanup retain the same cache exclusion through cancellation. Nondefault fixture constructors use caller-owned disposable roots and real host Git/install effects. The original paid engine/projects suites live in the private actual runtime; the full assembly and installed-companion gates verify this boundary. Authoritative configured/ownership fields retain their meanings; affected rows use the existing fixed `optional_runtime_unavailable` error, and executable cloud handoff is false without the runtime. |
| `transport.rs` | Bounded external curl/git children; cached mirror-only Git compatibility selection; credentials only in memory, never argv or Git config. The account's 403 `{"error":"return_window_ended"}` (a plan that ended and whose time to bring cloud work home has passed) becomes an error of its own in `engine::account`, so its mirror-row and open `error_code` read `return_window_ended`; any other 403 stays a plain response. |
| `policy.rs` | Mirrored-path policy (credentials, `.git`, staging names, kept copies and a folder's `.chimaera-workspace` identity marker at any depth are never mirrored), `REBUILT_DIRS` (dependency and cache folders that never travel as untracked content, used by the mirror inventory and the agent-config export), credential filtering, size budgets and the cloud profile (a user-confirmed setup command, an agent's proposal, and the environment variable names the last configuration export left out). |
| `mirror.rs` | Public pinned snapshot confinement and interrupted-cache cleanup remain single-sourced; paid Git inventory/commit/fetch/push/validation policy dispatches to private `pro-daemon-runtime`. No-runtime transfers refuse before mirror initialization or managed stops. |
| `shadow_cache.rs` | Validated reconstruction of an objectively damaged outgoing shadow, retaining its complete prior store in a bounded no-overwrite quarantine. |
| `repository.rs` | Thin original portable-repository dispatch plus free cloud-branch recovery and live install-root discovery. Original remote/config/ref allowlists and staged checkout/index/ref-transaction policy and tests live privately. The public host retains actual live transaction/CAS authority. |
| `repository/staging.rs` | Original immutable staging descriptor and fixed private-policy dispatch. Bounded codec/blob/index/merge validation and original test cases live privately; the wire shape and public live-file install validation remain unchanged. Service-backed SHA-256 remains unsupported. |
| `canonical.rs` | Keeps the user's own version of a conflicting file right beside it (`<name>.mine-<yyyymmdd-hhmm>`) when a return installs the incoming one; bounded per return, never overwrites an earlier copy, never mirrored. It captures a nonblocking regular-file descriptor before copying and creates the sibling beneath the same pinned parent. Unicode lookalike names are parsed without byte-boundary panics. `original_name` maps a copy back to the file it sits beside. |
| `kept.rs` / `kept/tests.rs` | The review of what a return kept in both versions: recorded pairs and live `@cloud` branches, both texts and choices (`use_mine` / `use_cloud` / `keep_both`, one or all). Every root/child is opened `O_NOFOLLOW`; choices pin the root inode and retain counted ownership/configuration/cache reservations in an owned task through file effects and persistence. Per-choice authority checks refuse a changed account/epoch/root. Ambiguous shortened basenames set `can_use_mine:false` and never replace a prefix neighbor. Router, cancellation, changed-authority and long-name regressions cover these seams. |
| `trash.rs` | Where a discarded kept copy goes: renamed (through its folder's descriptor, never replacing a name) into the home Trash (`~/.Trash`; the freedesktop.org home trash on Linux, with its `.trashinfo`), else its drive's existing Trash (`.Trashes/<uid>`; `.Trash/<uid>`, `.Trash-<uid>`), else deleted. A filesystem without exclusive rename refuses and retains the copy, never falling back to a racy replacing rename or deletion. `ProState::trash` holds the home Trash; tests point it at a fixture (never the real one). |
| `config.rs`, `config_wire.rs`, `config_exec.rs` | Public transaction host and fixed v1 companion contract for portable configuration. Explicit Pro transfer alone preflights a compatible installed image after original destination/credential admission and before mirror initialization, ownership changes or managed stops; hydration preserves existing no-op/recovery shortcuts. The same original image reaches Export/MergeStaged. Before/after snapshots, counted mutation/cache ownership, positive child/group cleanup, bounded actual staged-path/file/credential validation and generic install/recovery remain public. Private file selection and JSON/TOML/MCP/Git sanitizing policy remain in the companion; merge sees only staged images, never live home. Linux executes the captured descriptor; macOS copies at most128MiB into an exclusive0700 stage/0500 leaf retained through positive settlement. Admission/preparation/work share one ten-second deadline; uncertain cleanup retains evidence/cache quarantine. Required gates cover compatible metadata, original captured-image/root replacement, unchanged live home, cancellation and all companion-dependent continuity/project journeys through normal managed installation. Linux delivery and automatic installer/packaging require their own platform acceptance; local composition does not certify them. |

One recorded holder and epoch controls shared writes. **Laptop first (D1):** a
personal computer is fenced only by a verified other owner (`Ownership::Remote`,
from an authenticated read) or its own in-progress transfer (`Transferring`,
`Hydrating`, `SettingUp`); `AwaitingVerification` stays writable there. Lease
expiry, account unreachability, sign-out (`disconnect` never stops sessions),
plan changes, the privacy switch and daemon restarts stop publication only. A
verified other owner refuses input at once (`may_write`); the device's agents
then stop at their next safe pause, bounded to five minutes, as an owned task.
The saved state has a 1 MiB input bound. Loading preserves every existing
ownership, preference, destination adoption, legacy import and parked-transfer
fence within that bound; the 128-project runtime admission ceiling never
authorizes forgetting an older account or privacy restriction.
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
evidence and a pending intent before spawning. Registration gives that intent
to an owned bounded writer; it settles only after the driver's group or death
is observable and no other pending/abandoned intent remains. The intent captures
account generation, exact ownership and a per-workspace launch revision; final
spawn checks all three under counted admission. Stop invalidates that revision
even when the same epoch is reacquired, and late receipts/drops cannot settle a
newer launch. Receipt persistence remains counted until its write completes.
Every state write
records the live managed agents' process groups with their leaders'
start times (≤64 per project). A larger live roster records an explicit overflow marker; the recorded
prefix never proves the omitted work stopped. Same-boot overflow stays unknown
through subsequent truncated writes and reprobes; a cold boot or verified
supervisor cleanup is required to establish complete evidence. Persisted worker
identity keeps this fence even if a later runtime configuration says Device.
Clean transfer admits at most 64 sessions before any mutation and rechecks the
roster after draining all agents. Truth predicates inspect the whole existing
registry. The final synchronous agent spawn/registration window has a counted
reservation even for legacy/device ownership, so a pre-fence launch cannot
appear after a successful empty-workload check. Plain shells remain unaffected.
Cloud setup shells and both managed installer paths are also counted execution:
their groups and background descendants are fenced on authority loss and observed before stop completes.
Installer admission (`execution/installer.rs`) retains the exact pre-detection
`Dispatch` through shared install-lock waits, final spawn and actual detached
cleanup. Free unconfigured installs create no Pro state. Installers use the setup
group registry and launch-pending marker, so lease fencing, transfer and restart
cannot confuse an installer with an idle workspace. Caller loss cannot release a
cleanup reservation while its group is unreaped; bounded admission capacity and a
200 ms cleanup floor retain unknown groups until positive absence. A restored
pending marker never relaunches an installer.
A shared synced pending marker precedes agent/setup spawn, survives cancellation, and makes
same-boot restart evidence unknown until group cleanup is durably settled.
A graceful stop clears the ordinary agent evidence once they
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

Delegations, account refresh tokens and short-lived Git passwords are never
persisted by the transfer protocol. Private return before-images may contain
the destination's original settings; they stay under owner-only recovery
directories, never enter a mirror, and are removed only after commit. Never log remote response bodies,
credential helpers, or secret-bearing structs. Filesystem work runs off the
reactor. Every directory walk, child output, transfer, queue and state map is
bounded. Shadow commits never touch the user's index or branch. Hand-back never
resets a dirty worktree or rewrites a divergent branch.

Hand-back fetches never overwrite `FETCH_HEAD`. Repository adoption runs on a
bounded private checkout, retaining the original index and Git objects (including
staged-only blobs). Active-branch fast-forward there holds its index reservation
and a prepared Git ref transaction; the bounded finalizer survives cancellation
and installs the matching staged index after a committed ref. The live checkout
is changed only by the return journal, with HEAD/index invariants and preserved
before-images. Prepared Git transactions serialize so their helper cannot deadlock on the
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

**Recoverable installation.** `return-stage` retains the validated checkpoint,
three-way/kept-both plan, sanitized config merges and immutable per-session
preparation metadata. `return-install/journal.json` binds all destination file
intents to the account, workspace, epoch and checkpoint (legacy checkpoints bind
their three Git revisions). A bounded append-only progress log records ready,
applied and committed steps without rewriting the inventory per file. Its private
before/after blobs, stage files and directory enrollment become durable before
the first replacement. Roots retain device/inode identity; every component is
opened without following links. Replacement displaces the expected original to
a reserved sibling and installs with no-replace renames. Exact retries roll
forward; changed files, deleted installed files, replaced roots or missing
session preparation retain recovery data and the `Hydrating` fence. Overlapping
config/session targets require the same captured original; native session import
then supplies the final file. The original common/private Git stores reserve
HEAD, index, config, packed refs and named refs through installation commit.
Crash recovery recognizes only this journal's exact lock markers and removes
owned committed locks before profiles run. File installation, workspace
registration and commit run in counted, configuration-serialized owned tasks,
with authority rechecked immediately before canonical filesystem mutations. The
final reservation survives commit, the exact `Hydrating` → `SettingUp` transition
and durable state persistence; setup's initial transition also holds the
configuration lock and compares the current epoch. Existing
shared workspace/index/view/ledger stores are merged under their usual locks,
never replaced with stale whole-store snapshots; all imports stay deferred.
After those merges are durable, installation commits and `SettingUp` is persisted
before recovery cleanup. Only then do profiles and agents run. Arbitrary profile
command effects are not rolled back: failures remain actionable and fenced.
Staging is capped at 4 GiB, journal before/after data at 1 GiB, files at the mirror
ceiling, path inventories at the existing path ceiling, with available-space
checks before copies. Oversized preparation refuses before changing the project.

Cloud discovery is independent of power state and the obsolete global projects
folder. `GET /api/v1/pro/projects` returns `{projects,error}`; each row has
`workspace_id`, `name`, `host_id`, `host_alias`, `local_root`, `destination_saved`, `available`, and
`error`. `host_id`/`host_alias` are null for account catalog rows: display metadata
never guesses the holder's kind. Refreshes are serialized, cached for 30 seconds,
bounded to ten seconds and 128 rows/pages. Exact `project_catalog:1` capability
selects the account's immutable acknowledged-checkpoint catalog. Hidden/internal
workspaces publish `project.visible:false`; invalid names omit that optional
metadata. Missing legacy capability or initial catalog 404 uses ordinary cached
worker GETs (at most eight workers). A negotiated empty catalog never restores
stale worker rows; any other failure retains remembered rows/destinations. No wake intent,
mkdir, Git fetch, workspace registration or baton mutation. The worker's explicit
`cloud_internal` setup-workspace marker excludes provider-login scratch projects
from both discovery and automatic mirroring.

`POST /api/v1/pro/projects/copy` requires `copy_version:1` beside
`{workspace_id,destination_root?,expected_account_id,expected_endpoint}`. The
acknowledgment repeats `copy_version:1`, `state:local_copy|owned_local`,
`workspace_id`, `root` and `name`; a completed copy also carries its immutable
checkpoint, truthful `git_staging` report and kept-file count. `/projects/open`
returns 426 `copy_upgrade_required`; clients must never fall back to it. Opening
an already owning local project preserves its work without demotion or download.
Copy downloads an account read grant, stages only project files/portable Git and
its identity marker, and changes neither execution owner nor preferred home.
A failed copy retains its exact pending checkpoint and journal for retry. A saved
folder is reused when `destination_saved` is true, even before `local_root` is
ready. Fresh folders must already exist, be writable and empty, outside another
project/repository. No profiles, global agent configuration or sessions restore
on this path. Copy files are never automatically published.

`POST /api/v1/pro/projects/takeover` requires
`{workspace_id,expected_account_id,expected_endpoint,expected_epoch}`. Only a
completed inode/account-bound local copy may request the existing validated
account move. Ordinary chat/terminal input continues at the current owner and
never asks for a move. An owned bounded task records a unique explicit intent,
waits for drain/release, then uses the normal leased hydration. Failed old intents
cannot clear a newer one. Copy enrollment and the old-daemon `legacy_pending`
fence retire only after the file transaction committed, under `ImportGuard`
through durable `SettingUp`; failed role persistence restores both restrictions.
Copied projects never resume, renew/acquire in background, publish, advertise
phone readiness or enter lazy return. A persisted explicit pending takeover may
resume its own interrupted move; ordinary passive owner reads do not admit one.

Workspace list/status rows expose additive `local_copy` when enrolled:
`{state:ready|pending|taking_over|recovery_needed,ready,checkpoint?,owner_epoch?}`.
Ready copy roles retain the latest authenticated Baton epoch for explicit Take
over; passive reads refresh it under account-generation/configuration admission.
List omits it
for ordinary projects; status uses null. `recovery_needed` explicitly means the
independent enrollment survived missing/damaged role metadata. The capped
`copy-authority.json` latch is persisted before role state, survives sign-out and
ordinary state corruption; unreadable enrollment conservatively fences known
registered/Pro projects. An absent latch preserves free behavior. Execution/file
mutation remains restrictive; local-copy file editing needs its separate future
admission, never an execution grant. Error bodies retain stable `error_code`
reason categories (`folder_*`, account/privacy/availability failures); unsupported
copy capability is 426, bounded copy timeout is 504 `timed_out`.

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

**Who execution returns to.** Existing executing projects retain their preferred
installation/lazy-return policy. Opening a project on another computer creates a
copy and neither inserts an opened-here hint nor changes that policy. Explicit
Take over uses `moves::take_over_here`; the holder still learns `move_to`, drains
at a safe pause and may keep its work if its own user acted after the request.
A signed-out computer's live lease is retried only with the account's bounded
`retry_after_ms` hint for that exact holder and epoch. The captured account and
project remain current through the five-minute deadline; lease expiry and the
account's thirty-second reconnect grace both precede hydration.
Phone requests target only eligible executors, never ordinary local copies.
Ordinary viewer input is forwarded to the current owner, including sleeping-owner
wake behavior; `session_proxy` does not submit account moves. The native
conversation identity still remains in the ledger until its first resumed turn.

Normal lazy return only handles registered projects without a pending adoption.
Moving live cloud work waits for the settle gate (the app here for 20 s,
`leave::app_settled`; power plays no part) in both protocol versions, and a
project being taken back (`reclaim`) skips it. Work the cloud is not running returns at
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
setup against already installed files. Nothing refuses or queues a step that
needs the user's computer: the agents' where-you-run note says what the cloud
machine cannot do, and they leave such a step for when the user is back.
Before any agent resumes, hydration records what the move left behind for the
note (`mcp::cloud_context::record_arrival`: the manifest's `left_out` and the
sender's OS/CPU, `source_os`/`source_arch`, additive on the manifest) in
`<data>/pro/<workspace>/arrival.json`, bounded at write time to the 64 KiB its
reader accepts. Which conversations heard which note is `told.json` beside it;
`disconnect` removes both for every project, `privacy` (keep on this computer)
for that project. Only an enrolled project (an ownership record here) counts as
synced (`pro::synced`); `workspace_profile` alone also answers for projects that
never enrolled.

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
onboarding (`handoffs`, now also for a `Local` project, with its epoch)
without probes. Nonsecret blocked rows survive a
daemon restart beside a persisted `SettingUp` or restart-verification fence;
cached readiness never grants permission to resume. After sign-in the page's
`POST /pro/hydrate {workspace_id, expected_epoch}` re-checks (fresh) and
resumes the now-ready sessions (`provider_gate::resume_ready`); nothing is
fetched or reinstalled. One session failing to resume never stops the others.
Live cloud work moves home at its next pause once the app has been here for
the 20 s guard (`lazy_handback`); a development build may set the guard with
`CHIMAERA_PRO_SETTLE_SECS` (up to 300), release builds ignore it.
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
share: counts always, names while they fit, plus when the return happened and
how many files it kept then) and raises one `kept_both` notice per return that
kept anything (`notices::push_kept_both`, which names the return's kept
`@cloud` branches too: `engine::hydrate_scoped` passes `repository::receive`'s
result). A local copy reports its kept files immediately after its file
transaction commits. Unresolved copy conflicts carry into its next copy or
explicit takeover: the combined bounded report is saved in the new transaction's
stage, so retrying the commit cannot count the same files twice. Promotion to
execution keeps that report available for review.

**Reviewing both versions** (`kept.rs`, all authenticated, none reachable
through a browser view's workspace scope):
`GET /pro/projects/{workspace}/kept` answers `{workspace_id, files, total,
returned_at, unlisted, pairs, branches, here}`: `pairs` are the recorded kept
copies still waiting (`{path, mine_path, size, mine_size, changed_at,
mine_changed_at}`, paths project-relative, `path` the cloud's version with
`size`/`changed_at` null when the cloud deleted the file), `unlisted` the kept
copies the return did not name (it names up to 32), `total` the return's own
count (choices never lower it), `branches` the project's
`<branch>@cloud-<12 hex>` refs read live (Git runs only for a project with an
open report or recorded `git_branches`; one merged and deleted drops off),
`here` whether a choice can be made now (`may_write`), `trash` whether a
copy discarded here goes to a Trash (`trash::available`; false: its drive has
none, so it would be deleted).
`GET …/kept/file?mine_path=` returns both versions of one recorded pair
(`mine`, `cloud`: `{size, changed_at, text, binary?, too_large?}`, text up to
512 KiB of UTF-8, `cloud` null when deleted).
`POST …/kept/resolve {mine_path, choice}` settles one recorded pair:
`use_mine` renames the sibling over the file (a deleted file comes back), except
potentially shortened basenames (`pair.can_use_mine:false`), which require manual
recovery rather than inferring a target even when a prefix neighbor exists;
`use_cloud` moves the sibling to the Trash (`trash::discard`: a rename
through the pair's directory descriptor, so the path fences are unchanged;
deleted only where no Trash on its drive takes it), `keep_both` moves
nothing; the answer is the updated listing plus `discarded` (`{trash,
deleted}`: the copies this choice moved to a Trash, and deleted). `POST …/kept/resolve_all {choice}` applies one choice to every
recorded pair; pairs that cannot take it stay and are named in `failed`
(`{mine_path, error_code}`), and when none failed the report ends, unnamed
copies included (they keep their `.mine-…` names). Refusals are
`{error, error_code}`: `unknown_project`, `not_kept` (not a recorded sibling),
`unsafe_path` (a link, a non-plain file, a path out of the project or into
`.git`), `gone` (`use_mine` with the copy no longer there), `not_here`
(another owner, arriving, or leaving), `busy` (the project's cache lock held
past ten seconds), `folder_unavailable`, `failed`. Choices hold the project's
cache and configuration reservations (serialized with mirror passes), update
`kept_both`/`kept_paths`, persist, and mark git status dirty. Listings retain
the same reservations through scanning and settlement, so an older scan cannot
clear a newer return's report. Mutation parents are opened beneath the verified
root descriptor and checked again before effects, rather than reopened from an
untrusted root path. A listing settles recorded copies that are gone or no longer
plain files. The report ends when nothing waits.
A baseline file absent from the incoming snapshot is deleted only when the
manifest's additive `left_out` inventory (≤4096 paths the sender omitted by
policy, size, symlink, credential content or `.chimaeraignore`) is present and
does not list it. Fast-forwards set identical untracked files aside and keep
the cloud branch separate on a differing one; only `refs/heads` get `@cloud`
copies.

The lease loop starts a copy of every eligible project every 120 s (`TIMED_COPY`), and of one project as soon as one of
its agents finishes a turn (`TurnEnds`: fed each 5 s tick from `working_agents`, a working → not working transition marks
the project; copied once 20 s have passed since the last copy started, `TURN_COPY_GAP`; a turn that ends while a copy runs
is copied next; that pass skips `lazy_handback`). One copy task at a time, never while draining. Between copies,
`CoordinatorTick::start_return` runs `lazy_handback` alone, at most every ten seconds, while the app is settled and the
cloud holds a project, or while a project is being taken back.

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

**Leaving (`leave.rs`).** App presence: `ProState.app_since` is set when the
app arrives (`/pro/wake`, or its first `/pro/power` while away) and cleared by
`/pro/leave` and `/pro/sleep`; an arrival also advances `sleep_generation` (an
in-flight leave or sleep flush then parks or releases nothing) and clears the
last absence's outcomes here and at the account (`DELETE
/v2/workspaces/{id}/leave`). `POST /pro/leave` (no body, from the native app on
quit) answers 202 at once and runs everything in one owned task, so a client
that disconnects cancels nothing; the projects a leave or a watch is handling
sit in `ProState.leaving`/`watching`, each held by its task through a drop
guard (no stuck entry). The task records `sleep_generation` at arrival and
`routes::hand_over` takes the next generation only if no wake happened since
(`Handover::since`, else `woke`). For each enrolled project in scope and not
parked: `engine::leaving_agents` names the active kinds; `leave::plan` moves the
project when a Claude or Codex one is active and the account does not say every
such provider is signed out on the cloud machine (`GET /v2/cloud/agents`, written
by that machine's readiness checks, once per change; unknown is tried). Others
stay with a non-clean copy (`nothing_running`, `agent_kind_stays_here`,
`agent_not_connected_in_cloud`); refusals before any flush (`cloud_time_used_up`,
`cloud_unavailable`, `not_synced_yet`) record without a copy. A move carries
`engine::leaving_sessions` (working or waiting Claude/Codex conversations) as
`SnapshotOwner::must_carry`: the private snapshot probes each one in snapshot
mode before stopping anything and refuses with the typed
`ConversationStays::{TooLarge,NotSaved}`. A parked move stays `pending` with the
kinds it stopped that the cloud does not continue (`stopped`); `leave::watch`
then reads the account placement every 5 s (`verdict`): `leave.state` `moved`
(the cloud machine's own report after its resume, `leave::arrived`) records
`moved`; `staying_here` (the cloud machine found every moved conversation
waiting for a provider, the account refused for budget or allowlist, the worker
supervisor gave up) or no taker within `TAKE_BOUND` (4 min), or a cloud holder
silent past 15 min, takes the work back while the app is away
(`leave::take_back`: unpark, `Transferring` → `AwaitingVerification` so the
lease loop re-acquires its own epoch, or `reclaim` so `lazy_handback` brings it
home from the cloud at its pause; `release_pending` resumes locally at once).
A browser that opens a project held here while the app is away
(`Baton::open_in_cloud` in the ownership read, checked by `leave::observed` on
every tick) hands it over the same way when nothing in it works, waits or is
busy (`Sleep::opened`: no conversation needed). `/pro/sleep` records the same
outcomes without account calls (`leave::sleeping`, `leave::slept`). Each outcome
(`ProState.left`, persisted as `left`; `pending` survives a restart only while
parked, and its watch restarts on the next tick; pruned to registered projects,
cleared on sign-out) is one `chimaera_server::pro::leave` info line, the
additive `leave {state, reason?, at, stopped?}` on the status row, and a
best-effort `PUT /v2/workspaces/{id}/leave`. `POST /pro/projects/{id}/here` ("Run here") brings one
project back the same way whatever the app's presence (`leave::bring_back`, plus
`opened_here` so the return applies; 202 `{returning: true}`, 409
`not_elsewhere`, 404); rows carry additive `run_here` (it applies: the project's
work is in the cloud or on its way, `may_run_here`) and `returning` (in
`reclaim` until held here again; `start_return` prunes it). `/pro/status` has an additive
top-level `leaving {ready, reason?}` for a personal computer
(`optional_runtime_unavailable` without the Runtime). A daemon without the
optional Runtime answers leave with `{"leaving":[],"reason":"optional_runtime_unavailable"}`;
a free daemon answers 204.

**Quit handover (`park`).** `/pro/sleep {deadline_ms, park: true,
workspace_ids}` (older apps) and leaving's moves share one flush with three
differences from sleep. Only the listed
projects move (≤128 valid ids, all of them regardless of the time left; a
listed one that is not flushable is reported in `failed` as `unavailable`).
The computer stays awake, but the user chose the cloud: a flush whose copy is
published and whose release the account could not confirm in time takes the
same `release_pending` branch as sleep (it answers handed over, stays parked,
and the cloud continues from that copy once the lease lapses, as after a lost
connection); only a flush whose copy never published recovers at once
(`unpark`, AwaitingVerification, then renew and resume here), and the app says
so. And each flushed project is **parked**
(`ProState.parked`, persisted as a sorted `parked` list in `state.json`) from
the moment its flush starts until such a flush fails (`unpark`), the app returns
(`/pro/wake` clears every park; the app posts it at every launch), the work
is taken back (`leave::take_back`) or the account signs out. While parked, on a device the lease loop
neither renews nor acquires the project (`reconcile_generation`; a flush still
`Transferring` keeps renewing its own lease until it releases) and
`lazy_handback` skips it, however long this computer has been awake on power
and even when the cloud's lease lapsed or it released the project. `park`
checks the flush's `sleep_generation` and inserts under the same lock
`wake_parked` advances it under, so a wake is never followed by a stale park.
At load a parked project keeps its `Transferring` fence (not
AwaitingVerification); a parked entry whose ownership is neither
`Transferring` nor `Remote` (the daemon died mid-flush) is dropped.

Structured pause checks accept authoritative completed-turn/idle agent state even when a provider emits no textual idle status, but reject queued input, active turns, and background work (explicit permission/action waits remain safe pause points). Terminal agents (`agent_state::tui_at_pause`): Claude hook states decide (idle, finished, needs permission, errored and rate limited are pauses; running is not); a Codex TUI, which has no hook state, is at a pause once its authenticated `agent-turn-complete` notify arrived with no output after it, or once its terminal has been quiet for 10 s with the agent itself in the foreground.

The distinct authenticated `POST /api/v1/pro/configure/workspace` accepts only a
worker delegation bound to one workspace/revision, explicit account identity,
no keeper URL and exactly baton/mirror scopes. Its versioned nonsecret ack and
status field must match the supervisor's registered root before enabling work.
Legacy configure never accepts such a binding. The accepted origin/account/root
and directory identity survive disconnect/restart; invalid records fail closed.
Only negotiated `POST /api/v1/pro/configure/execution` with a fresh inherited
supervisor receipt may advance the registration revision, strictly upward, for
that same account/project/origin/root and captured inode.
Preparation and durable application share the exact startup, boot, launch-generation
and quiescence checks; application checks again before consuming the receipt.
A configured runtime, live chat/PTY, outstanding mutation or wrong/replayed receipt
refuses advancement. Ordinary Configure and delegation renewal gain no revision
replacement authority. Cleanup still grants neither a lease nor execution.
Pro operations reject foreign workspace IDs before filesystem/network work and
hydrate only into the accepted root. Renew cannot drop/change the binding or
holder or add keeper scope. This remains scoped delegation validation, not secret delivery or service-side
authorization. Optional environment hooks use the ordinary shared daemon and
per-workspace launch/maintenance guards; these endpoints impose no per-project
namespace requirement.

Snapshot failures emit only a fixed operation phase, fixed error category and clean/snapshot boolean, including failures recovered by resuming an idle session. Recovery does not turn that failure into a successful handoff; response and retry behavior are unchanged.

Mirror helpers require known Git 2.36 or newer and explicitly harden committed objects, refs and pack metadata with full fsync. A failed local shadow fetch only becomes eligible for repair when strict object validation confirms a missing or damaged object in the existing shadow; healthy divergence and ordinary transport failures retain their failure. Incoming and replacement object graphs must pass complete strict Git validation. The previous working-tree baseline is extracted before replacement. Both replacement and preserved evidence files are boundedly fsynced before the final authority check and swap. The entire damaged repository, including unpublished refs, reflogs and objects, is retained in one fixed same-volume quarantine; a second replacement cannot overwrite it. An interrupted swap with a missing current shadow uses only the preserved previous repository as its local baseline and rebuilds from the newly authenticated incoming snapshot. Per-workspace owned cache guards serialize snapshot, preflight fetch and hydration without blocking other projects; weak registry entries retain the same mutex while detached cleanup holds it. Managed Git process-group cleanup retains the guard and child capacity until observed quiescence; unknown cleanup keeps only that workspace unavailable for the daemon lifetime. The blocking rebuild cleanup and finalizer retain the guard across caller cancellation, and final replacement also holds configuration exclusion while checking exact ownership and synchronizing directories. No user repository ref or forced publication is involved.

Owner presentation never infers a peer kind from an opaque holder or the current daemon's role. Refusals carry `owner:null` when unknown; an explicit computer move remains named. Unknown stopped-session destinations use the existing `paused` frame with additive `reason:"elsewhere"`, so older clients stay neutral rather than defaulting a new moved destination to cloud. Local arrival/role and authenticated route aliases retain their known labels.

The optional runtime boundary keeps the exact `ProState` / bounded `DiskState` and one atomic writer public. Private scheduling, snapshot/reconcile, viewer and repository policy retain their original task and operation owners. The runtime receives opaque original generation/configuration handles, bounded host roster and finite effects, never mutable authoritative state maps or renderer-supplied authority. Existing status `configured` retains enrollment semantics. An enrolled/owned row with no more-specific mirror error reports fixed `optional_runtime_unavailable` through existing `mirror.error` / `mirror.error_code`; never-enrolled rows stay neutral. `cloud_handoff` advertises executable handoff only when an optional runtime is present; `configured` still describes enrollment. Kept/canonical/trash and free file recovery are not runtime-gated. Automatic assembly delivery remains a separate acceptance gate.

The owned persistence guard spans state and subsequent execution-latch durable writes, with cache update only after both succeed. The cancellation regression holds the actual blocking write while a successor waits, preventing observer loss from releasing the writer early.

The optional runtime now selects damaged derived shadow history and published baselines. Public `shadow_cache.rs` retains the exact cache/configuration finalizer, bounded no-symlink fsync, original parent descriptor and authority callback before directory publication. A prepared result is non-Clone and bound to the same cache guard/generation; private policy cannot install a different project or mint ownership. Seven original recovery/cancellation assertions and the original published-baseline case run in the private actual Harness; the broader live-install, durable fence and free recovery tests remain public. Private snapshot/reconcile orchestration uses the same captured public effects; automatic delivery remains separate.

The private staged-return planner selects the exact immutable-copy/published-handoff repository baseline and prepares the captured checkout through the existing transfer owner. Public hydration still performs destination admission, before-image capture, generation/lease rechecks, staged tree merge, configuration/session preparation and the original durable live installation transaction. Its remaining account-grant/renewal and lazy-return selection code remains public: it is interleaved with those original authority/store cutpoints, and the rejected separate return-owner adapter increased public code. The bounded planner and move loops use the original captured owner; combined assembly tests cover their composition.

First transfer capture creates only the fixed `pro` cache leaf beneath the already resolved and pinned existing data directory, using descriptor-relative mkdir/open with no-follow and pre/post inode rechecks. It never creates or recaptures the data root. Original cache/generation custody remains in the retained worker; real first-transfer, substituted data-anchor and symlink-leaf cases are covered by public host tests.

The extraction verification gates build the default public daemon without private dependencies and the composed assembly with its actual runtime, then exercise shared absence/refusal/recovery cases and relocated policy journeys. Companion-dependent cases use a normally installed compatible artifact; synthetic peers do not prove vendor login, real HPC, protected startup or signed distribution. Verification receipts belong in the boundary audit rather than this module map.

Initial authenticated hydration reads its checkpoint/credential admission before capturing the existing transfer owner. Repository materialization uses the same cache guard and original configuration generation; the outer cache scope still retains subprocess cleanup. A finite initial-route admission probe catches missing scope and post-read generation replacement without running private repository policy; the composed companion journey remains the actual transfer acceptance gate.
