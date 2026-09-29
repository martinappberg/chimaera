# Logical project viewing

Opening a project on another device is a view onto its current owner. It never
acquires ownership, wakes a worker, releases the home computer, or repeats an
input that may already have been accepted. Ordinary SSH and non-Pro local windows
keep their existing routes.

## Passive placement

Full device clients read `GET /v2/capabilities` before enabling the negotiated
execution protocol. Require `execution_authority:2`, `installation_binding:1`,
`workspace_placement:2`, `checkpoint_receipts:1`, and an exactly supported
`execution_capability`. Configure the daemon through the distinct
`/api/v1/pro/configure/execution` endpoint and verify its exact acknowledgment as
specified in [HANDOFF.md](HANDOFF.md). An old/unknown response is not a fallback
permission.

`GET /v2/workspaces/{workspace}/placement` returns:

```json
{"workspace_id":"w-project","holder_id":"d-home","route_host_id":"device-d-home","epoch":4,"policy_revision":1,"availability":"owned","preferred_installation_id":"i-home","checkpoint_id":"cp-saved","server_now":"2026-09-28T19:00:00Z","expires_at":"2026-09-28T19:01:30Z"}
```

Availability is `owned`, `unowned`, `expired`, or `privacy_disabled`. Only a live
owned placement has a route, and even a live one may have none: a holder whose
device was revoked or whose cloud machine was removed is owned but unroutable
until its lease lapses. Keeper device route IDs are `device-` plus the exact
raw holder; workers use `worker-` plus that holder. Neither fuzzy prefix matching
nor preferred-home metadata overrides the current owner. The account proves
ownership, not network reachability. A checkpoint ID proves a reported saved
copy, not that a fresh sync completed on this viewing device.

A project the account has no ownership record for answers
`404 {"error":"workspace_not_found"}`; clients read that as `unowned` at epoch 0.
Any other 404 means the service lacks this route. `403` means the account has
no plan for cloud work.

The local authenticated `GET /api/v1/pro/placements` returns registered
`{host_id,workspace_id,epoch}` rows only. It never exposes transport URLs, tokens
or filesystem roots and is not available through a forwarded project scope.
Native reconciliation uses this daemon inventory even after its own restart.
A route is retired with `DELETE ...?workspace_id=…` only on a definitive
answer: the project is gone from this computer, private, unowned, expired,
owned here, or owned at a **newer epoch** than the registered route. A check
that merely fails (account or keeper unreachable, owner connection down or
asleep, the scope probe timing out) keeps the last verified route for at most
150 seconds, longer than one lease plus the takeover grace, so a real owner
change is always seen as a definitive answer first. A kept route also needs its
shared transport, so after a native restart a failed check retires it. A shared
transport stays open while any kept or healthy project still needs it. Failed
probes do not skip healthy siblings; failed retirements remain reported and
retry on the next bounded refresh.
The daemon also polls each project roster independently, with at most four requests
in flight and a ten-second deadline per project. One failure only marks that
project unavailable; healthy siblings keep updating. Changed results notify views
immediately, while repeated identical success or failure emits no refresh.
A hinted project without a route must have live local execution authority before
its local files or Git state can be substituted. Unavailable ownership returns an
error instead of silently viewing or saving a stale local copy.

## Forwarded requests

The account browser gateway serves `/workspace/{workspace}/`; its cookie-authenticated
`/placement` read uses the same resolver. The browser checks placement for each
HTTP action and socket authentication (coalescing only concurrent reads). It sends
`X-Chimaera-Workspace` and `X-Chimaera-Epoch`; WebSocket first-frame authentication
carries `workspace_id` and `epoch`. Authentication still uses the existing cookie
or target daemon bearer. Partial, duplicated, malformed or mismatching scopes are
rejected. No authority comes from a header alone.

Before forwarding HTTP or opening a WebSocket, both gateway and native proxy make
a bounded authenticated **passive** scoped `GET /api/v1/health` to the target.
Require successful status and the exact response headers:

- `X-Chimaera-Scope-Version: 1`
- `X-Chimaera-Workspace: <requested logical workspace>`
- `X-Chimaera-Epoch: <requested live epoch>`

This prevents an older daemon that ignores additive fields from receiving a
write. A failed probe is not retried as an unscoped request. The target checks its
registered workspace and live execution grant again for each request and socket
write. Owner changes close stale sockets; the UI reconnects and replays received
history only. It never queues or automatically resends a message or file mutation.

## Project files and stable tabs

The portable view root is always `/project` (canonical unpadded base64url
`L3Byb2plY3Q` in `X-Chimaera-Viewer-Root` / socket `viewer_root`). It is presentation
metadata, never authority. Browser clients do not persist or transmit real
filesystem roots. Native windows translate their existing local file paths into
this alias on the local computer, then translate returned metadata back. Their
window layout stays local. This keeps existing file tabs stable while the project
runs elsewhere.

Only known path fields are translated: filesystem `path`/`dir`, create/delete
`path`, rename/copy/move `from`/`to`, resolver `base`/`bases`/`candidates`/`targets`,
workspace `root`, session `cwd`/`cwd_current`/`workspace_root`, filesystem listing
and draft metadata, resolver result paths, and file-watch path arrays. JSON
adapters are capped at 1 MiB; raw file contents, uploads, downloads, notebook cells,
table data, prompts and journals stream through unchanged. Path aliases match
whole components, so a sibling prefix does not become a project path.

The target resolves filesystem paths against its **actual registered root**,
including existing symlinks and the nearest existing parent for a new file.
Resource IDs are checked independently: sessions use the session-workspace
registry, native resume handles use that workspace's recents/transcript catalog,
and raw/download tickets use their registered filesystem resource. A ticket for
another project remains forbidden even with valid scope headers. Native forwarding
uses a bounded expiring ticket map; stable upstream tickets preserve the same
local preview URL, and changed route generations invalidate old tickets.

Forwarded filesystem and draft mutations retain the admitted account generation
and workspace epoch. Receiving or staging an upload body grants no execution
authority. Final commits recheck the exact live scope and hold a bounded commit
reservation; clean handoff and account/configuration replacement drain already
admitted commits, including filesystem work whose caller disconnected. No shared
state mutex is held during filesystem I/O. Reservation capacity is bounded to 64;
a stop/drain timeout fails closed rather than permitting a new owner early.

Forwarded terminal, chat and event sockets retain the account generation from
first-frame authentication. An equal workspace and epoch under a replacement
configuration does not revive an old connection. Terminal input rechecks that
admission in the actual PTY writer and holds it through write/flush; resizes hold
it inside the blocking operation. Structured commands retain it through a bounded,
owned enqueue, and clean transitions fence the old driver before activating a
replacement. Unscoped local and SSH sockets preserve their existing behavior.

The initial forwarded surface is an explicit allowlist: scoped workspace/session
rosters; current project timeline, plugin, skill and knowledge reads; session
creation and normal session actions; links between sessions in this project;
project Git reads and recents; project filesystem reads, edits, draft recovery,
file tickets/downloads and watches; and workspace-keyed browser layout state.
Account configuration, environment changes, arbitrary workspace creation, Git
worktree creation and browser-pane proxy tickets are not admitted through this
project route. Opaque browser proxy entries currently lack a trustworthy workspace
binding, so accepting their IDs would cross the project boundary. Local and SSH
surfaces retain their existing functionality.

These request checks are not OS isolation: another process running as the same
user can race filesystem changes. Per-project runtime isolation and delegated
credentials remain separate requirements. Likewise a viewer route does not prove
that a suspended process cannot perform an external action. Execution/uncertain
checkpoint recovery follows [HANDOFF.md](HANDOFF.md); the viewing client only
follows the newly acknowledged canonical owner.

## Stable installation binding

Native clients keep a random `i-…` installation ID and canonical 32-byte secret
proof in the OS Keychain, scoped by endpoint, account and canonical configuration
directory. Neither daemon nor webview receives the proof. Save must succeed before
`POST /v2/installations/bind`; a damaged/locked entry is not replaced by a new
identity. Sign-out retains it; deliberate account deletion/revocation owns cleanup.

A bind returning `409 clean_release_required` does not permit revoking the old
owner. Native first asks the old local daemon for its ordinary clean `/pro/sleep`
release. If old account authority is unusable, native requests the narrowly bound
recovery grant and calls `/pro/execution/recover` as specified in HANDOFF. Only its
exact released acknowledgment permits rebinding. Timeout, mismatch or uncertain
publication is failure. Recovery grants stay in memory, never in layout/settings.
