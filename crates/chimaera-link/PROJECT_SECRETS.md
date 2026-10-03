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
Catalog pages contain at most 64 projects so the maximum name lengths and both
applied/pending lists fit that ceiling; pagination is passive.

An operation UUID never authorizes re-submitting a value. Once accepted, the
trusted worker owns bounded persistence and cleanup even if the caller leaves.
A lost reply is resolved through the original operation status. A different
payload under an old operation ID refuses; responses never reveal whether a
guessed value matches stored data. At most 16 requests and four pending writes
are admitted per personal worker, with queue/backpressure limits retained until
the actual owned work and cleanup end.

### Native and browser account adapters

The keeper paths and closed bodies above remain unchanged. Account Home uses
additive native IPC or these same-origin account-gateway paths:
`GET /home/project-secrets` (the same optional `after` cursor),
`POST /home/project-secrets/commands`, and
`GET /home/project-secrets/operations/{operation_id}`. They never proxy through
a project daemon. Mutations require the existing browser origin/custom-header
checks; native IPC is restricted to the trusted local account-Home window.
Remote and project windows navigate to account Home instead.

Each catalog page is wrapped as exactly `{version:1,context,catalog}`; `catalog`
is one unchanged canonical keeper page. Pagination keeps its existing four-page,
128-project bound and requires the same context on every page. Command and
operation replies use `{version:1,context,receipt}` with the unchanged correlated
receipt. No adapter consolidates 128 projects into a nominal 64-project page.
Browser commands and operation reads require `X-Chimaera-Control-Context`;
native commands and operation reads require the equivalent IPC argument. The
command body itself is unchanged. A missing or mismatched context refuses before
forwarding, and a changed context while awaiting a reply cannot display success.
Browser errors are exactly `{version:1,error}`: the fixed keeper errors below,
or `sign_in_required`, `context_changed` or `unconfirmed`. Native IPC returns
only the corresponding fixed code. Errors contain no upstream body, request
field, value, serializer source or OS/network diagnostics.

The fixed additive account `GET /v1/personal/control-context` returns exactly
`{version:1,context}` after fresh full-device authentication; delegated access
refuses. `context` is 64 lowercase hexadecimal characters: SHA-256 over the
ASCII domain `chimaera-personal-control-context-v1`, then the account UUID and
device UUID as length-prefixed UTF-8 strings (unsigned 32-bit big-endian lengths),
then the original device session epoch as unsigned 64-bit big-endian. All three
fields come from fresh ordinary authentication, never caller fields or a new
refreshed-token issuance. The browser
adapter derives the same tag from its freshly authenticated browser identity.
It is stable across routine access-token/cookie refresh and account replicas;
sign-out, account/device replacement or session-epoch change invalidates the
old authority/context. Failed authentication clears the adapter's usable context.

This tag is only an opaque equality check for stale views, never a selector or
capability. Every adapter separately revalidates ordinary authentication and
binds forwarding to that exact identity; a supplied tag cannot select an account,
device, worker or keeper. Polling cannot admit an operation. The UI clears entered
values when submission starts, the editor closes or its context changes, never
persists them, and resolves ambiguous sends only through the original operation
read under the same authenticated context. Missing adapter/version support
refuses positively rather than falling back to a project route or resending.

### Redacted replies and catalog pagination

The external catalog is exactly
`{version:1,project_secrets:1,name_policy,projects,next}`. A project is exactly
`{workspace_id,revision,applied_names,pending,state}`. `revision` is its current
positive registered grant revision; this is the `expected_revision` used by a
command. `applied_names` and every pending name list are sorted and distinct,
with the existing 32-name limit. `pending` is required and is either JSON null
or `{operation_id,base_revision,names}`; the base revision must equal the shown
project revision. It describes staged additions/replacements, never values.

The closed project `state` is `ready`, `applying` or `unavailable`. Ready means
the registered grant may receive a fresh authorized command, not that a daemon
is running, idle or reachable. Applying means an owned durable transition is
incomplete. Its revision and names describe the stored transition, not usable
runtime values or an applied receipt. Unavailable covers a revoked, unenrolled or otherwise unusable
registration without exposing process/storage details. Neither non-ready state
permits a new command. A failed catalog request must not be rendered as an empty
project list or as removed access.

`name_policy` is exactly
`{max_name_bytes:128,max_value_bytes:8192,max_names:32,reserved_names,reserved_prefixes}`.
Both restriction lists contain at most 128 distinct sorted uppercase ASCII
names/prefixes, each at most 128 bytes; they are the actual worker restrictions,
not a client-maintained guess. The existing syntax rules remain mandatory. A
missing, malformed or unknown policy refuses value entry until a fresh supported
catalog is available. The worker still validates the name at the final command.

Rows are ordered by stable workspace ID. The first page has no cursor; a later
request uses the single optional query `after=<previous next>`, containing one
valid stable ID. `next` is required, null at the end or the last returned ID when
more rows existed. It is a pagination position, not a capability. A nonterminal
page must make progress. Clients collect at most four pages and 128 distinct
projects, rejecting duplicate IDs, reversed cursors or inconsistent policy.
Changes between pages may require an explicit refresh; pagination is not an
atomic catalog snapshot and cannot bypass command CAS. A status refresh never
starts, wakes, enrolls or changes a project.

A successful command or original-device operation read returns exactly
`{version:1,operation_id,workspace_id,base_revision,result_revision,names,outcome}`.
The names are the complete batch affected by that original decision. Required
`result_revision` is JSON null for queued/canceled outcomes, and the positive new
revision for applying/applied outcomes. An applying revision is still fenced;
only `applied` confirms cleanup and the remote floor. The closed outcome is `queued`, `applying`, `applied` or
`canceled`. A canceled queue keeps its original base revision and does not claim
an environment change. Unknown/invalid replies cannot become success. Clients
correlate the exact operation/workspace and original base revision before
displaying a receipt. Queued/applying reads may later advance; polling does not
advance them. A lost response remains visibly unconfirmed until such a receipt
establishes the result.

A stale-state command returns HTTP409 and exactly
`{version:1,error:"state_changed",project:<current redacted project or null>}`.
Null means no current usable catalog row was obtained, not deletion or cleanup
proof. The supplied operation remains unaccepted unless its original receipt
independently says otherwise. Missing, expired or wrong-device receipts share
the same fixed HTTP404 `operation_unavailable` response; never reveal another
device's operation. Other fixed errors are `unsupported`, `invalid_request`,
`limit_reached` and `unavailable`. Error bodies contain only `{version:1,error}`;
they never echo input, a value digest, OS output or serializer diagnostics.

The private catalog uses these same redacted rows, policy and pagination, plus
the exact `project_secrets_control` acknowledgment below. It never serializes
the registry's internal project state directly. Keeper strips that private
acknowledgment before external replies. Private command/receipt replies use the
same receipt shape; the keeper rechecks original device ownership and current
registration before exposing them.

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
retained. A retired or missing row cannot authorize application. Retirement of a
missing row creates a terminal tombstone for the exact supplied tuple and hashed
authority, under the same current-service checks and transaction locks as an
existing retirement. This is cleanup of an already durable worker preparation,
not a new personal admission or execution grant. The account enforces the same
1024 retained-row bound before creating this tombstone. Admission never
re-creates a retained retired operation, including when an earlier admission RPC
only reaches its transaction after cleanup.
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

The optional worker registration field is a closed
`project_secrets_control:{version:1,account_id,holder_id,process_boot,registration_generation,worker_credential_digest,capability}`.
IDs use the bounds above, process boot is a canonical UUID, and the registration
generation is positive, nonwrapping and durably advanced before a new control
lifetime. The digest is the lowercase SHA-256 of the current worker credential.
Capability is an independent random 256-bit, 43-character unpadded base64url
secret, retained only by this supervisor and its keeper; provider capabilities
cannot be reused. It never reaches a project, public reply or log.

Keeper accepts this field only from the account-validated current personal
worker, through its existing fixed registration route/address. It probes the
fixed private catalog with both that worker bearer and
`X-Chimaera-Project-Secrets-Control: <capability>`, then requires the exact closed
acknowledgment `{version:1,account_id,holder_id,process_boot,registration_generation,worker_credential_digest}`
before acknowledging registration with that same object. Every private route
requires both credentials without duplicate headers. Before registration ACK,
only this passive catalog probe is allowed; commands and receipt reads refuse.
Provider-only registration and the ordinary shared daemon cannot answer it.

A registration exchange owns one original five-second deadline including the
probe. Late replies cannot activate an expired/replaced registration. Exact
same-generation replay requires identical credential, capability, boot and
address; mismatched or lower generations refuse. Replacing or withdrawing the
current worker closes the previous shared revocation guard before subsequent
commands/callbacks. The worker's one-use authorization proof captures that same
guard and rechecks it at its serialized final effect, including after blocking
registry waits. An already durable Applying continuation retains cleanup
ownership but never restores the old registration. Process boot and control
generation are transient ingress bindings; they do not change the durable
queue's original admission tuple or invalidate it on an ordinary worker restart.

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

Only the trusted encrypted Registry durably stores the admission tuple and authority.
Automatic application reuses that exact original bundle after restart, while
retirement preserves it until positive account acknowledgment. A lost admission
or queue response never causes value resubmission. Catalog and operation reads
remain passive; controller recovery may finish an already durable applying
transition but cannot manufacture a new queued intent or execution grant.

## Durable preparation before account admission

Before the keeper's first account admission RPC for a `set`, the exact tuple and
random authority must already have a durable encrypted cleanup owner in the
worker Registry. The preparation contains no custom value or value fingerprint,
does not change applied or pending values, and is not a queued receipt. Only the
current supervisor's separately negotiated secret control accepts these fixed
private routes, with both worker bearer and secret-control capability as above:

| Method and path | Closed body and acknowledgment |
| --- | --- |
| `POST /internal/v1/personal/project-secrets/preparations` | `{version:1,authorization,device_authority,expires_in_ms}` → `{version:1,prepared:true,authorization,remaining_ms}` |
| `POST /internal/v1/personal/project-secrets/preparations/close` | `{version:1,authorization,device_authority}` → `{version:1,closed:true,authorization}` |

Both bodies and replies are at most 12 KiB. `authorization` is the exact closed
`set` tuple above; `device_authority` is its canonical 43-byte authority.
`expires_in_ms` and a successful `remaining_ms` are integers in `1..30000`.
The keeper supplies only the remaining original 30-second explicit-command
budget and retains its own original deadline. The worker fixes a monotonic
deadline from preparation request intake, including body, queue and storage
waits. Transit cannot extend keeper command authority: every eventual queue
still needs the original live keeper command and fresh account callback.

Preparation checks the current project revision, pending identity, resulting
name list and unchanged supervisor enrollment under the Registry's exclusive
writer. It fsyncs its exact immutable tuple and encrypted authority before
acknowledgment. The keeper must receive a positive exact preparation reply
before sending any account admission RPC. A retry for the same tuple and
authority can return only the existing remaining lifetime; it cannot restart
the deadline. A different tuple or authority, an expired/closed preparation or
a retained operation receipt refuses. Unsupported or lost preparation replies
never fall back to account admission or an older worker command.

The final queue write requires that same preparation to remain open after fresh
callback proof consumption. One Registry transaction promotes it into the
existing encrypted pending batch and real original-device receipt, preserving
the exact revision/pending/name CAS and reused-operation checks. The preparation
cannot be separately deleted before this promotion. A successfully queued batch
has no time expiry and retains the existing durable application/retirement flow.
Until promotion, external status remains unconfirmed: neither a preparation nor
its cleanup tombstone fabricates a queued, canceled or applied receipt.

Restart and secret-control withdrawal/replacement permanently close every
unpromoted preparation. The bounded monotonic preparation deadline also closes
it; a stored preparation cannot restore a deadline after restart. Explicit close
is an actor-serialized barrier: it persists the closed fence before acknowledging
and forbids later promotion under that operation. If a real operation receipt
already exists, close refuses and the real queued transition owns retirement.
Unknown or mismatched close requests also refuse. Preparation, close, expiry and
recovery never wake, launch, stop or edit a project.

The worker retains the closed encrypted cleanup recipe through lost replies,
outages and restart until the current keeper/account positively acknowledges
exact retirement. Only after closing/serializing against final promotion may it
send that retirement. The account's missing-row tombstone ensures a late old
admission RPC cannot recreate a live row. A keeper closes its original callback
decision before asking the worker to close; an ambiguous queue response first
resolves the original worker receipt, never resubmits the value. Recovery can
retire a preparation but cannot create or refresh a personal admission.

Preparing operations plus actual pending batches have a combined bound of 128;
all retained preparations and operation receipts have a combined bound of 1024.
Capacity refusal preserves older live work and cleanup recipes. Closed
preparation tombstones remain for at least 24 hours and are eligible for bounded
eviction only after positive retirement acknowledgment. An unresolved recipe is
never removed because its deadline or a correlation window expired. These
limits supplement the Registry's existing whole-file byte ceiling.

## Active-runtime automatic idle fence

This is a separately negotiated, initially disabled extension. Automatic apply
may first support only a positively stopped registry entry: no active or
quarantined runtime, daemon or published route. A running project stays queued
until the complete active-runtime fence below is verified. An ordinary health
reply, `/pro/drain`, handoff pause, execution quiescence or a startup cleanup
receipt does not advertise or prove this extension.

The supervisor owns one inherited, bidirectional control socket for the exact
project launch. It has no filesystem pathname and is not an HTTP route. Neither
a project bearer nor keeper forwarding can access it. Before accepting control,
the daemon sets the descriptor nonblocking and close-on-exec, prevents child
inheritance, and disables same-UID ptrace and `/proc` descriptor/memory access to
itself. The trusted launcher verifies those protections and drops project
ptrace capabilities. The supervisor retains its own endpoint and pinned daemon
pidfd. A changed executable, unsupported protection or missing channel refuses
active automatic apply; it cannot fall back to a bearer-authenticated endpoint.

Control messages carry no values, admission authority or service/device tokens.
Each closed request is at most 16 KiB; a closed reply is at most 32 KiB. There is
one active fence per project and at most four owned control operations. Their
capacity remains held through actual blocking preparation and cleanup, including
caller cancellation. Unsupported versions, duplicate fields and unknown commands
refuse. The exact binding is account, workspace, registration revision, launch
generation, OS boot and registered root device/inode, established by the trusted
launch. Requests cannot choose another process, executable, root or control URL.

The fixed `prepare` command additionally binds the original secret operation,
pending batch and expected applied revision. Admission closes under the same
in-memory lock as final command/input, spawn and mutation reservations. It also
excludes setup, import, profile/configuration, mirror/transfer and other owned
project effects through their final commit/registration windows. Existing work
must have actually settled; a canceled HTTP observer or an empty task queue does
not settle its owner. Passive reads remain permitted. New work is refused with
a fixed maintenance result before input is accepted or a process is spawned.

Only structured chats with authoritative idle/completed-turn evidence qualify.
Every session must have no active turn, accepted or queued send, pending input,
background task, permission/question or action wait, and a verified resumable
native conversation. Active external-input/Remote Control modes, terminal agent
UIs, hooks-only evidence, any plain terminal and unknown or unresumable sessions
keep the batch queued with Apply now available. Quiet output is never idle proof.
Preparation holds each chat's input-pause and lifecycle guard, captures its exact
spawned process identity, and reversibly SIGSTOPs only positively idle leaders
through pidfds. It never stops a whole process group by association. Already
produced protocol and journal tails must drain under a bound; strict idle is
checked again after that drain. Unexpected work aborts preparation.

Before returning `prepared`, the daemon durably records suspended conversations,
native resume identity, journals and delivery receipts, with a distinct manual
resume reason. This reuses the checked local conversation export primitives,
without Git publication, handoff, ownership/home/epoch change or process killing.
A high-entropy fence identifier binds that durable record and the exact stopped
leader identities; no caller-supplied PID gains authority. There are at most 64
sessions. A preparation has one absolute 30-second deadline measured from the
original request, including filesystem work. An exact retry returns the original
result and remaining deadline; it never extends or replaces the fence. Timed-out
or canceled preparation retains its owned guards until actual work settles and
cannot deliver late authority.

The supervisor then freezes its exact descriptor-pinned project cgroup and waits
boundedly for positive completion. A freezer request alone is insufficient. It
rechecks cgroup/root/launch currency and performs a bounded census of at most 128
processes, using pidfds, start identities and namespace-PID translation. Allowed
members are only the exact protected daemon, fixed trusted runtime plumbing and
prepared idle leaders. Any necessary agent wrapper must have an independently
verified exact identity; descendant/UID/process-group membership alone is not an
allowlist. A shell, detached child, replaced process or unexplained member keeps
the update queued. Fork/migration or identity ambiguity refuses. Project code
cannot migrate processes into or out of this supervisor-owned cgroup.

Only after that census does the serialized controller obtain fresh five-second
application authorization. Before its deadline it rechecks the exact pending
intent, registration, launch, prepared fence and stopped identities, durably
begins the existing applying/revocation-floor transition, and consumes its local
one-use fence. No daemon request is needed while the entire cgroup is frozen.
The owned continuation stops and verifies the complete old project runtime;
partial or ambiguous cleanup remains fenced and cannot yield an applied receipt.
No new launch or route can be admitted between the census and positive cleanup.
A successful update leaves the project stopped. Deferred chats are available for
explicit manual resumption; lease renewal, polling, restoration and a new cleanup
receipt never resume them automatically or send a pickup message.

Before durable applying begins, exact `abort` or expiry restores the original
suspension metadata and SIGCONTs only the same still-pinned old leaders. It never
starts replacement agents. The supervisor thaws only that original cgroup after
rejecting its application fence. Lost replies are reconciled against the same
operation and binding, not a new idle attempt. Once applying is durable, timeout,
channel loss or observer cancellation cannot reopen old authority; the controller
finishes positive cleanup or preserves recovery-required state. A daemon restart
retains durable parking until exact supervised recovery or an explicit user
resumption, never inferring success from a missing live process.

Suspended session rows add the nullable `manual_resume_reason` field. The fixed
value `project_secrets_idle` means this conversation was durably parked for this
maintenance flow; it does not say that values have been applied. Missing or null
preserves ordinary suspension. An unknown nonnull reason remains manual and
cannot be cleared by automatic recovery. Only the existing explicit session
Resume action may clear the reason under ordinary current execution authority.
For a retained manually parked session it calls the additive daemon endpoint
`POST /api/v1/sessions/{id}/resume` with no body. This is bearer-authenticated,
workspace-scoped and owned through ordinary exact account/execution admission;
it cannot create a new conversation or select another native resume identity.
Only a known deferred session with a manual reason is eligible. A successful
response is its normal session row under the same ID. It removes the reason
only after the original conversation and delivery journal have resumed and
the ledger change is durable. Missing/unqualified entries refuse; a duplicate
request for that already resumed exact session returns its normal row without
another spawn. Ordinary recent-conversation Resume keeps its existing flow.
Ownership recovery, provider readiness, polling and bulk workspace resumption
exclude these sessions and never send them a pickup message. An applied receipt
may explain that cloud secrets were updated and invite manual resumption;
preparation, parking or uncertain cleanup must not claim an update succeeded.

Enablement additionally requires real Linux process and restart fixtures: input
and spawn races, permission/background work, plain shells, detached descendants,
PID reuse, stale launch/root, failed drains/fsync, caller cancellation, freeze and
cleanup ambiguity, and delivery preservation without automatic resumption. A
sibling project must continue throughout. Kernel freezer and kill behavior is
specified by the [Linux cgroup v2 documentation](https://docs.kernel.org/admin-guide/cgroup-v2.html);
process census and daemon idle evidence are separate required proofs.

### Inherited idle control v1

The launch cleanup envelope gains only the optional closed field
`maintenance_control:{version:1,fd,channel_nonce}`. Omission preserves existing
startup behavior. `fd` is a non-stdio descriptor for the supervisor-created
unnamed connected Unix stream socket, not a caller-selected file or HTTP target.
The fixed launcher preserves exactly that descriptor through its exec gate.
The daemon validates its socket type and trusted launch provenance, sets
nonblocking/close-on-exec, excludes child inheritance, and establishes the
process protections above before enabling the channel.

Every message uses the exact existing inherited cleanup binding, restricted to:

```json
{
  "account_id": "ID",
  "workspace_id": "ID",
  "root_identity": {"device": 1, "inode": 1},
  "registration_revision": 1,
  "launch_generation": 1,
  "os_boot_id": "UUID"
}
```

Account/workspace IDs and root/boot validation follow that startup contract.
Registration and launch revisions are positive nonwrapping integers. The
`channel_nonce` and each `fence_id` are independent 256-bit random values,
43-character unpadded base64url, never logged or exposed to project code. The
sole server-first frame is exactly
`{version:1,type:"ready",binding,channel_nonce,project_secrets_idle:1}`. Supervisor
requires the exact selected nonce and binding within three seconds of starting
its channel handshake. Missing, repeated or mismatched Ready refuses the
extension. Ready describes support, not idle or execution authority. Prepare
also requires the daemon's current accepted scoped configuration, root/revision,
supervisor launch acknowledgment and captured account/execution generation.

Frames are a four-byte big-endian byte length followed by UTF-8 JSON. Reject
zero length, requests above 16 KiB, replies above 32 KiB, duplicate/unknown fields,
trailing JSON and unknown discriminants. Each request has a positive, strictly
increasing `request_id:u64`; exhaustion closes the channel. Replies echo it.
There is one serialized effect and at most four queued/owned requests; blocking
workers retain capacity through actual completion after observer cancellation.

All command requests and replies have these required common fields:
`{version:1,type,request_id,binding,attempt_id,operation_id,pending_id,
expected_applied_revision}`. The operation and pending UUIDs and applied revision
are the exact original secret intent, using the external bounds above. The
supervisor generates a separate canonical UUID `attempt_id` for one maintenance
attempt. That identity also belongs in the durable parking record. A new request
ID retries that same immutable attempt; it does not create a new idle decision.
The following table specifies all additional fields; no others are accepted.

| Request type | Additional fields | Meaning |
| --- | --- | --- |
| `prepare` | `expires_in_ms` | Admit one exact idle attempt with an original deadline of at most 30 seconds |
| `inspect` | None | Observe that retained attempt, without starting preparation |
| `abort` | `fence_id` | Roll back only that exact prepared attempt before durable Applying |

The supervisor starts its deadline before the first Prepare, and sends only its
remaining `expires_in_ms` in `1..30000`. The daemon also fixes a local deadline at
first admission. Exact retries retain both original bounds. A reply's remaining
time never extends the supervisor deadline, including across transport or
filesystem waits. A late preparation cannot deliver authority.

| Reply type | Additional fields | Meaning |
| --- | --- | --- |
| `busy` | `reason` | Positive no-fence result; metadata/leader restoration is complete |
| `prepared` | `fence_id`, `remaining_ms`, `leaders` | Exact durably parked attempt and positively stopped leaders |
| `aborted` | `fence_id` | Positive exact rollback; no replacement process was started |
| `expired` | `fence_id` | Deadline passed and exact rollback completed |
| `recovery_required` | `reason` | Cleanup or durable parking is unknown; admission remains closed |
| `conflict` | `reason` | Mismatched immutable identity or another retained active attempt |
| `not_found` | None | No retained receipt for that exact attempt; not idle/cleanup proof |

Busy reasons are the closed enum `active_turn`, `pending_input`,
`background_work`, `permission_wait`, `external_input`, `terminal_work`,
`setup_or_mutation`, `lifecycle_or_transfer`, `unresumable`, `process_unknown`,
`limit_reached`, `expired`. Recovery reasons are `parking_cleanup_unknown` or
`park_record_unknown`; conflict reasons are `identity_changed`, `binding_changed`,
`attempt_in_progress`, `outcome_unknown`. None includes OS output, a transcript,
command, path or environment. `remaining_ms` is positive and never exceeds the
original remaining deadline. Each leader is exactly
`{session_id,namespace_pid,start_ticks}`, with a valid session ID and positive
PID/start identity. There are at most 64 unique session/PID pairs. A wrapper or
extra descendant does not gain authority from this list. Even an empty leader
list requires the supervisor's complete positive process census.

An exact retry returns the original attempt's outcome and never repeats a stop
or export. Inspect cannot create a receipt. At most one attempt result is
retained per project/channel. A new maintenance attempt may replace only a
positive Busy/Aborted/Expired result for the same or a different pending intent,
only while the exact launch binding is current and no guards, stopped leaders or
admission latch remain. Prepared or RecoveryRequired cannot be replaced. The
controller compares the new attempt with its actual current durable pending
intent before sending; the attempt itself grants no secret-update authority.
New attempts require a separate serialized controller maintenance pass, at least
30 seconds after the previous attempt; a Busy reply
cannot recursively retry or spawn another pass. A missing older receipt grants
nothing. Unknown rollback retains its closed admission and recovery record.

Consume is deliberately not a channel command. The supervisor derives an opaque
local token from the exact authenticated Prepared reply, performs the pinned
frozen-cgroup census, and consumes that token once with fresh authorization and
the durable Applying transition under its existing controller owner. Its original
deadline and bindings remain mandatory after every wait. Abort/expiry can thaw
only before that durable transition and after positive exact rollback. After
Applying, failed kill, timeout or a lost acknowledgment must never thaw the old
cgroup or reopen its execution authority.

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
