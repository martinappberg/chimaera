# Device link protocol, version 0

This is the public contract for an optional authenticated account and keeper
transport. It carries the existing daemon HTTP and WebSocket protocols unchanged.
The client starts no background work until explicitly configured and signed in.

## Origins, versions and authentication

The configured origin is the **account**. `GET /v1/me` returns the **keeper**
origin. Account routes below use the former; hosts and WebSockets use the latter.
Both can be the same origin. `keeper_url` may be an empty string while no keeper
is assigned (for example, before activation). Account and device operations still
work; transport operations report that the connection is being prepared. Origins must have no credentials, path, query or
fragment. HTTPS/WSS is required; only literal `127.0.0.1` allows HTTP/WS for local
fixtures. An HTTPS account cannot redirect a bearer token to an HTTP keeper.
Clients do not follow HTTP redirects on authenticated requests.

Every request and WebSocket upgrade uses `Authorization: Bearer <access_token>`
except the OAuth browser and token endpoints and the public `GET /v1/plans`
(which takes no credential; a client never sends one it holds). Tokens never
appear in URLs. Missing,
expired or revoked authentication returns `401` before a WebSocket upgrade. After an
**account** `401` a client refreshes once and retries; a second `401` requires
sign-in. A keeper `401` alone does not justify a rotation: keepers also refuse
during account outages or with a stale revocation cache, so the client first asks
the account (`GET /v1/devices`, side-effect free) and refreshes only if the
account also answers `401`; this holds for keeper HTTP routes and WebSocket
upgrades alike. While its account is unreachable a keeper answers new work with
`503 {"error":"account_unavailable"}` (existing SSH logins and streams are kept):
clients treat it as transient, never as a reason to refresh or sign out, and the
daemon's automatic return waits quietly for the next pass. `403` means the account lacks authorization or needs
fresh multifactor authentication. Services must check account and device
ownership on every host and reverse stream lookup.

`me.protocol` is an integer major version. A client implementing v0 rejects any
other value before using the keeper (typed `ServiceUnsupported`). Breaking
changes require a new major version. JSON uses UTF-8. Errors use HTTP status
codes and optionally `{ "error": "stable_error_code" }`. REST response bodies are
limited to 1 MiB; control frames to 128 KiB.

**Additive evolution.** Services may add response fields and new enum values at
any time. Clients decode service responses without `deny_unknown_fields` and map
an unknown enum value (plan, worker state/reason/phase, host kind/status,
placement availability, continuation) to `Unknown`, which they treat as "not
actionable" rather than failing the whole response. Host rows of an unknown
kind are dropped; unknown event and reverse-serve message types are ignored. Only
acknowledgments from the local daemon stay exact (an old daemon that ignores a
field must never look like it accepted it). A service that lacks a required
route (404 on `/v2/capabilities`, or a 404 without the documented error code)
is reported as `ServiceUnsupported`, never as a transient failure.

## Account routes

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /v1/me` | — | Account below; for a paid account it also marks the keeper as in use (it is not a free status probe) |
| `GET /v1/plans` | — (no credentials) | Public `{plans}` catalog below; answers before sign-in |
| `GET /v1/devices` | — | Device array below |
| `DELETE /v1/devices/{id}` | — | `204`; revoke that device and its connections |
| `POST /v1/sign-out-everywhere` | — | `204`; revoke all devices, close held SSH logins and all link sockets |
| `POST /v1/billing/checkout` | `{plan:"pro"\|"max",interval:"month"\|"year",return_to?:"desktop",desktop_callback?:DesktopBillingCallback}` | `{url}` to hosted checkout |
| `POST /v1/billing/portal` | `{return_to?:"desktop",desktop_callback?:DesktopBillingCallback,target?:{plan:"pro"\|"max",interval:"month"\|"year"}}` (or an empty body) | `{url}` to hosted billing portal |
| `GET /v1/worker/status` | — | Passive `WorkerStatus` below; full device authentication, no provisioning or wake |

A portal `target` opens a hosted subscription-change **review** for the account's
existing subscription. The service chooses the owned subscription/item and its
configured price; clients cannot supply customer, subscription, item, or price
IDs. Creating the portal session never changes the subscription. The hosted page
shows the final amount and timing and requires explicit confirmation. Existing
trial time is preserved. The native return retains `outcome=portal`; clients
confirm the requested plan using a fresh authenticated `/v1/me`, never from the
return alone. A generic portal request remains unchanged.

`WorkerStatus` is `{state,reason,phase?}`. `state` is `no_plan`, `unavailable`,
`preparing`, `ready`, `sleeping`, `limited` or `error`. `reason` is null or one
of `provisioning_disabled`, `beta_invite_required`, `hours_exhausted`,
`storage_exhausted`, `spend_limit_reached`, `provisioning_failed`. An inactive
subscription is `no_plan`; disabled provisioning or an uninvited staging account
is `unavailable`; an eligible initial allocation or start is `preparing`.
`ready` requires a started worker with recent authenticated worker acknowledgement;
it describes account-known compute availability, not daemon connection readiness.
Clients still verify the live cloud information before opening provider terminals.
`sleeping` describes an existing stopped/suspended worker without a pending start.
Quota restrictions are `limited`; a failed preparation is `error`. Clients render
their own fixed, actionable descriptions rather than vendor error bodies.

The additive `attended_actions:true` field is meaningful only with
`state:"limited",reason:"hours_exhausted"`. It means explicit opens and actions
may still wake and use cloud work, while unattended continuation is unavailable.
Cloud work pauses after interaction stops; background polls and refreshes never
count as interaction. Missing/false retains the older complete-block behavior.
Ignore the field for any other state or reason; it cannot override subscription,
storage, spending or provisioning restrictions. This is policy information,
not proof that a worker is currently running or a provider is connected.

`phase` is optional and emitted only with `state:"preparing"`. It identifies an
account-confirmed stage, with no percentage or timing estimate:

- `keeper`: a matching started keeper cell and its registration are not yet ready.
- `worker`: the keeper is ready, but the worker is absent or not yet started.
- `connecting`: the keeper is ready and the worker is started, but a fresh
  authenticated worker acknowledgement is still pending within the startup grace.

Missing/null phase means preparation detail is unavailable; render the generic
preparing state. Ignore phase outside preparing. Existing state/reason precedence
is unchanged, and every other state omits phase. A phase is derived from the same
account database snapshot, without vendor calls or a wake. `connecting` does not
assert a live daemon connection or provider authentication. Older clients ignore
this additive field; older services remain valid without it. The native
`pro_cloud_status` command passes the typed status through unchanged.

This status read returns account-owned database state without calling Fly,
creating resources, refreshing desired machine state or waking a worker. It
contains no cell ids, service credentials or provider URLs. An eligible account
may have its connection ready before its first worker exists; status then reports
`sleeping`. First explicit cloud use or an eligible unattended continuation
requests that worker. Passive status reads never allocate or wake it.

The optional billing `return_to:"desktop"` selects a server-owned, credential-free
browser return page. Omission preserves the browser account return. An optional
`desktop_callback` is valid only with `return_to:"desktop"` and contains:

```json
{"redirect_uri":"http://127.0.0.1:49152/billing/callback","state":"<32 random bytes, unpadded base64url (43 characters)>"}
```

The URI must match that literal form exactly: decimal port 1024–65535, no
userinfo, query, fragment, other hostname/address, encoded path or extra path.
The app binds `127.0.0.1:0` before requesting billing and generates a fresh
cryptographic nonce for each attempt. Callback values are never logged. A service
validates both fields before any billing side effect and binds the attempt into
checkout idempotency, so retrying with a new listener cannot reuse an old return
URL. Missing callbacks remain compatible with older clients and services.
Checkout `409` means the account already has a subscription: the client exposes
typed `AlreadySubscribed`, and the native shell returns `use_billing_portal` so
the page re-reads the authenticated account instead of offering another checkout.
Service response bodies are never used as user-facing error copy.

The account's HTTPS return page automatically navigates to the validated native
callback, with an explicit return button as a fallback. Its short-lived, tamper-
evident return ticket carries no account/device credential and cannot select any
other redirect target. Callback navigation is a GET with exactly one `state` and
one `outcome` (`success`, `canceled` or `portal`). The app checks the exact Host,
path and state, rejects duplicate/unknown parameters, bounds request size and
read time, and consumes each callback once. Listeners and tasks are bound to the
signed-in account generation and canceled on sign-out/replacement. Waiting is
bounded to 15 minutes, followed by at most two minutes of payment confirmation.

**The return is only a hint, never proof of payment.** A native-owned, bounded
confirmation task refreshes authenticated account state even if the Pro pane is
hidden or closed. Checkout succeeds only when a fresh response confirms the
requested plan; portal return refreshes account state. A callback alone never
grants access. Cancel/expiry/error remain recoverable and do not silently reopen
checkout. No account token, Stripe session URL, or callback nonce enters UI state.

Account example (limits are supplied by the account, never hardcoded by clients):

```json
{
  "account_id": "a_example",
  "email": "person@example.invalid",
  "plan": "pro",
  "device_id": "d_example",
  "protocol": 0,
  "keeper_url": "https://keeper.example.invalid",
  "limits": { "cloud_hours": 40, "storage_bytes": 4000000000 },
  "usage": { "cloud_hours": 2.5, "storage_bytes": 1200000 },
  "hours_exhausted": false
}
```

`plan` is `none`, `pro` or `max`; cloud-hour usage is a nonnegative number,
limits and byte counts are nonnegative integers.

Optional additive fields, omitted by older services:

- `payment_due` (bool): the subscription needs a payment update. A service may
  instead send `subscription_status` (the billing provider's status); clients
  treat `past_due` and `unpaid` as payment due. A lapsed payment otherwise
  reads as `plan:"none"`.
- `returning_until` (RFC 3339 string or null): once a plan has ended, the time
  until which its cloud work can still be brought home. After it, the account's
  routes answer 403 `{"error":"return_window_ended"}`. Presentation only: a value
  that is not an RFC 3339 timestamp reads as absent, never failing the account
  read.
- `keeper_restart_at` (RFC 3339 string or null): the account's always-on keeper
  connection will restart to update. A future time is when; a time in the past
  means shortly, as soon as no Git transfer runs. The restart drops the SSH
  logins the keeper holds, so a kept host asks for its sign-in again on next
  use. Null (or absent) when no restart is planned; clients stop showing it
  when it returns to null. Presentation only, read like `returning_until`: a
  value that is not an RFC 3339 timestamp reads as absent.
- `plans`: the account's current offers,
  `[{plan:"pro"|"max",interval:"month"|"year",amount_cents,currency}]` with the
  amount in minor units and `currency` an ISO 4217 code. Clients never hardcode
  prices; without this list they show plan names only. An entry a client cannot
  interpret (a new plan or interval, a malformed amount) is dropped, never
  failing the whole account read. Each entry may also carry
  `cloud_time_multiple` and `storage_multiple` (positive integers): how many
  times the plan's monthly cloud time and storage are Pro's, rounded to a whole
  number and never below 1 (Pro's own entries carry 1), identical on both
  intervals of a plan. The list carries no absolute allowance, only these
  multiples relative to Pro; a subscribed account's own `limits` stay on
  `/v1/me`. Both are optional and omitted by older services; a value that is
  not a positive integer (zero, a string, a fraction, a negative, null) reads as
  absent for that field and never drops the entry.

**Public plan catalog.** `GET /v1/plans` is the same offers list without an
account, so a signed-out page can show prices. It takes no credential: a client
never sends a bearer it holds, and the route answers before anyone has signed
in. The body is `{"plans":[...]}` with the same
`{plan,interval,amount_cents,currency}` entries as `/v1/me`'s `plans`, with the
same optional `cloud_time_multiple` and `storage_multiple` (a client reads them
identically: an entry it cannot interpret is dropped, and it shows all four
prices or none; an older service without the multiples leaves them absent), or
`{"plans":null}` before the service's first successful catalog fetch. A priced
answer carries `Cache-Control: public, max-age=300`; a null answer and every
error are `no-store`. A `404` from an older service means no prices: never an error and
never a sign-in problem. A client treats the catalog as presentation only: it
asks at most every five minutes, waits for it in no other operation, and prefers
the `plans` of its own account once `/v1/me` has answered with one.

Device rows are
`{id,name,last_seen,this}` with `last_seen` an RFC 3339 timestamp and `this` true
only for the requesting device. Device tokens are account credentials; daemon
tokens below are separate, host-specific credentials.

### Browser sign-in and refresh

Use a system browser and RFC 8252 loopback callback. Bind `127.0.0.1:0` before
opening the browser. The redirect URI is exactly
`http://127.0.0.1:<ephemeral-port>/callback`, with no query, userinfo or fragment.
Generate independent cryptographically random state and PKCE verifier values
(at least 32 random bytes each). Retain them only for this sign-in attempt.

`GET /v1/oauth/authorize` takes `response_type=code`, `client_id=chimaera`,
`redirect_uri`, `state`, `code_challenge`, `code_challenge_method=S256`.
An optional `screen_hint` is exactly `sign-up` or `sign-in`; omission retains
the sign-in entry. It selects the identity provider's initial screen only and
never changes account authorization, MFA, PKCE, or billing. Unknown values are
rejected. Account creation/sign-in returns to the app; starting checkout always
requires a separate explicit action.
The browser signs in and completes mandatory multifactor authentication. It
returns `code` and the exact `state` to the callback. The client rejects absent,
duplicate or mismatched state/code and imposes a short callback deadline.
The account may use `GET /v1/oauth/callback` internally to finish its identity
provider's browser flow; that is distinct from the device loopback callback.

`POST /v1/oauth/token` consumes a **JSON** body:

```json
{
  "grant_type": "authorization_code",
  "code": "single-use-code",
  "redirect_uri": "http://127.0.0.1:49152/callback",
  "code_verifier": "original-random-verifier",
  "device_name": "My laptop"
}
```

Bind every short-lived code to the redirect URI and S256 challenge. Reject reused,
expired, mismatched or non-PKCE codes. `POST /v1/oauth/refresh` consumes
`{"refresh_token":"..."}`. Both return
`{access_token,refresh_token,token_type:"Bearer",expires_in:<seconds>}`.
Refresh rotates the refresh token; clients serialize refresh operations and
persist **every** replacement pair in the OS keychain. Account tokens never go
into app JSON settings, logs or the host directory. Local sign-out deletes the
keychain pair and terminates events, tunnels and reverse serve.

Refresh failures (RFC 6749 §5.2): an unknown, expired, revoked or already-used
refresh token returns `400 {"error":"invalid_grant"}`. The account treats reuse
of a rotated token as theft (it revokes the device), so a client must never
present that token again. Clients therefore treat **every 4xx except 404, 408
and 429** as final: clear the pair, publish sign-out, stop background work and
ask for a new sign-in. The typed client error is `AuthorizationRevoked`.

Everything else keeps the session and reports a transient error: `404` (a
missing route during a deploy), `408`, `429`, any `5xx`, a timeout, a
connection lost after sending and an unreadable success body. The account may
already have committed the rotation in any of these, so the client does not
present the token again within that operation; the next refresh happens only
when a later request is refused. The one immediate retry with the **same
token** (after a short pause) is a connection that was never established (DNS,
TCP or TLS), which provably never reached the account. The refresh request
allows 45 seconds, longer than an ordinary request, so a slow but successful
rotation is still received. A reply lost after a committed rotation leaves the
client holding a consumed token; healing that needs a service-side reuse grace.

### CLI device sign-in

The service implements this flow; no public client uses it yet.

`POST /v1/oauth/device/code` takes `{client_id:"chimaera",device_name}` and
returns RFC 8628 fields `device_code`, `user_code`, `verification_uri`,
`verification_uri_complete`, `expires_in` (600), `interval` (5).
`GET/POST /v1/oauth/device` completes browser approval after multifactor sign-in.
The CLI polls `POST /v1/oauth/device/token` with
`{grant_type:"urn:ietf:params:oauth:grant-type:device_code",device_code,client_id:"chimaera"}`.
Success is the token pair above; pending errors use RFC 8628
`authorization_pending`, `slow_down` or `expired_token`. Respect the interval and
expiry. Device-code endpoints do not require an existing device bearer token.

## Keeper REST routes

Capability-gated cluster control and keeper-owned job lifetimes are specified in
[CLUSTER.md](CLUSTER.md). They are additive; absent negotiation preserves the
ordinary host contract below. Host-bound key signing is not advertised by that
contract.

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /v1/hosts` | — | Host array |
| `POST /v1/hosts` | `{alias,ssh?}` | `201` host; an existing alias may return `200` |
| `DELETE /v1/hosts/{id}` | — | `204`; close that host's streams/login |
| `POST /v1/hosts/{id}/reconnect` | — | `204`; start reconnect asynchronously |

An optional `ssh` object contains `{hostname,user,port}` (port defaults to 22,
user may be null). Devices resolve local SSH aliases using bounded `ssh -G`
output, then send only this destination tuple. A keeper validates each field and
writes a strict SSH configuration containing only HostName, User and Port.
Never copy IdentityFile, ProxyCommand, credentials or arbitrary local SSH options.
Without `ssh`, the alias must already be a resolvable host on the keeper.

Host rows:

```json
{
  "id": "h_example",
  "alias": "cluster",
  "kind": "ssh",
  "status": "connected",
  "daemon": { "token": "in-memory-only", "build": "build-id", "sessions": 2 },
  "error": null
}
```

`kind` is `ssh`, `device` or `worker`; `status` is `connected`, `connecting`,
`prompting` or `offline`. `daemon` is null until authenticated daemon metadata is
available. `build` is the daemon build-id string, `sessions` a count. The daemon
token is delivered only to authorized devices over the protected link and retained
in memory only. `error` is null or a short user-safe explanation, never SSH output
containing passwords or key material. Host ids are opaque path segments; clients
must URL-encode them. Aliases have a maximum of 255 bytes. For `kind: "worker"`,
the registered route ID is exactly `worker-` followed by the account's worker ID.
That account worker ID is also the worker delegation's `device_id` and baton
`holder_id`; the prefix is not part of baton ownership. A device's reverse-served
row is likewise `device-` followed by its raw account device ID, and placement
routes use the same two forms ([VIEWING](VIEWING.md)). Clients may compare these
identities only for typed worker and device rows and must retain the complete
host ID for keeper requests, tunnels and placement routing. SSH host IDs are
opaque and have no such translation.

## Events

`/v1/events` is a bidirectional WebSocket, one per signed-in device. On every
connect the keeper sends a `host` frame for every currently known host, followed
by outstanding prompts belonging to the account. Consumers replace their host
snapshot by fetching `GET /v1/hosts` when reconnecting; a snapshot does not imply
that previously seen rows still exist. Events are notifications, not a durable
journal. A slow consumer is disconnected when its bounded queue fills and then
resynchronizes through REST. Replacing a device's connection closes its old one.

Keeper → device:

```json
{"type":"host","host":{"id":"h_example","alias":"cluster","kind":"ssh","status":"offline","daemon":null,"error":null}}
{"type":"host_removed","host_id":"h_example"}
{"type":"prompt","id":"p_example","host_id":"h_example","prompt":"Password:","echo":false}
{"type":"prompt_closed","id":"p_example"}
```

Device → keeper: `{"type":"answer","id":"p_example","value":"answer"}`,
or `value:null` to cancel. Prompt ids are unguessable and account-scoped. The
first valid answer wins; late/duplicate answers are ignored. A prompt expires
within 180 seconds, then emits `prompt_closed`. Answers are never logged or
persisted. Keep at most 64 outstanding prompts per account. Text is at most
8192 bytes. Cancellation and sign-out close prompt UI on every device.

Clients reconnect with jittered exponential backoff from approximately 500 ms to
10 seconds; reset after a stable connection. Each reconnection authenticates again.
Do not replay password answers after losing a connection. The Rust client exposes
connection-local opaque prompt IDs to its consumer and translates them back on
this wire; consumers answer the ID from the received prompt, not a keeper REST
response. A queued answer to an older connection is discarded even if the keeper
reuses its original ID.

Service behaviour clients must expect: the keeper closes every device stream and
drops every held cluster login when the account's session epoch changes (sign-out
everywhere, a replayed refresh token) or the account explicitly revokes access.
An unreachable account refuses new work with `503 account_unavailable` while
established logins/streams have a bounded fifteen-minute outage grace. It does
not report an outage as sign-out. A reconnect rebuilds the host rows, and logins may
prompt again.

## Data plane

`/v1/hosts/{id}/tcp` opens **one TCP connection** to that host's daemon. Unknown
host ids return `404`, unavailable hosts `409`, and exhausted stream quotas `429`.
Only binary frames carry data; each frame and complete message is at most 64 KiB.
Close/EOF closes the whole stream; **v0 does not support half-close**. A TCP writer
finishing its sending half must expect the receiving half to close too.

At most 16 data frames may be queued per direction. Stop reading the source when
that fills; never add an unbounded channel or accumulate a complete response.
Enforce transport limits before decoding. Each device has at most 128 concurrent
streams (including pending reverse opens); the client enforces this per signed-in
device across all of its forward tunnels and reverse streams together. A loopback
tunnel binds only `127.0.0.1`, keeps one stable ephemeral port, and opens a new
WebSocket per accepted TCP socket. A failed stream must not destroy the listener
or other streams, and neither does a failed `accept()` (descriptor exhaustion,
a reset before accept): the listener backs off briefly and continues. The native
app raises its open-file soft limit at startup (macOS starts GUI apps at 256).

Every socket, control and data, sends WebSocket ping at most 20 seconds apart.
Reply to ping with pong; 60 seconds without a pong makes the link dead. Bound
socket writes and opening handshakes so a stalled peer cannot retain tasks forever.

## Reverse serve

A signed-in device opens `/v1/serve` (one control socket per device). Its first
text frame, within 10 seconds, is:

```json
{"type":"register","alias":"My laptop","daemon":{"token":"local-token","build":"build-id","sessions":0}}
```

This registers the device as a `kind:device` host owned by the authenticated
account. Reply `{"type":"registered","host_id":"device-id"}` and broadcast the
host row. The alias and daemon token do not come from unauthenticated URL queries.

When another authorized device opens that host's `tcp` socket, the keeper sends
`{"type":"open","stream_id":"unguessable-single-use-nonce"}`. The hosting device
opens `/v1/serve/{stream_id}`, authenticating as the **same device** that owns the
control connection, connects to its own loopback daemon, and bridges bytes.
The keeper pairs the sockets. A stream id expires after 15 seconds and can be
consumed once. Bind it to account, device and current control-connection generation;
knowing another account's id must never authorize a stream. Close notifies with
`{"type":"close","stream_id":"..."}`. Unknown/expired streams return `404`.
A control message the device cannot act on affects at most one stream: an
unknown or malformed message is ignored, a duplicate `open` id is ignored, and
an `open` beyond the per-device quota is left unanswered (the keeper expires it).
None of these closes the control connection.

Dropping/replacing the control connection closes its pending and active streams
and clears the in-memory daemon token. The service keeper then removes the
device host (`host_removed`); the loopback fixture marks it offline. Clients
handle both. Reconnect re-registers metadata. Signing out revokes both data and
control sockets.

The events connection is kept alive for as long as its consumer exists. A
consumer that stays full for 10 seconds is not abandoned: the client drops that
connection and reconnects, and the reconnect snapshot replaces what was missed.

## Fixture and conformance

The fixture is excluded from default builds and binds only literal `127.0.0.1`:

```sh
cargo run -p chimaera-link --features fixtures --bin fake-keeper -- \
  --host cluster=127.0.0.1:9700
cargo run -p chimaera-link --features fixtures --bin link-conformance -- \
  --endpoint http://127.0.0.1:PORT --token fake-keeper-local-token --test-hooks
cargo test -p chimaera-link --all-features
```

`fake-keeper` prints its origin and static development token. `--listen` can pin a
loopback port for restart/reconnect tests. `--daemon-manifest alias=/path/to/manifest.json`
loads a real daemon token into memory without putting it on the command line; pair
it with the alias in `--host`. OAuth shows a one-button local sign-in
page. Test hooks require the fixture bearer token: `POST /_test/prompt`
`{host_id,prompt,echo}`, `GET /_test/answers`, `POST /_test/drop-events` and
`POST /_test/expire-access`. Answers are retained only in the fixture's bounded
in-memory test recorder. The fixture has no SSH implementation; `--host` maps an
alias to a real daemon already running on loopback.

The executable accepts any compatible account origin and device access token;
`CHIMAERA_LINK_TOKEN` avoids placing a token on the command line. It checks auth,
version negotiation, host/device decoding, events and reverse binary transfer.
`--host <id>` additionally checks authenticated real-daemon health through the
forward tunnel. Test hooks are opt-in and must never be implemented on production
services. The reverse probe temporarily advertises the conformance runner as a
device host; run it with a dedicated test device.

## Additive extensions

[Handoff extension v1](HANDOFF.md) defines the workspace baton and scoped mirror
credentials. It leaves the Link transport protocol number unchanged.

### Acting brings the work to you

Additive (2026-09-30): `POST|DELETE /v2/baton/{workspace}/move`, the
`move_to`/`move_requested_at`/`move_reason` fields of ownership answers, the
passive read's `ready`/`power` query, the `{"type":"bringing","to":"here"|"computer"}`
socket frame, the `bringing`/`still_working` refusal reasons and the `other`
flag of `moved`. Clients that ignore them keep today's behavior (the work
stays where it runs and a send wakes a sleeping cloud machine). In current
native clients, opening synchronizes a local copy and ordinary input stays
with the owner; only explicit Take over requests execution transfer. The
section title is retained for existing links. The contract is
in [HANDOFF](HANDOFF.md#explicit-take-over-moves-execution) and
[VIEWING](VIEWING.md#forwarded-requests).

### Sleeping worker HTTP transport

A worker's TCP WebSocket carries HTTP/1 with complete framing, keep-alive and
upgrade support. The keeper can answer cached, authenticated GET requests for
health, sessions, workspaces, journal and view state while the worker sleeps.
Cached documents are limited to 2 MiB each and 32 MiB in total. Responses carry
`X-Chimaera-Cache-Age: <seconds>` and `X-Chimaera-Worker-State: sleeping`.
Inner daemon-token authorization is checked even when a cache answers.

An unavailable worker and cache miss return HTTP 503 JSON
`{"error":"worker_asleep"}`. Passive reads do not wake the worker. Mutations
wake it; a deliberate interactive GET or WebSocket request can also use
`X-Chimaera-Wake: interaction`, or `wake=interaction` for browser WebSockets.
The keeper strips this intent marker before forwarding. `read_only=true`
always suppresses wake. Background health checks and viewer attachments must
never mark interaction. An awake daemon still requires its normal authenticated
WebSocket first frame.

### Sleeping worker sockets

Additive (2026-09-30). A keeper may keep a worker's sockets. One that does
marks every WebSocket upgrade it accepts for a worker host (101), awake or
asleep, with the response header `X-Chimaera-Sockets: kept`; a keeper without
it is the older kind, which answers a passive upgrade to a sleeping worker
with the 503 above and closes sockets on a suspension. Clients handle both and
tell them apart by that header only.

A keeping keeper accepts the upgrade of `/ws/chat/{id}`, `/ws/sessions/{id}`
and `/ws/events` whether the worker is awake or asleep, with or without the
wake marker, and keeps the client's side open across the worker's suspend and
resume. It remembers the client's first frame (folding a terminal's later
`resize`, `park` and `unpark` into it), holds a chat's acting commands, its
seven settings commands (`set_model`, `set_mode`, `set_effort`,
`set_ultracode`, `set_remote_control`, `set_mcp_enabled`, `reconnect_mcp`;
coalesced, the latest wins unless the user acted in between, outside the
four-command cap) and a terminal's typing whenever the daemon has not
answered `ready` on the current attach (nothing attached, or attached and not
yet `ready`), wakes the worker for them, attaches, replays the first frame (a
chat's `last_seq` raised to what it already relayed), waits for `ready` and
delivers what it held once, in order. An upgrade that carries the wake marker
wakes the worker even with nothing held. Every other chat command
(`set_thinking`, the reads, `cancel_send`, queue housekeeping, anything it
does not know) is never held and never wakes: dropped while nothing is
attached, passed through otherwise. A frame that does not fit a cap is
refused alone and the rest stay held. What it cannot deliver it hands back
with the daemon's own refusal frames (each carrying the `client_id` of the
command it refuses) followed by `remote_unavailable`, and keeps the socket
open. When the client's side closes while frames are still held it discards
them and never delivers them later; the client sends an unconfirmed send
again under its id at its next `ready`, and the daemon accepts an id at most
once. A keeper treats the id of a send it holds as accepted too: a second
copy is dropped silently (never refused, never held twice), a `cancel_send`
for it is answered `send_cancelled {client_id, cancelled:false}` by the
keeper and not passed on, and what a client sends while frames are held is
delivered after them. It adds no frame type: the one status it sends is the existing
`{"type":"waking"}`, once per wake. The rules, the caps, the send ids and
what clients do are in
[VIEWING](VIEWING.md#forwarded-requests) ("A sleeping cloud machine's
sockets"). HTTP requests are unchanged.

### Background handoff HTTP adapter

`/v1/hosts/{host_id}/http/{path}` accepts a device or daemon-delegation bearer and
forwards a narrow set of daemon HTTP routes. This lets a daemon perform background
handoff over ordinary HTTPS without depending on an app-owned local tunnel. The
keeper injects its in-memory daemon bearer, removes other caller headers except
Content-Type and the explicit wake marker, and never accepts a destination URL.

Allowed method/path pairs after `/http/` are `GET api/v1/sessions`,
`GET|POST api/v1/workspaces`, `GET api/v1/pro/bundles/{session_id}`,
`POST api/v1/pro/bundles/{session_id}/export`, `POST api/v1/pro/bundles`, and
`GET|PUT api/v1/pro/profile`. Profile GET adds `ETag` and `Cache-Control: no-store`; PUT accepts one exact quoted SHA-256 `If-Match` and returns 412 if the profile or account generation changed. Missing `If-Match` retains legacy unconditional behavior; malformed or multiple conditions return 400. This changes no JSON fields. Bundle import accepts only `fork=true|false`,
`origin=moved|home`, and unsigned `epoch` query parameters; profile accepts only
`workspace_id`. Session and workspace identifiers use letters, digits, `_` and
`-`, up to 128 bytes. Other routes, methods and query keys are rejected.

Bodies stream with a 128 MiB ceiling; response preparation is bounded to two
minutes. Authentication and host-generation revocation close in-flight streams.
Worker reads retain the sleeping-cache policy above; mutations and explicit wake
requests may start the worker. Direct SSH targets can use the same adapter.
Reverse-served device hosts return 409 when no direct target is available; they
remain accessible through the full TCP WebSocket transport.

The background HTTP adapter also permits `POST /api/v1/pro/handoff` with
`{workspace_id,expected_epoch}` for a paused worker to flush and release ownership.
Cloud setup uses the ordinary authenticated worker daemon: `GET /api/v1/pro/cloud`,
`POST /api/v1/pro/cloud/onboard` with `{agent:"claude"|"codex"|"github"}`, and
`POST /api/v1/pro/cloud/project` with `{url,name?}`. A worker health response may
include `pro_cloud_operations` so an in-progress clone counts as awake work.

### Isolated browser previews

An account browser workbench is served under `/app/{host_id}/` with an HttpOnly
browser session. It never exposes an account or daemon bearer to JavaScript.
Previewed applications use the separate per-account keeper origin.

`POST /app/{host_id}/browser/lease` requires that browser session, the account
Origin, and `X-Chimaera-Browser`; JSON `{proxy_id}` names an already-created,
target-pinned daemon preview. The response is
`{claim_url,grant,proxy_id,expires_at}`. The grant is single use, expires within
60 seconds, and is posted in a form to the keeper's `/browser/claim`, never put
in a URL. Optional form `path` is an origin-relative app path; the keeper may
redirect only under `/proxy/{proxy_id}/`.
Optional form `mode=refresh` renews the cookie and returns 204 without redirecting
or reloading the running app. Other mode values are rejected. A visible parent
may exchange another grant before expiry; a hidden preview is allowed to expire.

The keeper redeems it with service-authenticated
`POST /internal/v1/browser/claim {grant}`. The account returns
`{lease_token,host_id,proxy_id,expires_at}` and retains only hashes. The keeper
sets a Secure HttpOnly cookie named for that proxy ID. Its fixed lifetime is at
most 15 minutes. `POST /internal/v1/browser/validate {lease_token,proxy_id}`
returns `{host_id,proxy_id,expires_at}` only while the same account, device,
browser session and entitlement remain valid. Requests validate individually;
upgraded streams revalidate within ten seconds. Grants are capped at eight per
browser session. Wrong-account, expired, revoked and consumed grants return 401.

The preview cookie authorizes only its exact host and proxy ID. An absolute app
resource may use a same-origin Referer that names that ID; ambiguous cookie-only
routing is forbidden. Upstream cookies never become account/keeper credentials.
Browsers that block third-party cookies can open the preview in a separate tab.

### Account home in a browser

A signed-in account browser's `GET /` (and `HEAD /`) serves the same public UI
index as a project view, changed only by one first element in `<head>`:
`<meta name="chimaera-surface" content="account-home">`. The UI then shows the
account's Home and Settings rather than a workbench, since no daemon stands
behind that page. The index carries the same `X-Chimaera-Plan` header and
`Cache-Control: no-store`; its bundle is served under `/assets/` and
`/favicon.svg` exactly as under `/app/{host_id}/`. Without a live browser
session, `/` is the sign-in page.

Three routes serve that page. Each requires the browser session; none wakes a
keeper or a worker, and none accepts a host, a destination or a credential:

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /home/account` | — | `{email,plan,limits,usage,hours_exhausted,payment_due,returning_until}`: the `/v1/me` fields of those names; no identifiers, keeper address or prices |
| `GET /home/projects` | — | `{projects:[{workspace_id,name,href,available}],pending}` |
| `POST /home/sign-out` | — (the account Origin and `X-Chimaera-Browser: 1`) | `204`; revokes this browser's own device and clears its cookie |

`projects` lists the account's enrolled projects, whose `href` is
`/workspace/{workspace_id}/`, then any other project registered on a cloud
machine (never one the machine marks `cloud_internal`), whose `href` is
`/app/{host_id}/#ws={workspace_id}`. `name` is the owning daemon's registered
name, or null when it could not be read in time; `available` is then false and
the project still opens. An account without a plan that includes the cloud has
an empty list. `pending` means part of the list could not be read just now (a
keeper or machine that did not answer); clients ask again later. Clients follow
only those two `href` forms and ignore unknown fields.

The account's plan, billing, usage and devices page is `/account/billing`. The
former landing page `/account` redirects to `/`, or to `/account/billing` with
its query when it carries a known billing parameter. A browser sign-in lands
on `/`.

### Device display and installation binding

`GET /v1/devices` may include `installation_id` for an account-verified native
installation. Its absence denotes an unbound sign-in, not proof of another
physical computer. Names are display-only and must never be used for grouping or
authorization. `POST /v2/installations/bind` accepts optional `display_name`
(nonempty, at most 120 UTF-8 bytes, no control characters); only a successful
authenticated binding may update that device's label. The native app reads macOS
ComputerName instead of a network-assigned hostname. Older sign-ins remain
individually revocable; displaying them never revokes or merges credentials.
