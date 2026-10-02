# Named provider authority v1

This optional contract separates personal-cloud Claude, Codex and GitHub login
from project runtime access. It does not enable a service, image or namespace at
startup. Ordinary laptop/free launches and older unisolated cloud deployments
retain their existing behavior. An isolated deployment must positively negotiate
the complete contract; it must never substitute a broad daemon bearer, shared
provider HOME or legacy tunnel when a required capability is absent.

The user keeps one named personal-cloud connection per provider. Project HOME,
configuration, history, tools and custom secrets remain project-local. Provider
credentials keep their provider-granted permissions; this contract does not
create provider-side project scopes or erase access tokens already issued to a
running process. It does not replace subscription OAuth with API-key billing.

## Independent capabilities

The exact positive versions are `provider_runtime:1` and `providers_control:1`.
Missing, false, unknown or partial capabilities refuse the relevant operation
before effects. Runtime support grants no login permission. Control support
grants no project execution permission. Protected local acknowledgments must
match version, account, holder, process boot and registration generation exactly;
an HTTP success alone is not acceptance.

All identifiers are 1–128 ASCII letters, digits, `_` or `-`; boot and operation
IDs are canonical UUIDs. Versions and generations are positive integers except
the initial disconnected connection generation, which may be zero. Arithmetic
overflow refuses; counters never wrap. Provider IDs are the closed set `claude`,
`codex`, `github`. Unknown commands refuse. Errors are fixed codes, never raw
HTTP/parser/process errors or provider response bodies.

## Personal control registration and transport

The trusted supervisor starts one fixed login coordinator outside every project
UID and bind mount. Its login HOME, CLI executable, arguments and working
directory come from the supervisor's fixed configuration. No browser, project,
device or keeper request selects a UID, path, executable, shell, helper, upstream
or port. Official login runs in a fresh attempt home, without project settings,
shell startup files or custom-secret overlays.

Registration is an optional authenticated worker-registration extension:

```json
{"providers_control":{"version":1,"account_id":"a-example","holder_id":"worker-example","process_boot":"00000000-0000-4000-8000-000000000000","registration_generation":1,"worker_credential_digest":"0000000000000000000000000000000000000000000000000000000000000000","capability":"opaque-random-secret"}}
```

The account and holder must match the freshly validated current worker
credential. `worker_credential_digest` is its SHA-256 digest, 64 lowercase hex
characters; the keeper computes and compares it from the validated bearer,
never trusts a claimed digest. It is an identity binding, not a bearer or
authorization proof. The keeper derives the fixed worker target from that authenticated
identity; no target address is carried by this extension. The supervisor persists
the monotonic registration generation outside project state before enrollment.
Registration replacement requires a greater generation; an equal-generation
heartbeat is accepted only for the same exact boot and capability digest. A
stale registration cannot overwrite a newer target. A changed current worker
credential, instance, boot or generation retires the previous registration and
cancels its in-flight commands before enrolling the replacement. Unregistered,
recovered, expired or unsupported targets have no control authority.

The capability is at least 256 random bits, at most 256 ASCII base64url bytes.
It stays in supervisor/keeper memory and the coordinator's startup channel. It
never appears in Host/daemon listings, browser/native responses, project state,
URLs, journals or logs. It is distinct from a project runtime capability and
every ordinary daemon token. A retained internal admission pins the exact
registration, current account credential and cancellation owner; substituting
caller headers for this admission is forbidden.

The external keeper API is a fixed JSON adapter, not a TCP tunnel or wildcard
path proxy:

| Method and path | Purpose |
| --- | --- |
| `GET /v1/personal/providers` | Cached redacted connection catalog and exact control capability acknowledgment |
| `POST /v1/personal/providers/commands` | One closed control command |
| `GET /v1/personal/providers/operations/{operation_id}` | The authenticated device's bound operation status |

Only an account-authenticated device credential may call it. Daemon delegations,
workspace/viewer scopes, project bearers and runtime capabilities refuse.
Mutations freshly validate the device with the account and recheck the current
registration before forwarding; account outages refuse. Device/account
revocation cancels its pending login attempts and prevents their credential
publication. It does not silently disconnect an already established named
provider connection. Explicit Disconnect is the cloud-wide removal operation.

Each pending login has a keeper-owned admission, independent of the browser
response lifetime. Its opaque random nonce binds account, device/session epoch,
worker credential digest, registration generation and immutable operation ID.
The supervisor retains that admission; runtime capabilities cannot name or mint
it. Its monotonic lease is at most 30 seconds and its entire attempt at most 15
minutes. The keeper renews only while its owned pending-login validator proves
the same authority; revocation, registration replacement or unavailable
validation closes it or lets that bounded lease expire. Polling remains passive
and does not itself mint or restore login admission.

An unexpired lease governs bounded login-tree cleanup, not successful canonical
import. After the official login tree is stopped, publication requires a fresh
keeper authorization exchange for the exact admission nonce, operation,
device/account epoch, worker credential digest and registration generation.
The keeper freshly checks account authorization using its existing ordered
account/epoch locking semantics; cached device validation cannot authorize this
exchange. Missing/replaced authority or an account outage refuses publication.
Only the bounded pending attempt may remain until its deadline. The authorization
reply is private, single-use and exact-bound; root import then also checks the
connection generation and durably records its operation receipt. It cannot be
replayed for another provider, account, registration or operation.

The account's fresh authorization check is the finite cross-service
linearization point: signout before it refuses import; signout after it cancels
remaining admission/use but cannot undo an already authorized local publication
or provider effect. The contract does not promise instantaneous distributed
rollback. A later account/worker generation must never consume that reply.

The browser gateway exposes the equivalent same-origin personal routes under
`/home/providers`, using its existing authenticated browser session and mutation
Origin/CSRF checks. Native uses the device-authenticated typed personal adapter.
Neither receives a coordinator bearer or falls back through `/hosts/{id}/tcp`.
The existing named Connections UI invokes this personal transport even when
opened from a project. Project daemons must not run Connect/Disconnect locally
or treat a scoped delegation as permission; missing integration gives an
actionable upgrade/control-unavailable answer before effects.

Passive catalog/status reads never wake, refresh a token or run a login CLI.
Connect, Submit and explicitly confirmed Disconnect may wake through existing
account attendance/allowance rules. Polling never repeatedly wakes a machine.

## Fixed login-only consumer

The supervisor gives the fixed control daemon a private startup pipe followed
by EOF, at most 4096 bytes, with version, exact account/holder/boot/generation
and current worker credential digest plus the control capability. It must be a pipe, read within three seconds and
consumed before serving. The coordinator exposes only the fixed adapter on its
supervisor-owned Unix socket. That socket and login HOME are never mounted in
a project. Ordinary HTTP bearer authentication cannot enroll this consumer.
The supervisor verifies its exact startup acknowledgment from the daemon it
actually launched before advertising `providers_control:1`.

A command has this common envelope (at most 8 KiB):

```json
{"version":1,"operation_id":"00000000-0000-4000-8000-000000000000","provider":"claude","expected_connection_generation":0,"command":{"type":"connect"}}
```

Commands are `connect`, `cancel {attempt_id}`,
`submit {attempt_id,submission_nonce,code}` and
`disconnect {acknowledge_cloud_work:true}`. Poll is a read, not another command.
The fixed consumer requires the exact login-only admission in addition to this
body. It maps an immutable operation ID and nonsensitive request digest to one
attempt; an identical retry observes the same attempt, while a changed
nonsensitive body with the same ID refuses. Submitted codes/passwords are
explicitly excluded from durable digests. Operations bind the authenticated
device and registration generation.
A caller cannot poll, submit to or cancel another device's attempt by naming it.
Generation mismatch requires fresh explicit intent, never an automatic retry
against a newer connection. Cancellation cannot release the writer/cleanup
reservation while a login tree or credential publication is still running.

The existing redacted attempt fields remain `id`, `provider_id`, `operation`,
`phase`, `expires_at`, `action`, `error_code`. Added authority fields are
`control_version:1`, `connection_generation`, `credential_revision` and exact
`registration_generation`. Browser URLs/device codes retain existing curated
origin validation. Submitted codes/passwords are bounded live memory only:
1–4096 bytes, no controls; they never enter status, durable operation digests,
logs or diagnostics. A submission nonce records consumption without persisting
the value. Replaying a consumed submission nonce returns its status even when
the retry carries a different secret value; it never compares persisted secret
hashes or resends either value to a CLI. No general password/shell input command
exists.

Login attempts last at most 15 minutes, one writer per provider. At most 24
attempt/operation records are retained and at most 16 requests/streams are
admitted; a full table refuses before starting a child. Cleanup has a bounded
owned continuation, and unconfirmed cleanup keeps that provider fenced. Browser
disconnect does not silently terminate a completed connection or release an
in-progress critical write.

After the official CLI completes, the coordinator stops and reaps the entire
login tree before importing the fixed credential leaf. The reader pins the
attempt-home descriptors, refuses links/FIFOs/special files, and caps each leaf
at 128 KiB. It selects only the validated provider schema; sibling history,
configuration, SSH/Git stores and arbitrary JSON are never copied. Claude imports
subscription OAuth access/refresh, expiry, scopes, account UUID, selected
organization, subscription type and rate tier. Codex imports ChatGPT OAuth
access/refresh/ID token and matching user/selected account/plan, refusing API-key,
PAT or other billing-mode substitution. GitHub obtains only the exact github.com
token/identity from the official CLI in its trusted login home; it invents no
refresh grant. Secret payloads cross only a supervisor-private authenticated
channel; they never pass through keeper/account/browser/native APIs.

The broker commits a complete validated leaf under exact account, freshly
authorized login admission and expected connection generation. It fsyncs the atomic credential
publication before reporting Connected. Disconnect first durably advances its
generation/tombstone and cancels old runtime transports; bounded official
cleanup follows. A late login/refresh cannot resurrect the previous generation.
Credential refresh changes the revision, not the connection generation. Lost
refresh replies retain uncertainty and require sign-in rather than replaying a
possibly consumed rotating refresh token.

## Runtime attachment and launch

The supervisor creates a per-project Unix socket at the fixed namespace path
`/run/chimaera/providers.sock` through a narrow bind. A random capability maps to
the retained opaque ProjectAdmission: exact account, workspace, registration
revision, launch generation, monotonic expiry and cancellation, plus allowed
provider/connection generations. No serialized workspace/UID/header claims can
mint that admission. Socket path and connection generation come from the verified
startup binding; project inputs cannot redirect them. Registration replacement,
expiry, namespace death or provider disconnect revokes before subsequent effects
and closes old transports. Peer credentials/socket identity supplement this
binding; knowing a UID alone grants nothing.

The daemon acknowledges exact runtime version and binding before any provider
launch. The launch overlay is applied after user shell/prelude processing and
cannot be shadowed by custom secrets, project configuration, TLS/proxy overrides
or a different provider HOME. Conflicting selected-secret names are visibly
refused before launch; other selected custom secrets remain project-specific.
Provider files remain in the project-private HOME/configuration/history. The
actual upstream origins/executables are fixed in the trusted adapter.

Codex 0.159.3 runs its app-server inside the project, behind a bounded JSON-RPC
authentication shim. Structured chat uses its stdio interface; the stock TUI uses
its project-local remote Unix interface. The shim bootstraps external ChatGPT
tokens and handles `account/chatgptAuthTokens/refresh`; auth messages/tokens never
reach browser sockets, journals or transcripts. It preserves same user/selected
account identity, request IDs, resume/cwd, approvals and normalized driver events.
A callback has ten seconds; an admitted central refresh may finish after that
observer times out, but delivery still rechecks admission. It does not expose a
project-callable unauthenticated rotating-refresh method.

Claude 2.1.287 uses a project-local frontend to the fixed message gateway and an
opaque capability in `CLAUDE_CODE_OAUTH_TOKEN`. The gateway replaces incoming auth
with the current actual subscription OAuth bearer for the fixed provider origin,
preserving reviewed OAuth beta headers, body and SSE semantics. It permits only
the pinned reviewed inference/token-count routes, rejects account/login mutation,
arbitrary paths/origins and redirects, and never substitutes ANTHROPIC_API_KEY.
No prompt or response bodies are stored in the shared authority. Project-private
trust/onboarding settings are independent of the personal login HOME.

GitHub uses a fixed trusted gh launcher and HTTPS Git credential helper. A token
is delivered only to the admitted child or exact HTTPS github.com credential
request, not inherited by every shell. GH configuration/history remain local.
Project `gh auth login/logout` cannot change the named connection. Rejected writes
fail visibly without automatic replay. Access-token exposure necessary for a
stock CLI does not give it refresh-token or personal-control access.

Runtime requests/bodies are bounded (control 128 KiB, inference 16 MiB), stream
frames at most 64 KiB and queues at most 16 frames; maximum 16 retained operations
per broker. Upstream connect/refresh timeouts and idle-read timeouts are finite;
every stream is additionally bounded by its exact admission deadline. Owned
provider activity participates in supervisor drain/idle publication. Passive
status does not wake or refresh credentials, and refresh is never a periodic
reason to wake an idle worker.

## Migration and enablement gates

Before importing an existing official cloud login, the supervisor exclusively
quiesces every old cloud CLI credential-refresh owner and verifies cleanup. It
pins the original leaf, imports only that bounded schema and durably records the
broker generation and migration receipt. Restart reuses the receipt; it cannot
recapture a partially migrated or refreshed leaf. Original login files remain
outside project binds and are retired from CLI refresh use before the broker is
sole owner. Missing/ambiguous cleanup or schema refuses migration without
silently logging everyone out. All cloud CLI consumers of that named connection,
including unisolated personal work, must use the broker after migration; an old
CLI cannot keep refreshing a duplicate store. Local laptop connections are not
part of this migration.

Enablement requires independent source review and synthetic integrated tests:

- Positive exact registration/ack; absent/mixed versions refuse with no fallback.
- Device/browser auth, CSRF, delegated/project denial and same-account operation
  ownership; revoked credentials/replaced registrations refuse stale writes.
- Existing Connections UI from Home and project context reaches personal control;
  poll remains passive, explicit Connect/Disconnect preserves attendance rules.
- Official-login schema import without config/history, unsafe-leaf refusal,
  quiesced migration crash/restart and no duplicate refresh owner.
- Two real project namespaces, separate HOME/config/history/secrets, stock Claude
  and Codex TUI plus structured chat, resume, simultaneous rotation and Git/gh.
- Revoke/expiry during dial, callback, stream, login and publication; observer
  cancellation retains bounded permits/cleanup, no stale-generation resurrection.
- Lost/truncated refresh reply, disk failure and parser errors remain secret-safe
  and fail closed; partial migration remains recoverable/actionable.

Synthetic tests use task-owned homes and loopback fake providers. They do not
prove live subscription billing or hosted deployment readiness. Actual provider
acceptance needs separately authorized vendor calls; this contract alone does
not authorize them or enable startup.
