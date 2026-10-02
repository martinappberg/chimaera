# Selected-project custom secrets v1

This optional contract is disabled until the complete project execution,
storage, network, provider and control boundaries have passed integration tests.
Publishing this contract advertises no capability and enables no service.
An older deployment must return unsupported before accepting a secret value.

A custom secret is an environment value shared with explicitly selected cloud
projects. It is separate from the personal-cloud Claude, Codex and GitHub
connections in [PROVIDERS.md](PROVIDERS.md). A project receiving a value can use
or print it; this feature isolates other projects, rather than promising to
erase values from an authorized project's outputs or external services.

## Product behavior

The control lives in the personal account surface. The user supplies a name,
value and selected projects. Each selected project has its own stored grant,
revision, pending change and outcome. Selecting several projects does not imply
an atomic change across them. Results identify any project that did not accept
the change; success for one project must not hide a failure in another.

Additions and replacements queue until the affected cloud project is idle.
**Apply now** explicitly stops that project's current cloud work and applies
its queued changes. The confirmation names the project and every queued name
that will be applied. Removing access also explicitly warns that the affected
cloud work stops. Neither operation takes over local/HPC execution, changes the
preferred executor, or resumes an agent automatically.

Queued, applying, applied, canceled and unconfirmed are distinct visible states.
A saved queue receipt is not an applied receipt. No input is silently retried
after an ambiguous reply. Status reads resolve the original operation ID and
never create or restore an intent, submit values, start a daemon or wake compute.
Explicit submission and Apply now may request cloud availability; polling and
refresh remain passive.

Stored values are never read back. The UI clears its entered value after the
submission starts and when its account changes or the editor closes. It never
stores values in local storage, settings, a URL, a session journal or telemetry.
The control displays names, selected projects and outcomes only.

## Authority and negotiation

The exact positive capability is `project_secrets:1`. It is independent of
`providers_control` and `provider_runtime`; none implies another. It is offered
only by the personal control plane after the actual worker proves the complete
isolated-project path. A cached capability can show status but cannot authorize
a value submission. Missing, partial or unknown versions refuse before values
are forwarded. There is no legacy shared-daemon fallback.

Only a freshly authenticated personal device session can submit, cancel,
replace, immediately apply or remove a secret. Daemon delegations, project
bearers, viewer tokens and provider-runtime capabilities cannot edit secrets.
Native commands are available only to this computer's local account window;
a remote/project webview cannot use the native account bridge. An account web
surface uses its own authenticated same-origin control route and CSRF defenses,
never a project daemon as a credential proxy.

The keeper chooses the current personal worker from authenticated account
state. The worker chooses the project from its trusted stable-ID catalog and
durable registration. Requests never select a host, path, process, port, UID,
executable, environment overlay location or account identity.

## Bounded control messages

The fixed external keeper surface is:

| Method and path | Purpose |
| --- | --- |
| `GET /v1/personal/project-secrets` | Passive redacted catalog and exact capability acknowledgment |
| `POST /v1/personal/project-secrets/commands` | One explicit project command |
| `GET /v1/personal/project-secrets/operations/{operation_id}` | Passive original-device operation status |

A command has version `1`, a canonical UUID `operation_id`, one stable
`workspace_id`, its exact `expected_revision`, and `expected_pending` (the exact
pending intent UUID, or JSON null when no intent was shown). Stale registration
or pending state returns a conflict with redacted current status. A fresh read
and another explicit decision are required; there is no automatic CAS retry.
Revisions are integers from zero through `9007199254740991`, never wrapping.
Stable IDs are 1–128 ASCII letters, digits, `_` or `-`.

Commands are a closed tagged union:

| `action` | Additional fields | Effect |
| --- | --- | --- |
| `set` | `name`, `value` | Queue one addition/replacement; preserve other applied values and the pending changes shown by `expected_pending` |
| `apply` | None | Apply that exact pending batch now, with its explicit stop confirmation |
| `cancel` | None | Cancel that exact pending batch; applied values remain unchanged |
| `remove` | `name` | Stop the project and remove that applied name; cancel its pending batch, as stated in the confirmation |

An accepted `set` replaces the pending intent identity, retaining other queued
names from the exact previously displayed intent. Its fresh personal admission
authorizes the whole resulting batch; the response lists all queued names.
The native/browser control shows those names before a submission that carries
existing queued changes forward. Another device's unseen edit cannot be folded
into the user's decision. A remove confirmation also names any queued changes
that will be canceled. An absent name is a conflict, not a false removal success.

There are at most 128 registered projects, 32 applied or resulting secret names
per project and one pending batch per project. A name is 1–128 uppercase ASCII
letters, digits or `_`, with no leading digit. Runtime/loader/shell/provider
configuration names are reserved and visibly refused; the negotiated catalog
provides the exact supported restrictions. Values are nonempty UTF-8, at most
8 KiB each, without NUL. The complete JSON request is at most 64 KiB, including
escaping, and rejects duplicate and unknown fields. Value-bearing types have
no debug representation; request/error logging must not include their bytes.
Catalog/status bodies have a 1 MiB ceiling and contain no values or value hashes.

An operation UUID never authorizes re-submitting a value. Once accepted, the
trusted worker owns bounded persistence and cleanup even if the caller leaves.
A lost reply is resolved through the original operation status. A different
payload under an old operation ID refuses; responses never reveal whether a
guessed value matches stored data. At most 16 requests and four pending writes
are admitted per personal worker, with queue/backpressure limits retained until
the actual owned work and cleanup end.

## Durable queue and application

The queue stores encrypted values outside all project files, homes, bind mounts,
mirrors and process environments. Encryption authenticates account, stable
project, immutable intent identity, base grant revision and name. A pending
value is not usable by the current project runtime. Only names and nonsecret
operation receipts are exposed to status. The existing applied grant remains
unchanged while an update waits.

A queued batch survives ordinary worker restart and device access-token refresh.
There is no arbitrary idle-wait expiration: one bounded pending batch per
project is durable user intent. Its original personal device/session epoch,
current keeper and worker credential identities, project registration, base
revision and exact resulting batch remain bound. Revoking that device/session,
changing the account epoch, replacing those service identities, canceling the
batch or changing the project registration retires its authority. It requires
a new explicit decision; neither polling nor recovery may mint a replacement.
No plaintext device access bearer is persisted with the queue.

Before applying, the keeper obtains fresh account authorization for that exact
durable secret-purpose admission and current service credentials. Provider
login admissions cannot be reused. The account check survives ordinary access
refresh but rejects actual device/session/account revocation. The private
authorization is one-use and expires within five seconds measured from request
start. After the reply, the keeper and worker recheck their current registration,
pending intent and expected applied revision before any effect. Account outages
leave the batch queued and do not permit applying from a cached successful read.

Automatic application first proves the exact project has no active agent turn,
pending input, setup command, mutation, user terminal or other user workload.
An idle managed agent may be durably parked through the existing conversation
drain/resume protocol; its conversation and delivery receipts must survive.
An idle-looking terminal, quiet output, an empty browser view, missing process
evidence or a failed status request is not proof that stopping is safe.
Unknown or unresumable workloads keep the change queued. The idle decision,
durable parking and stopping/admission fence share the same serialized runtime
owner: new work cannot start between the check and the update. An explicit
Apply now uses that same fence after its stop confirmation. Custom processes
and detached descendants belong to the project runtime and cannot escape the
stop boundary.

The trusted owner durably fences the old grant revision before stopping the
complete old project runtime. Only a positive cleanup acknowledgment, confirmed
remote revocation floor and durable new encrypted grant permit an applied
receipt. Partial cleanup, a failed write or a lost remote acknowledgment remains
applying/recovery-required and denies old authority. Restart resumes the owned
transition, never replays raw submitted values or acknowledges a missing step.
Other projects and their work remain unaffected.

Applying/removing a secret changes only the named project grants. Unchanged
applied values are preserved internally without returning them to the client.
Removing the last custom secret leaves the isolated project usable; it does not
delete the project, revoke named provider connections or disable future cloud
work. It still requires the same stop/revocation acknowledgment.

Pending admission retention is bounded by the project cap. Terminal operation
receipts are retained for at least 24 hours with a finite account limit and
bounded cleanup; reaching that limit refuses new submissions rather than
discarding a still-needed acknowledgment. No terminal receipt can restore a
canceled or already applied intent.

## Account authorization for the durable queue

These internal endpoints are fixed service-to-service paths. They receive no
custom value, value hash, provider credential or caller-selected destination:

| Method and path | Purpose |
| --- | --- |
| `POST /internal/v1/personal/project-secrets/admissions` | Admit one explicit `set` intent durably |
| `POST /internal/v1/personal/project-secrets/applications/authorize` | Freshly authorize that exact pending intent for automatic application |
| `POST /internal/v1/personal/project-secrets/edits/authorize` | Freshly authorize an explicit `apply`, `cancel` or `remove` decision |
| `POST /internal/v1/personal/project-secrets/admissions/retire` | Irreversibly retire the exact original intent |

All four require the keeper's current service bearer and the current personal
worker service token in the closed request. They are unavailable unless this
separate secret-control capability is enabled; provider-control admission is
never an alternative. The authorization tuple is a closed object of at most
8 KiB, with these exact fields:

- `version:1`, `account_id`, `holder_id`, `worker_credential_digest`;
- `device_id`, `device_session_epoch`, `workspace_id`, `expected_revision`;
- `operation_id`, `expected_pending` (UUID or null), `action`, `names`.

IDs follow the external bounds above. Internal authorization requires an existing
registration, whose revision starts at one and has the same external maximum.
The worker digest is the
64-character lowercase SHA-256 of its current service credential; it never
enters a project. `names` is a sorted, distinct list of at most 32 valid secret
names: the entire resulting pending batch for `set`, the exact shown batch for
`apply` or `cancel`, and the single applied name for `remove`. A `set` has at
least one name and its operation UUID becomes the pending intent identity;
it cannot equal its previous pending UUID. Apply/cancel require a non-null
pending UUID and nonempty names. Remove has exactly one name. Process boot IDs
are deliberately absent: restarting the same worker preserves intent, whereas
replacing its credential or machine invalidates it.

Admission accepts the closed object `{authorization,device_token,worker_token,
device_authority}`. The keeper generates `device_authority` as exactly 32 random
bytes encoded as canonical unpadded base64url before its first request. It never
comes from a project. The account stores only its hash with the immutable tuple,
original device/account epoch and original keeper/worker credential identities.
The original full device access token must be current when creating the row.
An exact retry can observe that same row after ordinary access-token refresh;
it cannot alter its tuple, renew a retired row or authorize resubmitting values.
The keeper retains the authority only in the trusted encrypted queue/control
store, never a plaintext device access token.

Automatic application accepts the closed object `{authorization,worker_token,
device_authority}` and must find the original live `set` admission. Explicit
edits instead accept `{authorization,device_token,worker_token}` with action
`apply`, `cancel` or `remove`; they always require a current full device access
token. Apply now is a fresh explicit decision for all the shown names, so it can
replace authority retired by an earlier device revocation without reading values
back or silently adopting unseen edits. The worker must still compare the exact
pending identity and applied revision at its serialized execution fence.

Admission/application/edit validation locks account, keeper cell and worker cell
in that order, then verifies the original/current device, session epoch,
entitlement, current service identities and exact nonrevoked workspace
registration revision using the clock after those waits. Missing registration
refuses. All success replies are closed, at most 12 KiB, and exactly
`{version:1,authorized:true,authorization:<original tuple>}`. Their private local
proof is one-use and expires five seconds from the caller's request start;
response arrival never restarts the clock. Keeper and worker perform their
post-wait identity checks before consuming it. A reply is not a queued or
applied receipt. These routes never wake a service, stop work or change a grant.

Retirement accepts `{authorization,worker_token,device_authority}` and returns
`{version:1,retired:true,authorization:<original tuple>}`. It requires current
keeper/worker authority for the same account, plus the exact original tuple and
hashed admission authority, but not a still-live original device or old project
revision. This permits cleanup after revocation without resurrecting authority.
It marks the row terminal irreversibly and is idempotent while the tombstone is
retained. A retired or missing row cannot authorize application; retirement of a
missing row refuses. Admission never re-creates a retained retired operation.
After all bounded receipts have expired, a genuinely new explicit full-device
decision is required; status, recovery and automatic application cannot create
an admission. UUIDs alone do not prove an unlimited replay history. The worker
retires superseded/canceled/applied intents after
its corresponding durable local transition, preserving an owned cleanup receipt
until retirement is acknowledged. Partial admission or local persistence failure
never counts as successfully queued.

There are at most 128 live admissions and 1024 total retained rows per account.
Admission refuses when full; it does not discard an older live intent. No live
admission expires merely because a project remains busy. Revoked/expired device
sessions, changed account epoch, replaced service identities and changed/revoked
project registrations permanently retire their admissions during bounded
maintenance. Retired rows remain for at least 24 hours, with cleanup batches of
at most 1000. External operation receipts independently preserve their minimum
retention and reject replay while retained before a new internal admission could
be attempted.
Each complete request is at most 12 KiB and rejects duplicate/unknown fields;
credential-bearing request types have no Debug implementation. Service tokens
are 16–256 ASCII letters, digits, `_` or `-`; no credential is logged.

## Private keeper and supervisor bridge

This bridge is separate from provider login control. It uses the existing
authenticated current personal-worker registration, with a separately negotiated
`project_secrets:1` acknowledgment. A provider registration, a cached project
catalog or the worker's ordinary shared daemon cannot supply that acknowledgment.
Replacing or withdrawing the registration immediately closes command admission.
No ordinary startup enables this bridge before the acceptance gate below.

The worker exposes only these fixed supervisor routes to its authenticated
keeper. They are not forwarded into a project namespace:

| Method and path | Purpose |
| --- | --- |
| `GET /internal/v1/personal/project-secrets` | Redacted registered-project catalog |
| `POST /internal/v1/personal/project-secrets/commands` | One owned queue or explicit edit |
| `POST /internal/v1/personal/project-secrets/operations/read` | Original-device receipt read |

A command envelope is exactly `{version:1,authorization,command,admission}`.
`authorization` is the account tuple above. `command` is the original external
command, unchanged. For `set`, `admission` is exactly
`{authorization,device_authority}` for the already admitted original intent;
for other actions it is JSON null. The two tuples and all command identity,
revision, pending, action and resulting-name fields must agree. This envelope
has an 80 KiB ceiling including escaping and rejects duplicate/unknown fields.
It never contains a device access bearer. Receipt reads accept exactly
`{version:1,operation_id,device_id}` and cannot create or resume an operation.

The worker obtains fresh authorization through fixed keeper callbacks before
the serialized local effect. Every callback authenticates the current worker
service token and rechecks the current personal registration and credential
digest after waits. No URL, host, account or credential chosen by project code
can select its target. The callback routes are:

| Method and path | Closed body and effect |
| --- | --- |
| `POST /v1/personal/project-secrets/admissions/validate` | `{authorization,device_authority}`; validate an existing `set` admission before persisting its queue |
| `POST /v1/personal/project-secrets/applications/authorize` | `{authorization,device_authority}`; freshly authorize the exact durable pending batch |
| `POST /v1/personal/project-secrets/edits/authorize` | `{authorization}`; validate a currently owned explicit apply/cancel/remove decision |
| `POST /v1/personal/project-secrets/admissions/retire` | `{authorization,device_authority}`; retire an original admission after its durable local transition |

Queue validation and automatic application use the account's existing
secret-purpose application check; neither can create an admission. The queue
validation additionally requires the keeper's still-owned original `set`
command. An explicit edit callback must find the keeper's original bounded
command and its still-current full device session. It cannot restore that
decision from a worker-supplied tuple, a status read or an old receipt. Keeper
command ownership ends after a terminal worker receipt or a bounded ambiguous
outcome; a new explicit decision is required after it ends.

Every successful callback repeats exactly the account reply shape above. Bodies
and replies are at most 12 KiB. The worker measures a five-second deadline from
its callback request start; the keeper also limits its account exchange to the
same maximum from its request start. There is no refreshed TTL on receipt or
cached positive reply. A private local proof is consumed once, under the same
owner that rechecks the exact pending identity and registration and persists
the change. Expiry before that effect leaves the queue unchanged. Once durable
application begins, its cleanup and floor-confirmation continuation remains
owned even when the observer disconnects or the proof subsequently expires.

Only the trusted encrypted queue stores the admission tuple and authority.
Automatic application reuses that exact original bundle after restart, while
retirement preserves it until positive account acknowledgment. A lost admission
or queue response never causes value resubmission. Catalog and operation reads
remain passive; controller recovery may finish an already durable applying
transition but cannot manufacture a new queued intent or execution grant.

## Acceptance gate

Capability enablement requires the real account, keeper, supervisor, daemon and
native/browser control flow: queue through restart and refresh, cancel, explicit
apply, positive idle fencing, revocation during lock waits, ambiguous replies,
stale same-account edits and credential replacement. Verify two projects with
different values for the same name through actual PTYs, structured chat, Git,
plugins/MCP and process/file/network boundaries. Confirm selected-project
values never reach unselected projects or the personal provider coordinator.
Drive light/dark and narrow layouts, multi-project partial results, unsaved input
cleanup and passive polls while compute is unavailable. Unit checks alone do
not enable this contract.
