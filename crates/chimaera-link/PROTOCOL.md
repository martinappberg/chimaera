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
except the OAuth browser and token endpoints. Tokens never appear in URLs. Missing,
expired or revoked authentication returns `401` before a WebSocket upgrade. A
client refreshes once and retries; a second `401` requires sign-in. `403` means the
account lacks authorization or needs fresh multifactor authentication. Services
must check account and device ownership on every host and reverse stream lookup.

`me.protocol` is an integer major version. A client implementing v0 rejects any
other value before using the keeper. Unknown additive fields and event types can
be ignored. Breaking changes require a new major version. JSON uses UTF-8.
Errors use HTTP status codes and optionally `{ "error": "stable_error_code" }`.
REST response bodies are limited to 1 MiB; control frames to 128 KiB.

## Account routes

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /v1/me` | — | Account below |
| `GET /v1/devices` | — | Device array below |
| `DELETE /v1/devices/{id}` | — | `204`; revoke that device and its connections |
| `POST /v1/sign-out-everywhere` | — | `204`; revoke all devices, close held SSH logins and all link sockets |
| `POST /v1/billing/checkout` | `{plan:"pro"\|"max",interval:"month"\|"year",return_to?:"desktop",desktop_callback?:DesktopBillingCallback}` | `{url}` to hosted checkout |
| `POST /v1/billing/portal` | `{return_to?:"desktop",desktop_callback?:DesktopBillingCallback}` (or an empty body) | `{url}` to hosted billing portal |
| `GET /v1/worker/status` | — | Passive `WorkerStatus` below; full device authentication, no provisioning or wake |

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
contains no cell ids, service credentials or provider URLs. Automatic initial
preparation is a bounded account-service responsibility for eligible active or
trialing accounts; subsequent passive reads never wake a sleeping worker.

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
  "limits": { "cloud_hours": 100, "storage_bytes": 20000000000 },
  "usage": { "cloud_hours": 2.5, "storage_bytes": 1200000 },
  "hours_exhausted": false
}
```

`plan` is `none`, `pro` or `max`; cloud-hour usage is a nonnegative number,
limits and byte counts are nonnegative integers. Device rows are
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

### CLI device sign-in

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
must URL-encode them. Aliases have a maximum of 255 bytes.

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
Do not replay password answers after losing a connection.

## Data plane

`/v1/hosts/{id}/tcp` opens **one TCP connection** to that host's daemon. Unknown
host ids return `404`, unavailable hosts `409`, and exhausted stream quotas `429`.
Only binary frames carry data; each frame and complete message is at most 64 KiB.
Close/EOF closes the whole stream; **v0 does not support half-close**. A TCP writer
finishing its sending half must expect the receiving half to close too.

At most 16 data frames may be queued per direction. Stop reading the source when
that fills; never add an unbounded channel or accumulate a complete response.
Enforce transport limits before decoding. Each device has at most 128 concurrent
streams (including pending reverse opens). A loopback tunnel binds only
`127.0.0.1`, keeps one stable ephemeral port, and opens a new WebSocket per accepted
TCP socket. A failed stream must not destroy the listener or other streams.

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

Dropping/replacing the control connection closes its pending and active streams,
clears the in-memory daemon token and marks the device host offline. Reconnect
re-registers metadata. Signing out revokes both data and control sockets.

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

### Background handoff HTTP adapter

`/v1/hosts/{host_id}/http/{path}` accepts a device or daemon-delegation bearer and
forwards a narrow set of daemon HTTP routes. This lets a daemon perform background
handoff over ordinary HTTPS without depending on an app-owned local tunnel. The
keeper injects its in-memory daemon bearer, removes other caller headers except
Content-Type and the explicit wake marker, and never accepts a destination URL.

Allowed method/path pairs after `/http/` are `GET api/v1/sessions`,
`GET|POST api/v1/workspaces`, `GET api/v1/pro/bundles/{session_id}`,
`POST api/v1/pro/bundles/{session_id}/export`, `POST api/v1/pro/bundles`, and
`GET|PUT api/v1/pro/profile`. Bundle import accepts only `fork=true|false`,
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
