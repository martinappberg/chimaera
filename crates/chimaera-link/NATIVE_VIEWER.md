# Native workspace viewer — disabled proposal

**Status: proposed, not implemented or enabled.** The endpoints and
`native_viewer` capability below do not exist in the reviewed baseline. Publishing
this document adds no route, capability advertisement, deployment, or verified
native integration. Existing device, keeper, browser and desktop behavior is
unchanged. Service implementation and acceptance require a separate review.

This optional extension lets a signed-in phone or tablet view one logical
workspace and start a structured agent on its existing execution host. It reuses
the daemon's HTTP and chat protocols behind an account-origin gateway. The mobile
device is a viewer/controller: it receives no daemon credential, host-selection
authority, ownership grant, local execution role or takeover operation.

## Negotiation and origin

Read authenticated `GET /v2/capabilities` passively. Require the exact integer
values `native_viewer:1` and `workspace_placement:2` before any proposed viewer
request. Missing, false, string-valued or unknown versions disable this surface;
there is no fallback to a browser cookie, host tunnel or unscoped daemon request.
These flags do not authorize a device to configure an executor or acquire an
ownership lease. The full native execution negotiation in
[VIEWING](VIEWING.md#passive-placement) remains separate.

HTTP requests use only the configured account origin, with the proposed prefix:

```text
/v2/workspaces/{workspace_id}/viewer/{daemon_path}
```

For chat, use the WSS equivalent of that same account origin. Follow the existing
[origin and authentication policy](PROTOCOL.md#origins-versions-and-authentication):
no credentials, path, query or fragment in the configured origin; HTTPS/WSS
except literal-loopback fixtures; no authenticated redirects or downgrade.
Never accept an upstream URL, keeper address or host ID from the mobile caller.
Logical workspace and session IDs remain opaque and retain their original values;
their path-segment grammar is 1–128 ASCII letters, digits, `_` or `-`. Do not
derive authority from names, prefixes or their apparent owner.

The account resolver chooses the current owner. Placement metadata is not a
caller-selected target or proof of network reachability. Discovery, capability
checks, device reads, placement refresh and background attachment are passive.
Do not use `/v1/me` as a free compute-status probe; its existing behavior is
documented in [PROTOCOL](PROTOCOL.md#account-routes).

## Authentication and exact scope

Every HTTP request and native WebSocket upgrade must carry exactly one
`Authorization: Bearer <full-device-access-token>` header. Daemon/delegation
credentials and browser cookies are not accepted as substitutes. The service
requires the header even when a cookie is present; cookies are neither used for
authorization nor forwarded. Tokens must never appear in queries, fragments,
first-frame JSON, errors, logs or ordinary persisted UI state. The gateway alone
injects the private upstream daemon credential.

The URL workspace, singleton `X-Chimaera-Workspace` header and singleton
`X-Chimaera-Epoch` header must agree with the authenticated live placement.
The epoch is a canonical unsigned decimal integer, compared exactly, never
rounded through a floating-point value. Partial, duplicated, malformed and
mismatching scope is refused before upstream effects. A query/body workspace or
session reference cannot widen it. Account/device revocation and account-generation
replacement invalidate admission even when workspace and epoch remain equal.

Reapply [VIEWING's forwarded-request rules](VIEWING.md#forwarded-requests):

- Resolve an `owned` or `suspended` placement with a current routable owner.
  `unowned`, `expired`, `privacy_disabled`, unknown or unroutable placement
  grants no route.
  Preferred-home and checkpoint metadata cannot override the current owner.
- Before forwarding, first make a bounded authenticated **passive** scoped health
  probe. Require HTTP 200 and exact singleton acknowledgments
  `X-Chimaera-Scope-Version: 1`, `X-Chimaera-Workspace: <workspace_id>` and
  `X-Chimaera-Epoch: <epoch>`. Failure never falls back to an unscoped request.
  Only the transport can vouch for a sleeping scoped owner as described there.
- The target rechecks its registered workspace and live execution authority on
  every request and socket write. A header or cached catalog row grants no
  authority. Retain the existing 20-second scoped renewal window on thaw and
  `workspace_scope_changed` / close 1013 outcome when the scope cannot be admitted.
- Positive scope acknowledgments may be reused only under the existing
  15-second same-host/project/route-stamp/epoch rule; sleeping answers are not
  cached. A transient failure cannot indefinitely extend a route. Preserve the
  at-most-150-second retention and definitive retirement rules in
  [passive placement](VIEWING.md#passive-placement), including the requirement
  for a surviving verified transport; mobile restart creates no authority.

For this proposed native surface, a deliberately authorized action has one
narrow exception to the passive-probe rule: when a sleeping cloud owner's scoped
cache cannot vouch for it (for example after transport restart), run the existing
home-first decision **before** any wake-bearing probe. Only an action already
authorized to wake the selected owner may mark its scoped health probe as
interaction. Bound that probe to 40 seconds and require the same exact HTTP 200
scope acknowledgment before forwarding the action. An accepted home target gets
its own fresh acknowledgment; it cannot inherit the cloud target's proof.
Background reads/attachment, failed authorization and unknown placement never
take this exception. An unresolved probe returns unavailable, never an unscoped
request or a speculative write. This is an explicit proposed exception, not a
claim that VIEWING's blanket passive-probe requirement enables native wake today.

Authorization is checked for each action, not just when a workspace screen opens.
Authentication failure, scope change, compute unavailability and an unsupported
route remain distinct sanitized outcomes. A missing new viewer route disables
the extension; it is never a request to wake, take over or change account state.
The ordinary account-401 refresh policy may retry a passive read after a successful
serialized refresh. It does not authorize repeating session creation or a socket
send: their uncertain-outcome rules below take precedence.

## Phase-one allowlist

The following are the only admitted method/daemon-path pairs. Paths are shown
after the `viewer/` prefix. Reject other methods, path aliases, traversal,
ambiguous separators, duplicate query keys and unknown query/body fields before
forwarding. Session IDs must belong to this exact workspace independently of
the HTTP headers.

| Method and daemon path | Permitted request | Existing result |
| --- | --- | --- |
| `GET api/v1/health` | No query/body | Scoped health and acknowledgment headers |
| `GET api/v1/workspaces` | No query/body | Existing roster, filtered to this workspace |
| `GET api/v1/agents` | No query/body | Existing agent/model availability catalog |
| `GET api/v1/sessions` | No query/body | Existing direct session array, filtered to this workspace |
| `GET api/v1/fs/list` | `path`, optional `hidden=true\|false` | Existing `{path,parent,entries,truncated}` listing |
| `GET api/v1/fs/file` | `path`, optional unsigned `offset` and `limit` | Raw byte slice and existing file metadata headers |
| `POST api/v1/sessions` | Closed structured-agent body below | Existing created session identity/result |
| `GET ws/chat/{session_id}` | Native WebSocket upgrade; no query/body | Existing structured chat stream after scoped first frame |

The proposed creation body is:

```json
{"workspace_id":"w-project","kind":"agent","ui":"chat","agent":"codex","model":"catalog-model-id","theme":"dark"}
```

`workspace_id`, `kind:"agent"` and `ui:"chat"` are required. Optional fields are
only `agent`, `model`, `name` and `theme`. Omitted agent/model/theme retain the
existing daemon defaults; explicit choices must come from the live catalog and
be valid for structured chat. `theme` is `light` or `dark`. The proposal adds
UTF-8 ceilings of 64 bytes for agent, 128 for model and 120 for name, with nonempty
control-free values, and a 16 KiB total creation-body ceiling. These are **new
proposed gateway limits**, not a claim that the existing daemon enforces them.
Reject `prelude`, `cwd`, `resume`, `title_hint`, PTY dimensions and all other
fields. In particular, missing `kind` or `ui` must never silently create a shell
or terminal agent. The gateway cannot invent an idempotency key the daemon lacks.

Initial file reads use the existing portable `/project` root and component-based
alias rules in [project files](VIEWING.md#project-files-and-stable-tabs).
The gateway fixes `X-Chimaera-Viewer-Root` to canonical unpadded base64url
`L3Byb2plY3Q`; this is presentation metadata, never authorization. Paths and
symlinks are checked against the actual registered root. The initial surface
does not admit arbitrary owner-machine paths or unrelated conversation resources.
Preserve known metadata translation; do not search or rewrite prompts, journal
text or file contents. Journals and existing creation replies can contain real
owner paths; displaying such content grants no file access.

`fs/file` is **not JSON text**. Preserve `X-File-Size`, `X-Truncated`, `X-Mtime`
and the optional stable whole-file `X-Content-Hash` according to the existing
daemon behavior. Its slices are bounded to 2 MiB; compressed-file semantics stay
unchanged. Native preview code treats bytes as untrusted content, supports
bounded/cancelable reads, and reports unsupported or oversized formats honestly.

No file save/upload/delete, session archive/rename/delete/resume/fork, terminal,
events bus, HTML workbench, application preview, installation binding, provider
login, host control, ownership transfer or executor configuration is admitted.
The wider existing browser and desktop surfaces are unaffected.

## Native chat authentication and receipts

The bearer header authenticates the native upgrade. Within the proposed
five-second first-frame deadline the client sends one bounded closed frame:

```json
{"type":"auth","workspace_id":"w-project","epoch":4,"last_seq":0}
```

The frame's scope must equal the upgrade's URL and headers. `last_seq` is an
unsigned integer represented exactly; a client cannot round or invent a cursor.
The frame contains no `token`. The gateway adds only its own upstream credential
and fixed viewer-root metadata to the existing daemon authentication frame.
The native caller cannot override either. Refuse duplicate JSON keys and unknown
first-frame fields; admit no command before the gateway validates that initial
scoped authentication. This admission is distinct from the target daemon's
readiness: an already-sleeping kept socket may have no `ready` until user input
wakes its owner. After initial authentication, a bounded holder may accept an
explicit send while the target is unattached/not ready, following VIEWING's
wake, scope and retention rules. It delivers only after the target admits the
scope and answers `ready`, and does not report the held send as delivered.

Phase one admits only that authentication and explicit user `send` commands,
using the existing command schema and a fresh valid `client_id` for each logical
send: 8–64 ASCII letters, digits, `_` or `-`, as in VIEWING. The initial closed
send subset contains exactly `type`, `client_id` and `blocks`, with one text
block and no other block fields:

```json
{"type":"send","client_id":"send_example_01","blocks":[{"type":"text","text":"Inspect this workspace and write a short result file."}]}
```

No inline image/file payloads, settings commands, approval/question answers,
interrupts or other client commands are enabled by this extension. Incoming
approval/question events may be rendered, but never answered implicitly. Connected
decision and settings controls require separate capability/acceptance work.

Reuse the daemon's `ready`, ordered `batch`/`ev` replay, sequence cursor and receipt
frames; do not translate session IDs or invent network success from a demo receipt.
Apply replay through `head` before concluding anything about pending sends;
discard already-applied sequences and handle a journal reset as the existing
chat protocol requires. A second `ready` is a reattach, not a new conversation.
Correlate a `user_message` echo and `send_confirmed` to their original `client_id`.
A queued echo alone is not proof of durable delivery. `send_uncertain` keeps the
text visible as uncertain; it does not prove nondelivery. Restore a refused send
only for an exact receipt that proves nondelivery. Never let a later refusal
overwrite a confirmed or uncertain result.

**Conservative mobile recovery:** `ready.send_ids:true` alone does not positively
distinguish older process-local deduplication from the current durable guarantees
described in [VIEWING](VIEWING.md#forwarded-requests). This initial native
extension therefore does **not** automatically resend or withdraw a send after a
lost acknowledgment, lost connection, process restart, route/epoch change or app
restart. Keep the original text and ID, replay/read authoritative state, and show
uncertainty if it cannot be reconciled. Enabling automatic replay needs separate
positive durable-receipt negotiation and acceptance for bounded ID retention,
dispatch crashes, process replacement and ownership transfer. This proposal
does not alter the existing desktop/browser retry behavior.

A lost `POST api/v1/sessions` reply is likewise uncertain: reconcile the scoped
roster without claiming a name/time match proves this creation, and never blindly
repeat the POST. Neither operation promises exactly-once external agent effects.

## Wake, lifecycle and bounds

Preserve [VIEWING](VIEWING.md#forwarded-requests) wake and home-first policy.
Only a deliberate user open may mark an allowed GET with
`X-Chimaera-Wake: interaction`; the native upgrade uses that same header for
deliberate wake intent rather than a caller-selected upstream query. Creation and
explicit sends are user actions. Passive probes, discovery, background refresh,
replay and reconnect carry no interaction marker. Opening a view or sending does
not itself transfer execution or change the preferred home. A sleeping cloud
owner may use the existing home-first behavior; absence of an eligible computer
does not authorize the phone to become an executor.

Retain the existing kept/unkept sleeping-socket, scope-change and revocation
semantics. The new gateway must bind kept input to the same account/device,
workspace, epoch and connection generation; it must discard held input when its
owning viewer ends. Existing holder/receipt rules do not become a mobile replay
permission. Closing tabs changes views only. Sign-out cancels requests and closes
sockets; app suspension must not start work or silently resubmit it on foreground.

Existing forwarded response-head budgets are 30 seconds, or 120 seconds when
waking an owner, and transfers terminate after 120 seconds without progress.
Scope probes and opening handshakes remain bounded and cancelable. The proposal
adds a 16 KiB auth-frame limit, 64 KiB UTF-8 send-text limit, 8 MiB complete chat
message limit, and per-direction queue ceilings of 16 frames **and** 16 MiB.
Enforce both frame and complete-message ceilings before decoding, apply
backpressure and bounded concurrency, and terminate oversized streams with an
honest error. These new ceilings are not claims about existing service acceptance.
Ordinary JSON responses retain the 1 MiB ceiling; raw file slices are the explicit
2 MiB exception. Never buffer an unbounded upstream response or retain a stream
after its owner is canceled.

## Acceptance before enablement

The implementation must prove all of the following before advertising
`native_viewer:1`; publishing documentation or passing a loopback fixture is
insufficient:

1. Native iOS and Android WebSocket clients actually send the single bearer and
   scope headers, reject redirects/downgrades, enforce bounds/cancellation, and
   reconnect without putting credentials in a URL or first frame. If either
   platform lacks this support, keep the surface disabled; do not weaken auth.
2. Full-device authentication, cross-account/device denial, workspace/session
   mismatch, duplicate scope, stale epoch, account-generation replacement and
   revocation fail closed for HTTP, upgrades, kept sockets and every later write.
   The gateway's authenticated principal/watcher lifetime must be independent of
   browser cookies; the existing browser gateway coupling is not native proof.
3. Exact target health acknowledgment refuses old/unscoped daemons. Owner move,
   thaw renewal, privacy disablement and removed routes fence old admissions;
   transient failure retention cannot become indefinite authority.
4. Passive reads/attachment and foreground replay do not wake compute. Explicit
   open, creation and send preserve current-owner/home-first rules, including a
   sleeping owner, unavailable preferred computer, scoped-cache loss after
   transport restart, and kept/unkept transport. Home-first selection precedes
   any deliberate probe wake; every selected target still supplies exact scope.
5. Closed session creation cannot spawn a shell/TUI or inject a prelude. Lost
   creation replies are not replayed. Send/echo/receipt replay handles lost ACK,
   restart, queued input, uncertain dispatch and epoch changes without duplicates
   or a false nondelivery claim. Decisions/settings stay unavailable.
6. File listing/slices retain exact scope, reject symlink/traversal escapes and
   preserve the browser path through tab changes. Raw bytes, malformed UTF-8,
   oversized messages, stalled transfers and cancellation remain bounded.

This is a public wire/acceptance proposal only. Service routing, principal
implementation, credentials and deployment configuration remain outside it.
