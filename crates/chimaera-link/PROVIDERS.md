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

The fixed worker-private exchange is `POST
/v1/personal/providers/publications/authorize`. It requires the authenticated
current worker credential and an already positively registered personal-control
target; device, runtime and project credentials cannot call it. The JSON body
is at most 8 KiB, rejects unknown fields and contains:

```json
{"version":1,"account_id":"account-id","holder_id":"worker-id","process_boot":"00000000-0000-4000-8000-000000000000","registration_generation":1,"worker_credential_digest":"<64 lowercase SHA256 hex>","device_id":"device-id","device_session_epoch":1,"operation_id":"00000000-0000-4000-8000-000000000001","operation_nonce":"<opaque pending-login nonce>","operation_digest":"<64 lowercase nonsensitive SHA256 hex>","provider":"claude","action":"connect","expected_connection_generation":0}
```

IDs use the existing bounded control identity rules. Device/session and
registration generations are nonnegative exact integers; the registration
generation is positive. `action` is only `connect` or `disconnect`; the three
provider IDs above are the complete allowlist. Pending and publication nonces
are opaque base64url values of 43–128 bytes, generated by the trusted owners.
No credential leaf, password/code or secret-value digest enters this exchange.
The nonsensitive operation digest is the same immutable command identity used
by the keeper and coordinator. For Connect and Disconnect, SHA256 hashes the
operation ID, authenticated device ID, account ID, holder ID and process boot
in that order, each UTF-8 string prefixed by its eight-byte unsigned big-endian
byte length. It then hashes the eight-byte big-endian registration generation,
the 64 ASCII bytes of the worker credential digest, the eight-byte big-endian
expected connection generation, one provider byte (`claude=0`, `codex=1`,
`github=2`) and one command byte (`connect=0`, `disconnect=3`). There are no
trailing bytes for these commands. The device/session epoch is separately bound
by the retained keeper operation and exact fresh exchange tuple.

The keeper resolves every field against its live operation record created by
the original authenticated device intent. It retains that device validation
context in memory and freshly revalidates it with account before responding.
Worker-supplied device/epoch/account fields do not grant permission. It also
checks the actual worker bearer against the current holder/boot/registration
generation/credential digest under the registration gate. No matching active
operation, any changed authority or an account/worker outage refuses; no
unregistered or cached-validation fallback exists.

Before starting official login or Disconnect, the original explicit command
creates a private account admission through `POST
/internal/v1/personal/providers/admissions`. The caller is authenticated by the
current keeper service bearer, scoped to exactly one account. The closed JSON
body is at most 12 KiB and has exactly four fields: `authorization` (the complete
worker-private request above, itself at most 8 KiB), `device_token` (the full
device access bearer that authenticated that explicit command), `worker_token`
(the current registered worker service bearer), and `device_authority` (exactly
32 random bytes encoded as 43 unpadded base64url ASCII bytes). Both service/device
bearers are 16–256 ASCII base64url bytes. The keeper generates and retains the
random authority before its first request so an exact retry after a lost reply
can reuse it. Delegations, workspace grants and runtime capabilities refuse.
No caller supplies an expiry or extends an existing admission.

The account transaction locks the exact account first, then its keeper and
worker cell rows in that order, before checking the device and service credential
rows. It revalidates the presented keeper and worker hashes against the locked
current cells, the worker's exact holder ID, the device's current access token,
unrevoked session and exact account epoch, and the current paid entitlement.
Expiry is checked against the current database clock after lock acquisition.
Device revocation/token rotation and cell credential replacement use compatible
lock ordering; no external call or provider effect occurs while these locks are
held. A changed or expired credential, delegation, missing current cell, account
mismatch or absent entitlement refuses. The account does not infer a process
boot or registration from caller fields: the keeper must still match those
fields to its retained operation and registration after each fresh call returns.

The account stores only the random authority's SHA-256 hash, the exact immutable
operation tuple (with the pending operation nonce hashed), the original device
and account epoch, current keeper/worker credential hashes and database-clock
expiry. Admission lasts at most fifteen minutes from original creation. At most
eight unexpired admissions per device and twenty-four per account are allowed;
active records are never evicted. Expired nonsecret tombstones are retained for
at least twenty-four hours, with at most 256 retained records per account;
capacity refuses before another admission. Maintenance removes at most 1000
expired retained rows per pass. The record is shared durably across account
replicas/restarts; no process-memory cache grants publication authority.

A successful admission reply is at most 12 KiB and exactly
`{"version":1,"authorized":true,"authorization":<the complete exact request>,"expires_in":1..900}`.
The remaining lifetime is rounded down from the original database expiry. The
keeper's local deadline starts before its first admission request and uses the
returned remaining lifetime, additionally capped by the original command's
fifteen-minute absolute deadline; RPC time cannot extend login. Exact operation,
authority and tuple retries reuse that same original expiry without extension.
Changed identity/nonce/tuple or an expired record refuses; a retry or passive poll
cannot mint replacement admission. An evicted expired record cannot authorize a
publication; another explicit command requires a new operation and authority.
An exact already-admitted retry observes the original record after fresh
session/epoch/current-credential checks, even if routine rotation retired the
original access bearer. A missing record still requires a current full device
access bearer before creating admission.

Successful canonical publication uses `POST
/internal/v1/personal/providers/publications/authorize`, authenticated by the
current keeper service bearer. Its closed body is at most 12 KiB and has exactly
`authorization`, `worker_token` and `device_authority` with the bounds above.
The account takes the same ordered locks and compares the entire original
admission, current keeper/worker hashes, holder, unrevoked original device/session
epoch, expiry and entitlement afresh. Ordinary device access-token rotation does
not invalidate this original session admission; device revocation, account epoch
change and keeper/worker replacement do. This exchange never creates, renews or
restores admission. Success is at most 12 KiB and exactly
`{"version":1,"authorized":true,"authorization":<the complete exact request>}`.
After original admission, the keeper's active-login validator uses this fresh
session-authority check to bound its existing thirty-second pending-login lease;
it does not keep testing a retired access bearer. That background validation
starts no login, extends no database expiry, and does not mint a publication nonce
without an actual canonical-publication request from the matching live operation.

The keeper requires every response field and the full tuple to match, then
rechecks its current registration and live operation before minting the private
one-use publication nonce. Neither bearer nor the random authority is returned
to a browser/project, persisted in keeper operation records or included in
diagnostics. Legacy account validation replies without an explicit
`delegated:false` cannot authorize personal control; ordinary legacy data-plane
validation retains its existing compatibility behavior. Unsupported services,
network/parser failures and account lock timeouts refuse without fallback.

A successful bounded reply repeats the complete exact request tuple and adds
`authorized:true`, `publication_nonce:<new opaque one-use value>` and
`expires_in:1..5` seconds. It is sent only on that private authenticated
exchange, never to a browser. The worker requires the exact tuple, version and
positive acknowledgment. Its local monotonic deadline starts before the
exchange, using the returned lifetime, so network delay cannot extend the
receipt. Missing/mismatched fields, false acknowledgment, expiry and parser
errors refuse canonical writes. Error replies are fixed categories without
request bodies, raw worker credentials or provider diagnostics.

The root provider store consumes the publication nonce with the exact
expected-connection-generation CAS. Its durable nonsecret operation receipt and
complete credential or disconnected tombstone share the same atomic file
publication and directory flush. A lost reply can observe that exact committed
receipt; it cannot run another login, reapply a different credential or repeat
a disconnect. The store retains only its latest settled receipt per provider
(three total), including disconnected tombstones. Eviction never resets the
monotonic connection generation, so an older forgotten receipt cannot pass its
original expected-generation check. Counter exhaustion refuses new connections
and preserves revocation; it never wraps into old authority. The coordinator
retains at most 24 operations, evicts terminal records only, and expires pending
attempts at their existing fifteen-minute ceiling. After an evicted operation,
a passive query reports unavailable and a new explicit intent is required.
An expired authorization reply can be replaced only through another fresh
check of the same still-active exact keeper operation; an old nonce never
becomes valid again.

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

### Personal UI adapters and deployment selection

The canonical keeper paths and bodies above remain unchanged. The device Link
adapter validates their closed catalog and attempt schemas before the native
shell or account gateway maps them to the existing named Connections UI. A
catalog requires exactly three distinct known provider rows and an exact
version-one registration acknowledgment; each authority counter must be exactly
representable by the consumer. A partial catalog or ordinary daemon readiness
does not negotiate personal control.

The account exposes fixed passive `GET /v1/personal/providers/mode` for a full
device bearer and `GET /home/providers/mode` for the authenticated account
browser. Both return exactly
`{"version":1,"context":<64 lowercase hex>,"mode":"legacy"|"personal"}`.
The context is the SHA-256 of the fixed UTF-8 domain
`chimaera-personal-control-context-v1`, followed by the account ID and device ID
(each prefixed by its four-byte big-endian UTF-8 length), followed by the
original authenticated device session epoch as an eight-byte big-endian
unsigned integer. IDs satisfy the account's authenticated stable-ID rules; no
UUID-only assumption is made. Routine access-token or browser-cookie refresh
preserves this equality tag. Signout, device revocation, account replacement or
session-epoch change retires it. It is neither a selector nor authority; every
request separately revalidates ordinary authentication.

Mode comes only from trusted account deployment state, never optional worker
health, successful catalog discovery or a caller field. A newly selected adapter
requires a fresh positive mode reply. Missing, malformed or unavailable mode
refuses selection; it never probes a legacy route as error recovery. Existing
ordinary/free unisolated entry points remain unchanged until this new adapter
is positively selected. Personal mode stays latched for that authenticated
context, and all isolated project contexts use the personal account transport
even when Connections was opened inside a project. After personal selection,
404, unsupported capability, timeout, authentication failure or lost reply can
never retry or downgrade through a daemon bearer or host TCP tunnel. There is
no user-facing transport toggle.

This disabled fixture increment may use an explicit in-memory account mode
whose initial value is legacy. Production migration additionally requires
durable, non-downgradable per-account personal selection before isolated routing
or canonical credential import: process restart, absent configuration, an old
image or unavailable control capability must not restore legacy refresh owners.
Exclusive quiescence of those old owners remains necessary before import. No
production switch, startup enablement or migration is introduced by these
adapter definitions.

The browser exposes `GET /home/providers`, `POST /home/providers/commands` and
`GET /home/providers/operations/{operation_id}`. Native has separate personal
IPC entry points, never a project-daemon command. Catalog replies are exactly
`{"version":1,"context":<tag>,"catalog":<canonical keeper catalog>}`;
command/status replies are exactly
`{"version":1,"context":<tag>,"operation_id":<exact command or requested UUID>,"attempt":<canonical keeper attempt>}`.
The command reply echoes the submitted command UUID; a status reply echoes
the exact requested UUID, which may be the parent or a previously known child
alias. Both resolve to the parent's exact attempt.
There is one bounded catalog, with no pagination, profiles, credential values
or control capability. Replies are at most 64 KiB and command bodies at most
8 KiB. Browser mutations require the existing Origin/CSRF checks and exactly
one `X-Chimaera-Control-Context` header matching fresh normal authentication;
native mutations recheck that same tag and account generation before send and
after completion. Passive operation reads carry and compare that original tag.

The browser command body is the closed adapter-only envelope
`{"original":<original selector>,"command":<canonical keeper command>}`,
with the same 8 KiB total limit. The original selector is exactly
`{context,operation_id,provider,operation,expected_connection_generation,registration,attempt_id}`;
`attempt_id` is required nullable, and registration is the exact observed
`providers_control` tuple. An original-parent operation GET requires exactly
one `X-Chimaera-Provider-Original` JSON header, at most 4 KiB, carrying this
same closed selector; the path UUID must equal its parent `operation_id`.
These fields grant no authority. The adapter independently reauthenticates,
checks the context and account, and validates exact provider, generation,
registration and known attempt on the returned status. Status recovery does
not require a current catalog whose generation may already have advanced.
Duplicate headers, unknown fields and malformed selectors refuse with fixed
errors; headers and bodies are never logged. Canonical keeper wire is unchanged.

The client creates and retains an original parent operation UUID before Connect
or confirmed Disconnect. Its provider, expected connection generation, observed
registration and context remain immutable. Each Submit or Cancel creates a
distinct child command UUID once before its single send; reusing the parent's
UUID for a changed command is invalid. That child remains bound to the parent's
exact attempt ID, provider, generation, registration and context. A Submit also
creates one submission nonce before its one send; submitted code/password bytes
never enter durable state, status, digests or diagnostics, and clear on send,
hide and account change. An ambiguous mutation retains only the original
nonsecret parent identity and known child UUID. Recovery polls the original
parent; it never creates a fresh parent, resends a code or changes an expected
generation. A consumed submission nonce remains consumed even if a retry would
carry a different value. Browser opening rereads the original parent attempt
and applies the fixed provider-origin allowlist rather than accepting a caller
URL.

Account adapter errors are closed
`{"version":1,"error":<fixed category>}` objects. Categories are
`unsupported`, `invalid_request`, `state_changed`, `unavailable`,
`operation_unavailable`, `limit_reached`, `sign_in_required`, `context_changed`
and `unconfirmed`; raw keeper bodies and provider diagnostics are never exposed.
An HTTP rejection with no valid fixed object remains unconfirmed. Missing mode
or a missing catalog route may report unsupported before effects, but never
authorize a legacy fallback. Accepted mutation owners retain their four writer
and sixteen request reservations through actual HTTP completion after observer
cancellation; account replacement cannot race that retained native owner.

## Internal fixed worker command and lease exchange

The keeper's fixed adapter selects the worker address only from fresh account
worker identity and the current positively acknowledged registration. Its
internal paths are `GET /internal/v1/personal/providers`, `POST
/internal/v1/personal/providers/commands` and `GET
/internal/v1/personal/providers/operations/{operation_id}`. These are private
supervisor routes, never a project daemon route, generic path proxy or fallback
TCP tunnel. Requests require both the exact current worker service bearer and
the registered personal-control capability, carried respectively in
`Authorization: Bearer ...` and `X-Chimaera-Personal-Control`. Neither credential
is copied into response bodies, browser/native traffic or diagnostics. A worker
without this separate positively enrolled adapter refuses before effects.

The cached catalog is a closed object with exactly `providers_control` containing
the exact registration fields above without its capability, and `connections`.
`connections` contains exactly three distinct closed rows, one for each known
provider, each with exactly `provider`, `state`, `generation` and `revision`.
`state` is `disconnected`, `connected`, `needs_sign_in` or `recovery_needed`;
`generation` and `revision` are exact unsigned 64-bit integers, including zero
for initial disconnected state, with the existing nonwrapping counter rules.
Consumers unable to represent a counter exactly refuse actions using it rather
than rounding. These are broker connection states; the personal UI adapter maps
them to its existing named-provider status presentation. The catalog carries no
account/user profile, secret value, token, credential leaf or provider history.
The keeper
requires that exact registration acknowledgment before enrolling a target;
ordinary daemon health or an HTTP 200 alone is insufficient. Catalog and status
are bounded to 64 KiB and are passive. Unknown, missing or partial acknowledgment
refuses; registration discovery never itself runs a login command.

An internal command is a closed object with exactly `version:1`,
`authorization` and `command`. `authorization` is the complete original pending
operation tuple above, at most 8 KiB. `command` is the fixed login-only consumer's
closed command, at most 8 KiB; its provider, expected connection generation and
authenticated device must match that original tuple. The complete envelope is
at most 20 KiB, rejects duplicate/unknown fields and has no debug representation.
It never carries a selected process, HOME, executable, endpoint or upstream.
Submit codes remain bounded live memory and are excluded from durable digests.

Before forwarding a new Connect or Disconnect, the keeper generates and retains
the original pending-operation nonce, creates the exact durable account
admission and receives its positive acknowledgment. The operation ID and
nonsensitive command digest must match that tuple; Disconnect includes the
existing explicit acknowledgment. The worker then installs that immutable
identity into an opaque supervisor-owned operation, without starting a CLI or
writing a provider credential. Submit and Cancel refer only to the same live
original operation and exact device/epoch/provider/connection generation; they
cannot enroll another admission. The fixed consumer retains its existing
one-use submission nonce behavior, including a retry with a different code.

Before the first login effect, and for each renewal, the worker requests `POST
/v1/personal/providers/leases/authorize` at its fixed keeper. It authenticates
with its current worker service bearer and sends only the complete original
authorization tuple, within 8 KiB. The keeper matches that tuple to its retained
original explicit device operation and current worker registration, freshly
validates the existing durable account admission, then rechecks those exact
identities under its admission gate before replying. This endpoint never creates
or restores a grant or operation and is unavailable to browser/project tokens.
Account outage, signout, expired/terminal operation or replaced registration
refuses; no cached positive read permits renewal.

The closed lease reply has exactly `version:1`, `authorized:true`,
`authorization` repeating the complete exact tuple and `expires_in:1..30` seconds.
Its total body is at most 12 KiB. The keeper rounds down the minimum of thirty
seconds, the remaining original explicit-command lifetime and the remaining
durable account-admission lifetime, using its lease-request receipt/start as
the origin and subtracting elapsed authorization/gate time before the reply.
Less than one remaining second refuses. The original command deadline begins
at the keeper's accepted explicit user intent, never at later worker delivery;
neither first forwarding nor renewal may restart that fifteen-minute bound.
The worker's monotonic deadline starts before its lease request, uses that exact
returned lifetime and requires every identity and positive field to match.
Delayed round trips cannot extend either original deadline. An already expired
or canceled worker operation cannot consume a later reply; a renewable lease
cannot resurrect it. Renewal belongs to the retained operation owner and is
independent of any browser viewer or status poll. It neither wakes compute nor
starts a new login.

The command owner and lease validator survive an HTTP observer leaving, with
at most sixteen requests and four control writers retained through their actual
bounded effect/cleanup. The registration/account gate also covers worker
replacement and revocation, with fresh account checks repeated after any gate
wait before forwarding. Old captured commands cannot reach a new registration.
After login-tree stop and verified cleanup, canonical credential publication
still requires the distinct five-second one-use publication exchange above;
a login lease never substitutes for that proof or its connection-generation CAS.

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

### Supervisor-local login helper

The disabled `chimaera personal-provider-control` entrypoint delegates to the
fixed login consumer. Its only inputs are two inherited descriptors: the one-shot
startup pipe described above and a supervisor-private Unix socketpair. It accepts
no path, executable, HOME, upstream, environment override or TCP listener. The
socket is never mounted in a project. The startup pipe is consumed and closed
before a CLI child can start; the control descriptor is close-on-exec. The
supervisor verifies the exact registration ACK before advertising the capability.

Each socket message has a four-byte unsigned big-endian payload length followed
by one closed JSON object. Requests are at most 20 KiB and replies at most 128 KiB.
A complete request/read or reply/write has a five-second deadline. The helper
serves one bounded request at a time, while its separately owned login runners
continue independently of observations. Every request includes the exact
startup `capability`; it is never printed, returned or accepted as a project or
ordinary daemon bearer. The fixed request variants are:

- `type:"command"`, `device_id`, `command` (the existing fixed command object),
  and `lease_deadline_ns` (integer for Connect, null for Submit/Cancel). A helper
  cannot start a new login without that positive lease. Disconnect is performed
  by the private canonical writer and does not invoke a CLI logout helper.
- `type:"renew"`, `operation_id` (the original Connect UUID), and
  `lease_deadline_ns`.
- `type:"status"`, `operation_id`.
- `type:"credential"`, `operation_id`.
- `type:"release"`, `operation_id` (after canonical publication or confirmed
  failure, to discard the retained credential and login writer).
- `type:"shutdown"` (revoke all pending attempts and wait for bounded cleanup).

`lease_deadline_ns` is the absolute same-host `CLOCK_MONOTONIC` nanosecond
boundary computed from the worker's lease-request start plus the exact keeper
TTL. The helper verifies that it is future and at most thirty seconds away, and
uses that same boundary for its pending login. Renew does not resurrect an
expired/canceled attempt or exceed its existing fifteen-minute local ceiling.
Transport delay and helper reception cannot add time. Only the supervisor that
has just validated the exact original keeper lease sends command/renew; this
local deadline is not independent authority or a caller-selected duration.

The unsolicited startup reply is exactly `{type:"ready",registration}`.
Subsequent replies are closed variants: `{type:"status",status}` with the fixed
runner's redacted `attempt_id,provider,phase,action,error_code`; or
`{type:"credential",operation_id,credential}` with the bounded typed
credential-only leaf; or `{type:"released",operation_id}`; or
`{type:"stopped",cleanup_confirmed}`; or `{type:"refused",error_code}` containing
only a fixed category. A credential reply exists only on this private socket,
only after positive stop/reap/home cleanup and while the original lease remains
valid. It is never copied into a public status, catalog or HTTP response. The
supervisor retains the completion through the fresh publication exchange and
canonical generation CAS, then explicitly releases it. Lost responses cannot
create a second login or resend a consumed submission nonce.

The helper retains at most twenty-four immutable operation/alias records, evicts
only positively terminal records, and admits at most one writer per known
provider (three total). Unknown cleanup never becomes terminal merely because
its authorization expired. EOF, supervisor revocation or shutdown cancels
pending admissions; bounded cleanup retains a failed writer/home fence rather
than extracting a leaf or claiming success. Worker bootstrap must refuse a new
provider capability until any previous login owner has been stopped and its
cleanup positively established. No ordinary worker startup enables this helper.
