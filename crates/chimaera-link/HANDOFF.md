# Handoff extension v1

This additive extension to [Link v0](PROTOCOL.md) defines workspace ownership
and scoped Git access. Its independent `baton_version` is 1. Existing Link v0
connections and clients remain compatible. Requests use the account origin,
JSON, and the same device bearer authorization as `/v1/me`.

## Workspace baton

One workspace has one recorded writer. Workspace ids are stable across hosts.
A holder id is either the signed-in device id or a worker id authorized by its
service credential. The server derives the account and holder authority from
that credential; a caller cannot claim another holder by naming it.

| Method and path | JSON request | Result |
| --- | --- | --- |
| `GET /v1/baton/{workspace_id}` | — | Current baton |
| `POST /v1/baton/{workspace_id}/acquire` | `{ "holder_id": "device-id", "expected_epoch": 0 }` | Acquired baton |
| `POST /v1/baton/{workspace_id}/renew` | `{ "holder_id": "device-id", "epoch": 1 }` | Renewed baton |
| `POST /v1/baton/{workspace_id}/release` | `{ "holder_id": "device-id", "epoch": 1 }` | Released baton |

Every success returns:

```json
{
  "workspace_id": "w-12345678",
  "holder_id": "device-id",
  "epoch": 1,
  "expires_at": "2026-09-27T01:01:30Z",
  "server_now": "2026-09-27T01:00:00Z",
  "requires_fork": false
}
```

The initial baton has epoch 0, null holder/expiry, and `requires_fork: false`.
Acquisition compares the required `expected_epoch` atomically. Every ownership
transition on acquisition increments the epoch; acquiring again as the current
unexpired holder is idempotent. A different unexpired holder prevents acquisition.
Release verifies holder and epoch, clears holder/expiry, and preserves the epoch.
The next acquisition increments it. Renew verifies the same holder and epoch and
fails after expiry; it cannot resurrect an expired lease. Leases last 90 seconds;
clients renew every 5 seconds while active. `server_now` makes expiry interpretable
without trusting the client wall clock.

v1 conflicts return HTTP 409 with `{ "error": "stale_epoch", "baton": <current
baton> }` for a mismatched epoch, an occupied baton, a caller that is not the
holder, and an expired lease alike; the returned baton tells them apart. A caller
naming a holder other than its own credential's gets `400 invalid_request`;
invalid JSON/ids also use 400. Epoch arithmetic must reject overflow.
Acquire/release may return 409 `mirror_commit_in_progress` for up to 10 seconds
while a previously verified push atomically publishes its refs. Retry with jitter
and the same expected epoch; renew remains available during that fence. The
loopback fixture uses exactly this vocabulary.

An expired holder remains visible in GET until the next successful acquisition.
Taking over that expired, unreleased baton increments the epoch and sets
`requires_fork: true`: the receiver must preserve the prior branch and create a
separate continuation. A clean release permits the next acquisition without a
fork. The flag remains attached to that ownership epoch, including renews; clean
release clears it. Service unreachability is not evidence of another owner: the
laptop may keep working offline. A later verified newer epoch fences shared
writes and must not silently resume the old cloud agent.

## Mirror credentials

`POST /v1/mirror/credentials` accepts:

```json
{ "workspace_id": "w-12345678", "epoch": 1 }
```

Omitting `epoch` requests read-only credentials. Supplying it requests write
credentials and must match the caller's current, unexpired baton holder and
epoch. Response:

```json
{
  "workspace_id": "w-12345678",
  "repository_url": "https://mirror.example/workspaces/w-12345678/repository.git",
  "working_tree_url": "https://mirror.example/workspaces/w-12345678/working-tree.git",
  "username": "scoped",
  "password": "opaque-short-lived-secret",
  "expires_at": "2026-09-27T01:15:00Z",
  "read_only": false,
  "storage_limit_bytes": 21474836480,
  "max_file_bytes": 100000000
}
```

The two remotes separately retain the user's repository history and shadow
working-tree snapshots; shadow commits never modify the user's branches.
Credentials are scoped to this account, workspace, permission and, for writes,
epoch, with lifetime at most 900 seconds. Git receive-pack revalidates the current
unexpired holder and epoch when accepting a push, so transferred ownership fences
an old token immediately. Read-only credentials cannot execute receive-pack.
Exhausted storage rejects writes without removing existing objects.

URLs contain no userinfo, query secrets or credentials and require HTTPS, except
literal loopback fixture endpoints. Passwords stay in memory and reach Git through
a scoped credential helper, never command-line arguments, persisted Git config or
logs. Redirects must not forward authorization to another origin. Secret-bearing
response bodies and token debug output must be redacted.

## Conformance

Implementations must exercise initial acquisition, CAS conflict, holder identity
binding, renewal, release/reacquisition, expired takeover/fork, and immediate
fencing of mirror writes after ownership moves. Run `link-conformance --handoff` against a compatible test account; add
`--test-hooks` only for the loopback fixture. The fixture implements baton and
credential issuance, with bearer-protected expiry and mirror-write authorization
hooks. Its mirror-write hook tests the fence; it is not a Git object store.
Private Git services additionally test real receive-pack quota and ref updates.

## Daemon delegation

An app can authorize its daemon to keep mirroring after the app exits without
sharing its rotating OAuth refresh token. `POST /v1/delegations` with `{}` and
a full device bearer returns:

```json
{
  "access_token": "opaque-scoped-secret",
  "expires_at": "2026-09-28T01:00:00Z",
  "scope": ["baton", "mirror", "keeper"],
  "device_id": "device-id"
}
```

There is one active delegation per device. Minting a replacement immediately
revokes the previous one. Its lifetime is at most 24 hours, capped at the parent
device's fixed refresh expiry. `POST /v1/delegations/renew` with `{}` and the
delegation bearer returns the same token with a new expiry under the same cap.
Renewal never extends the parent device's authorization lifetime. Daemons renew
hourly with jitter, keep credentials only in memory, and stop authenticated
background work on definitive 401/403. Network failure preserves local work.
The daemon also renews at once (then at most once a minute) after any account
request answered 401. When renewal is refused (401/403) or the delegation has
expired unrenewed, `GET /api/v1/pro/status` answers `configured: false` with an
additive `renewal_failed: true`, and the native app mints and configures a new
delegation; a successful renewal or a new configuration clears it.

The operation-scoped, account-wide token can access only baton, mirror and keeper transport operations,
plus its own renewal. It cannot read `/v1/me`, enumerate or revoke devices, access
billing, start OAuth or mint another delegation. The keeper introspector accepts
it as the same account and original device holder, restricted to the keeper
scope. Parent device revocation and sign-out-everywhere invalidate it and close
its keeper transport. Servers store only a token hash. The app passes this
credential only to its authenticated local daemon; it never persists the value
in configuration, logs, bundles or mirrors.

## Workspace-bound worker delegation

**Status: optional foundation.** Supporting services may expose the distinct
scoped endpoints below. The current worker startup still configures its daemon
through `/api/v1/pro/configure/execution` with an unbound worker grant. Scoped
consumer and account tests do not establish a project-isolated worker flow.

`Delegation` additionally accepts `workspace: {workspace_id, revision}`. Absence
or `null` retains the existing account-wide semantics. Presence is an immutable
restriction to one registered workspace and positive registration revision; it
is never a request to expand authority. This revision identifies supervisor
registration, independently of the baton epoch. The initial bound contract is
worker-only with exactly `baton` and `mirror` scopes and no keeper scope.

A service implementing bound grants must authorize the workspace and live
registration revision before reading or creating a baton, changing policy, or
issuing mirror credentials. Derived Git grants retain that restriction and lose
access on project revocation/rebinding, parent revocation or account epoch
change. Renewal must preserve the exact workspace, revision, holder and scope;
a missing or different binding is not an acceptable replacement. Bound grants
cannot enter keeper discovery, inventory, raw TCP, reverse streams, provider
controls, account/device/billing management, or service-level worker operations.
The existing account-wide routes and grants retain their current behavior.

### Scoped service mint and revocation

A supporting account exposes the distinct authenticated worker-supervisor route
`POST /internal/v1/worker/workspaces/{workspace}/delegation` with
`{"revision": <positive registration revision>}`. Only the current account's
worker service credential may call it; device or daemon grants cannot. The reply
is a `Delegation` with exactly `baton` and `mirror`, the current worker holder,
and the exact `workspace: {workspace_id, revision}`. An absent or mismatched
binding is failure, never a reason to retry the account-wide mint. The supervisor
must obtain the revision from its durable trusted project registry, not a project
process or UI-supplied path. No secret values travel through this endpoint.

The service retains at most 128 registration records per account, including
revocation tombstones. A first registration accepts a positive revision. A
replacement at the same live revision rotates that project's token only; a
higher revision requires prior revocation. A revoked registration can be
registered again only at a strictly higher revision. Only token hashes are
stored. Each token lasts at most 24 hours, capped by its parent service expiry.
`POST /v1/delegations/renew` retains its token, workspace, revision, holder and
scopes, and fails if the registration, parent service, worker holder or account
epoch changed. Registration does not acquire execution or wake a machine.

`DELETE /internal/v1/worker/workspaces/{workspace}/delegation` takes the same
revision body and returns 204 after revoking that exact registration; an already
revoked matching revision is idempotent and a different revision returns
409 `workspace_registration_changed`. New grants and derived Git authorization
then fail, including a push's final authorization and checkpoint acknowledgment.
A Git commit already authorized before revocation retains only its existing
bounded publication fence. The supervisor must stop the old project namespace
before acknowledging removal of access to a secret; revoking network authority
alone cannot erase a value a running process already received.

Only exact-workspace baton reads/acquire/renew/release, holder policy publication,
mirror credentials and self-renewal accept this bearer. All account-wide routes
continue to use their existing authentication and reject it. Foreign workspace
requests fail before creating rows or reading project state. Every modifying
request rechecks the binding under the account lock after any wait; mirror fetch,
push, final commit and checkpoint acknowledgment recheck the same registration.
This surface is additive and remains unavailable when the service does not offer
negotiated continuity. It does not enable project-secret sharing by itself.

### Daemon acceptance

The trusted supervisor uses the authenticated **distinct** local route
`POST /api/v1/pro/configure/workspace`. Its body is the existing configure body
(`account_id`, `role`, `endpoint`, `keeper_url`, `delegation`, optional
`hours_exhausted`) plus `workspace_root`. It requires an explicit account ID,
`role: "worker"`, an empty keeper URL, a bound delegation with exactly the two
scopes above, and an existing absolute registered project directory. Initial daemon acceptance
requires Unix directory identity; unsupported platforms reject this route while
keeping the legacy device flow unchanged. The daemon
must be dedicated to that project: no unrelated registered workspace is accepted.
An unbound daemon already configured for an account must first be replaced with
a fresh dedicated daemon; reconfiguration cannot downgrade its authority.

A success is HTTP 200 with only:

```json
{
  "workspace_authority": 1,
  "workspace": { "workspace_id": "w-12345678", "revision": 7 },
  "workspace_root": "/projects/registered-project"
}
```

The supervisor must verify the version, exact binding and canonical registered
root before enabling work. `WorkspaceConfigureAck::decode` verifies a bounded
response without including its contents in errors. An old daemon's 404, 204,
SPA HTML fallback, missing acknowledgment, or mismatched acknowledgment fails
closed. **Never retry through legacy `/api/v1/pro/configure`.** That legacy route
rejects a bound payload in a supporting daemon; older serde consumers may ignore
the additive field, which is why a separate endpoint is mandatory. A service's
scoped mint likewise requires a distinct endpoint and explicit binding response,
never a body added to a legacy endpoint that may ignore unknown fields.

Acceptance latches account origin, account identity, workspace, revision and root
directory identity in a small credential-free record. Disconnect and restart
retain this restriction; malformed records disable scoped work. A different
binding requires a new trusted daemon registration/state, never deletion of a
live daemon's record. Delegation credentials remain memory-only. The additive
`workspace_configuration` field in `GET /api/v1/pro/status` reports the accepted
acknowledgment, including while disconnected; `configured` separately reports
whether credentials are currently installed.

Pro operations reject another workspace before filesystem or account access.
Hydration uses only the accepted directory, never an imported manifest's root or
an arbitrary caller destination; directory identity is rechecked before install.
A changed account origin or binding is rejected. Renewal also preserves the
holder and cannot widen scopes. Explicit trusted reconfiguration may change the
worker holder for the same registration; the remote service must separately
validate any such worker migration. Bound workers do not run account-wide
project discovery or lazy hand-back.

This daemon-owned marker prevents accidental reconfiguration; it is not a trusted
supervisor registry. A missing marker loads as legacy/unbound. A compromised
daemon could remove its own state, so the supervisor must independently retain
its registration and verify the exact scoped acknowledgment at every startup.
The remote credential restriction remains mandatory even if this marker is lost.
No missing marker or refused configuration permits a broad credential fallback.

This is a **consumer contract**, not complete project isolation. It does not
scope every generic session/file/MCP endpoint or protect against a compromised
daemon with an account-wide network credential. Project namespaces, trusted
project routing, provider delivery and cryptographically enforced service-side
workspace authority must all be integrated and verified before selected-project
secret sharing is enabled. The public fake account still implements legacy
account-wide delegation only; service-side scoped conformance remains a separate
implementation gate.

Conformance includes legacy optional-field decoding, exact acknowledgment
validation, a real loopback old-daemon rejection without fallback, retained
restrictions after disconnect/restart, renewal downgrade rejection, changed-root
rejection, and foreign-workspace requests with no file or network side effects.

## Automatic takeover policy

The current holder PUTs
`/v1/baton/{workspace}/policy` with
`{holder_id, epoch, handoff_enabled, offline_takeover, has_agents}`. It requires
an unexpired owned epoch and returns 204. Delegation credentials may publish the
same policy. A full device may DELETE that path to disable all three flags even
when another device holds the baton; scoped delegations cannot disable policy.

The daemon publishes this policy with every snapshot in **both** protocol
versions (the account's offline wake and the worker's discovery read only these
flags, and v2 has no other resource that sets them). It is published while the
epoch is still owned and before any snapshot bytes are pushed, so a refusal
cannot strand a published checkpoint that nothing will continue.
`handoff_enabled` and `offline_takeover` are `!hours_exhausted`; `has_agents`
says whether the snapshot archived an agent session. A workspace-scoped
delegation may reach this one `/v1` path under v2 as well.

Policy survives ownership changes. Automatic worker wake considers an expired
**device** holder only, and requires all three flags, a published mirror, and
current entitlement and budget. An expired worker lease alone never wakes a
worker. These flags do not change the lease compare-and-swap rules or permit
active-owner takeover.

## Required placement and execution-fencing follow-up

The maintainer's requested next placement contract is distinct from the current
v1 behavior above. Both protocol versions deliberately let a personal computer
keep running during account unreachability until it verifies a newer epoch
("laptop first"); remote write fences do not prove that an offline old process
has stopped. The workspace credential consumer contract does not resolve that
execution-partition gap.

Acceptance for the placement follow-up requires:

- A preferred home host, current execution holder and viewer device are separate
  identities. A phone/browser follows the logical workspace and current session;
  opening that view cannot choose or wake a different execution host.
- Available home execution stays at home. Cloud is the synchronized handoff and
  recovery fallback when home is unavailable; safe return prefers home again.
- Abrupt loss recovers only the latest durably published project/conversation
  checkpoint. It cannot recover uncheckpointed bytes or blindly replay external
  actions that may already have happened.
- Before cloud takeover, an execution lease or equally strong end-to-end fence
  must prevent old-home execution from continuing concurrently during a network
  partition. Account write fencing alone is insufficient for arbitrary external
  tool side effects. Expiry, resume from sleep and return must preserve the same
  single-runner guarantee, with deliberate safe handling of uncertain effects.
- Browser/native routing, current-host changes and recovery are verified together;
  no cloud-machine chooser is part of the normal continuity flow.

These are required follow-up gates, not claims that v1 or the workspace-bound
credential addition already implements automatic home-first routing or complete
execution fencing.

## Transfer operations, sleep and drain (daemon routes)

`POST /api/v1/pro/handoff {workspace_id, expected_epoch}` and
`POST /api/v1/pro/hydrate {workspace_id, expected_epoch, requires_fork?, destination_root?}`
run as owned daemon tasks keyed by (kind, workspace, epoch). A caller that
disconnects (the keeper's 90-second relay, a supervisor timeout) loses only the
reply: the flush or hydration completes, or its recovery restores ownership, and
it never leaves Git mid-write. A repeated request joins the running task. A
completed handoff is remembered for ten minutes, so a retry after a lost reply
answers 204 instead of a bare 409; hydration re-verifies on every request, and
failures are forgotten so a retry starts fresh. The status codes are unchanged
(204 on completion). Refusals carry an additive stable `code` next to the English
`error` (for example `ownership_changed`, `checkpoint_pending`, `account_changed`,
`draining`, `workspace_busy`, `cloud_provider_not_ready`, `git`,
`service_rejected`, `return_window_ended`); clients map codes, never error text. A bare 409 from a
worker handoff is final for that return pass unless the worker woke into a new
epoch.

`POST /api/v1/pro/sleep` accepts an optional `{deadline_ms}` (additive; an empty
body keeps the 25-second default) and answers within it. It preempts the
periodic mirror pass, flushes every owned project in parallel (projects with
live agents first, the rest only while time remains) as owned tasks, and does
not wait out the account's publication fence past the deadline (an unreleased
lease lapses and the cloud continues from the acknowledged checkpoint). The
reply is `{handoff, failed:[{workspace_id, error:<code>}]}`, plus
`reason:"deadline", pending:[workspace_id]` when flushes are still finishing on
their own. A flush that could not hand its project over (its release ran out
of time, or publication failed) never renews the lease or resumes agents inside
the sleep window: an unreleased lease lapses and the cloud continues from the
acknowledged checkpoint. `POST /api/v1/pro/wake` advances a sleep generation and
returns every project a sleep flush holds to this computer at once (writable
immediately): a flush still running keeps its publication, stops no further
sessions, never releases and resumes the sessions it stopped; one that finished
without handing over resumes its stopped sessions right away. Neither waits for
the account. Signing out does the same for any transfer or return this computer
itself started.

`POST /api/v1/pro/drain {deadline_ms?}` is the public half of a fenced cloud
suspension. It takes the job reservation, stops new periodic passes, refuses new
handoff/hydrate/sleep/privacy work with 409 `{error:"draining"}`, and answers 200
`{token}` once every transfer task, sleep flush, project cache (including a Git
finalizer outliving its caller) and Git helper slot is free and state is synced
to disk. On a cloud machine it first publishes each project it holds under a
live lease (at most thirty seconds, ten short of the deadline): the copy a
computer may take the work from while the machine sleeps ([acting brings the
work to you](#acting-brings-the-work-to-you)). Past the deadline (default 60 s, at most 600 s) it releases itself and
answers 409 `{error:"transfer_busy"}`. `DELETE /api/v1/pro/drain` cancels; a drain
also lapses 15 wall-clock minutes after it began (a machine resumed without a
cancel). The token is at most 24 characters with no control characters. Drain
requests are serialized: a second one waits for the first and returns the same
token. A request whose caller gives up before it completes leaves nothing
draining. A transfer admitted just before the drain but still waiting for the
job reservation refuses itself (409 `draining`) when the drain takes it, rather
than holding the drain open. After a completed drain, any
`pro_cloud_operations > 0` counts as activity. Lease renewals continue while drained. On a cloud machine
`GET /api/v1/health` reports `pro_cloud_operations` (transfer tasks, sleep
flushes, held project caches and busy Git helpers; a completed drain counts
zero) and additive `last_activity_ms`, the last user change that is not session
input (file saves, drafts, uploads, file moves, Git worktrees, session and
workspace lifecycle; an explicit route list, so read-only POST helpers and
passive viewing never count).

The supervisor's idle sample reads `GET /api/v1/sessions`. Besides
`agent_state`, `output_active`, `background_running`, `phase`, `exec_stage` and
nullable `last_input_ms`, chat rows carry an additive boolean
`needs_permission`: true while the conversation waits on a permission or a
question. The supervisor keeps the machine awake on it for a bounded time and
then suspends with ownership retained, so the question survives in the frozen
process and its answer wakes it. PTY rows omit the field (a Claude TUI reports
`agent_state: "needs_permission"` instead).

## Explicit worker wake

A full device may POST `/v1/worker/wake` with `{}` for a deliberate cloud action.
It returns 202 with `{worker_id, state, keeper_url}`: `worker_id` is nullable,
`keeper_url` is empty until assigned, and state is `pending`, `starting`,
`started`, `suspended`, `stopped`, or `retry`. Provisioning unavailable returns
503; entitlement or budget refusal returns 403 with a stable error code. The
account is derived from the device credential. Delegations cannot invoke it.
Directory/health polling must not call this endpoint.


Deleting the handoff policy also disables mirroring for that workspace account-wide:
existing derived Git grants stop working, and new read/write grants are denied.
Publishing another policy does not clear this privacy choice. Only an explicit
full-device POST `/v1/baton/{workspace}/enable-mirror` with `{}` may re-enable
credential issuance (204); daemon delegations and worker credentials cannot call
it. Re-enabling does not restore the automatic handoff flags.


## Managed execution v2 (negotiated implementation)

The additive public types are in `continuity.rs`. This protocol is enabled only
through exact capability negotiation; a legacy configure success never proves
execution fencing. Two exact capabilities are supported:
`{version:1,boundary:"managed_processes",expired_takeover:false}` with policy
`managed_v1`, and `{version:2,boundary:"canonical_checkpoint",expired_takeover:true}`
with policy `checkpoint_fork_v1`. The latter is the automatic-recovery default:
it guarantees a single canonical grant/checkpoint publication, not atomic
physical process extinction on a disconnected computer or exactly-once external
effects. Process groups do not contain deliberately detached descendants.

`GET /v2/capabilities` returns execution_authority 2, the exact default capability,
supported_execution_capabilities, failover_grace_seconds 30, installation_binding
1, workspace_placement 2 and checkpoint_receipts 1. Clients decode the advertised
capabilities leniently and then compare each exactly with what they implement.
They use the service default when they implement it; otherwise their own
preference (`checkpoint_fork_v1`, then `managed_v1`) among the listed ones. No
capability in common, or a 404 for the route, is `ServiceUnsupported`: no
execution configuration and no legacy fallback. `failover_grace_seconds` is
informational to clients today. Native clients keep the stable installation
proof in the account/origin-specific keychain; the daemon receives only its
opaque installation ID.
`POST /api/v1/pro/configure/execution` takes ordinary Configure plus
`execution:{version:1,installation_id,capability}` and optional workspace_root
for a workspace-bound worker. Its 200 response must exactly match
`ExecutionConfigureAck`; 204, HTML, 404, missing/changed fields fail closed.

`GET /v2/baton/{workspace}` may observe legacy work with null continuity; it
never grants execution. Existing live legacy grants remain legacy until a clean
release. Acquire/renew/release add execution_capability and return continuity
(version 2, mode managed_v1 or checkpoint_fork_v1, policy_revision, preferred_installation_id), an
execution_lease (opaque id and increasing sequence), and a checkpoint. Acquiring
and renewing pin the selected checkpoint; GET reports the latest acknowledged
publication. Once enrolled, legacy acquisition/renewal/write credentials and
publication cannot downgrade the workspace. Both the link client and the daemon
decode these account responses leniently: unknown fields in the baton,
continuity, lease and checkpoint are ignored and an unknown `continuation` value
reads as `uncertain`. The capability object and the daemon's own acknowledgments
stay exact.

A client deadline starts before the mutating request and uses server-relative
lease duration (at most 90 seconds), minus a 15-second stop margin. Passive GET,
replayed lease sequences, stale generations, clock discontinuity and restart
cannot extend that deadline. Viewer location never selects the preferred home.
The preferred installation is simply the latest device that acquired the project;
an acquire is never refused for not being the preferred installation, only for
`held` (another live holder).
Clean transfer selects exactly the receipt's working-tree/config/handoff Git
object IDs. Unknown continuation evidence is uncertain, never a blind replay of
external actions. In checkpoint_fork_v1, after the recorded lease expiry plus
30 seconds, an acknowledged checkpoint permits a new canonical epoch and a
forked native conversation. The logical session identity remains stable. The
receipt may originate from an earlier epoch if an intervening executor never
published; its own source epoch, not the incoming grant epoch, binds the manifest.
A known idle conversation remains idle. Interrupted or unknown work gets bounded
historical recovery context directing the agent to inspect files and external
state before repeating effects, using existing permissions without a routine
human-review gate. Timeout alone is never proof that the old OS process stopped.

That context is one visible pick-up message the receiving daemon sends a
structured conversation, in plain words (where it now runs, same conversation
or a copy, files installed and may differ, re-check tools and paths). Its
`UserMessage.origin` is a stable tag clients key on: `moved` (a clean move, now
in the cloud), `home` (a clean return, back on the user's computer) and
`recovered` (either direction, continuing from the last saved point because
the other machine stopped responding). A terminal agent gets one neutral
positional prompt instead, and only for a turn that was cut off.

Lease expiry fences execution only on a cloud worker, and a resumed worker
renews before fencing: after a suspension (seen as a clock discontinuity) its
daemon first renews the recorded epoch, which the account grants to a suspended
owner at the same epoch with no fork; only a refused renewal (or no answer within
20 seconds) fences. That renewal starts the moment the thaw is noticed (by the
daemon's 100 ms watchdog, or by the first request or socket frame to arrive
after it), never at the next 5-second renewal tick, because a viewer's socket
is admitted only once it answers. A personal computer is
fenced only by a verified other owner (an authenticated read naming another
holder) or its own in-progress transfer: account unreachability, sign-out, a
lapsed plan, the privacy switch or a daemon restart stop publication, never its
agents or shells. A verified other owner refuses local input at once; the
computer's agents stop at their next safe pause, at most five minutes later.
Re-acquiring the epoch this installation itself held (its own clean release, or
its own lapsed lease) continues local work: no checkpoint install and no fork,
even though the account marks a lapsed-lease acquisition `requires_fork`. The
account's `takeover_grace` refusal is a quiet wait. Plain shells are never
managed processes.

A cloud machine that suspends keeps ownership (placement `suspended`, lease
expired by design); the account answers anyone else's acquire with 409 `held`
for as long as it stays paused. A computer that wants the project back reads
placement (passive, never wakes) and, once settled on power, POSTs the worker's
`/api/v1/pro/handoff` through the keeper's HTTP adapter with `X-Chimaera-Wake:
interaction`: the keeper wakes the machine, which renews its own epoch, flushes
at a safe pause and releases, and the computer then hydrates. A `held` refusal
is never treated as final, and the computer never fetches or acquires from
under a suspended owner.

### Acting brings the work to you

Opening a project elsewhere only views its owner; acting on it moves the work.
Both halves are additive.

**Between computers.** A computer whose user sends a chat message or types
into a terminal of a project another signed-in computer holds asks for it:
`POST /v2/baton/{workspace}/move` with `{holder_id, epoch}` (its own holder,
the epoch it saw; a device credential or its daemon's delegation; a cloud
machine's credential is 403). While the other computer holds a live lease the
account records the request and answers 200 with the ownership read; while
nobody runs the project (released, or a computer's lapsed lease) the same
request reserves the next acquisition. A project a cloud machine holds is
never asked for this way, whatever the machine's state (awake, asleep, or
stopped with its lease lapsed): 409 `held`. An epoch the caller did not see
is 409 `stale_epoch`. The holder asking for its own project is its user
acting there: the last actor wins (by the account's clock) and any request
for the project ends. `DELETE /v2/baton/{workspace}/move` withdraws the
caller's own request (204; the same call from any other computer changes
nothing).

While a request is fresh (six minutes for a computer's, two for a phone's)
every ownership answer (GET, acquire, renew, release, move) carries the
additive `move_to` (the holder the work goes to), `move_requested_at` (the
account's clock) and `move_reason` (`computer` or `phone`); a stale request
reads as none. Only `move_to` may acquire the project while it is fresh
(anyone else, including a cloud machine the release would otherwise wake, is
409 `held`); the worker's discovery does not offer it and a release for it
wakes nothing. The acquisition ends the request. A request that expires
unanswered ends as if it had never been made: a release made for it is then
an ordinary release, so the worker's discovery offers the project again and
the automatic cloud wake after a device release may take it.

The holder's daemon reads `move_to` from its renewal answer. Unless its own
user acted after the request (its local input time, placed on the account's
clock through `server_now − move_requested_at`, in which case it posts `move`
naming itself), it finishes its current step (a chat's turn, a terminal
agent's next pause; plain shells never wait), publishes and releases exactly
like the clean handoff (`/pro/handoff`'s owned flush), and its own views say
the session `moved` with `to:"computer"` and the additive `other:true`. A
handover that fails recovers here and posts `move` naming itself, so the
asker hears at once. The asking daemon holds the input that asked (the
viewer relay's held-input budget), tells the viewer `{"type":"bringing","to":"here"}`,
takes the released epoch like a return (the reservation keeps anyone else
out; no fork), resumes the sessions and delivers the held input once. After
five minutes without a release it withdraws the request and refuses the held
input (`reason:"still_working"`); a holder that released for a request that
was then withdrawn takes its own epoch back (no install, no fork).

**From a phone.** A computer's daemon adds `?ready=1&power=ac|battery` to its
passive ownership read when it could take the project now (a negotiated
personal computer, the project registered there, not handed to the cloud on
quit, allowed to leave its computer). While a cloud machine holds the project
the account remembers that computer for a few seconds. When a phone sends or
types into a project whose owner is a sleeping cloud machine (`suspended`) —
the first input the account would hold for that machine
([VIEWING](VIEWING.md#forwarded-requests), "A sleeping cloud machine's
sockets"), or a reconnect carrying wake intent from a client that parks — the
account's browser gateway asks one such computer to take the work home
instead: it must be reachable over the keeper (its app online),
seen in the last fifteen seconds, and the machine must have gone to sleep
cleanly — its latest checkpoint is its own, of its current epoch, with
continuation `idle`, acknowledged no earlier than two minutes before the
suspension, with no wake requested since. The project's home comes first,
then a computer on power, then any. The request (`move_reason:"phone"`) lets
that computer acquire from the sleeping owner cleanly (`requires_fork:false`,
no wake; the machine finds its epoch gone when it next resumes and fences).
The computer takes it without its settle wait. The gateway tells the phone
`{"type":"bringing","to":"computer"}`, holds its socket authentication and
first input, and delivers them once to the computer when its session
answers; if the computer has not acquired within about twenty seconds the
request is withdrawn and the machine is woken as before
(`{"type":"waking"}`), with the held input delivered to it instead. While
the work is being brought the gateway holds only a chat's acting commands,
its seven settings commands and a terminal's typing, as a keeper does
([VIEWING](VIEWING.md#forwarded-requests)); a view's other frames are not
held. It is a holder like any other: a second copy of a send it holds is
dropped, and a `cancel_send` for one is answered `cancelled:false` there. Held input the owner's session does not take is refused to the phone
(`command_failed` or `read_only` with reason `reconnecting`; a refused chat
command carries the `client_id` it was sent under, so exactly that send
returns to the composer), never dropped.

A cloud machine's drain (`POST /api/v1/pro/drain`) therefore publishes each
project it holds before it answers, within the drain's deadline; a failure
only means the machine is woken for the work instead.

### v2 refusals

v2 acquire/renew/release and the related routes answer with these stable codes
(HTTP 409 unless noted). Clients must treat the retryable ones as waits, not as
permanent refusals:

| Code | Meaning | Client behaviour |
| --- | --- | --- |
| `stale_epoch` | Epoch mismatch or not the current holder (carries `baton`) | Re-read and decide; never force |
| `held` | Another live holder owns the project, or a fresh request to move it reserves it for another computer (carries `baton`) | View it; no takeover |
| `takeover_grace` | Expired holder still inside the 30 s grace (carries `baton`) | Retry after the grace |
| `unsafe_takeover` | Expired holder but the project is not in `checkpoint_fork_v1` (carries `baton`) | No automatic takeover |
| `mirror_commit_in_progress` | A verified push is publishing refs (≤10 s) | Retry with jitter |
| `checkpoint_required` | No acknowledged checkpoint for this epoch (also on a clean release that has not published) | Publish first, then retry |
| `clean_release_required` | The caller still owns a legacy (pre-v2) grant, or a rebind while the old holder still owns work | Release cleanly first |
| `installation_required` | A device acquiring without a bound installation | Bind the installation |
| `not_preferred_home` | Retired: the account no longer refuses an acquire for not being the preferred installation (that is the latest device that acquired). An older service may still answer it | Leave it to its home/cloud (older services only) |
| `capability_not_supported` | The request's capability differs from the project's mode | Use the recorded mode |
| `continuity_upgrade_required` | A legacy (v1) route on a v2-enrolled project, or v2 renew/release without a record | Use v2. A legacy release answered this way carries only `{"error":"continuity_upgrade_required"}` (no `baton`); the daemon recognises it before any conflict parse (`release.rs` `UpgradeRequired`), logs one line, latches the project as enrolled (`execution::require_v2`, in memory) so the next reconcile reads it over v2 and restores its policy, and never retries in the same call. The same refusal to a v2 request is a plain failure: no latch, no retry |
| `recovery_in_progress` | An installation recovery owns the project | Wait for it |
| `workspace_limit`, `installation_limit`, `installation_already_bound`, `stale_policy`, `publication_expired`, `checkpoint_mismatch` | Account limits and stale publication/policy state | Surface; no retry loop |
| 403 `mirror_disabled` | The project is kept on its device (privacy) | Stop publishing |
| 403 `return_window_ended` | The plan ended and the time to bring its cloud work home (`returning_until` on `/v1/me`) has passed | Say so plainly (the daemon's `error_code` and mirror-row code are `return_window_ended`); no retry |
| 404 `workspace_not_found` | Placement read for a project with no ownership record | Treat as unowned, epoch 0 |

Returning home installs the canonical receipt only after its own registered
managed processes are stopped. Launch evidence records the process groups of
live managed agents; a graceful daemon stop clears it once they exit, and a
same-boot successor after a crash waits only for recorded groups that still
exist (re-probed every lease tick). Without recorded groups a computer proceeds
and a worker stays fenced; a worker never acquires or renews a lease it could
not accept. Unsynchronized local file conflicts are retained
outside the mirrored project (100 MB per file, 1 GiB/4096 files total), while
canonical file content occupies the original path. Exceeding preservation limits
retains the original and fails the import; it never deletes old conflict copies.
The three-way baseline is the last acknowledged publication (never a local
commit whose push failed); a file only this computer changed keeps its edit, and
a file absent from the incoming snapshot is deleted only when that snapshot's
additive `left_out` inventory (paths it deliberately omitted) shows it gone.
A globally advertised new capability does not change an existing strict-mode
renewal; mode changes require an explicit clean unowned acquisition.

A normal installation rebind first stops/publishes/releases with its old valid
credentials. If those credentials expired, full new-device authentication plus
the existing installation proof can request
`POST /v2/installations/{installation}/recovery` with workspace_id and
expected_epoch. The response is `ExecutionRecoveryGrant`: five-minute, one-use
mirror/release authority for exactly the old device, workspace and epoch. It
does not renew execution and is never accepted by ordinary acquire, renew,
proxy, or daemon Configure. The dedicated local
`POST /api/v1/pro/execution/recover` accepts `ExecutionRecoveryRequest`, verifies
the old binding, stops managed execution, publishes its final snapshot using
`/v2/recovery/mirror/credentials`, and calls `/v2/recovery/release`. Only the exact
200 `ExecutionRecoveryAck` permits the native rebind. This uses the same trusted
owner clean-release boundary as normal handoff, not an OS attestation assembled
from client JSON. Account issuance reserves the old epoch; a timeout cannot
renew the recovery deadline or grant another execution owner.
