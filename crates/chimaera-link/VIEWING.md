# Logical project viewing

Opening a project on another device is a view onto its current owner. It never
acquires ownership, wakes a worker, releases the home computer, or repeats an
input that may already have been accepted. Ordinary SSH and non-Pro local windows
keep their existing routes.

## Passive placement

Full device clients read `GET /v2/capabilities` before enabling the negotiated
execution protocol. Require `execution_authority:2`, `installation_binding:1`,
`workspace_placement:2`, `checkpoint_receipts:1`, and one execution capability
both sides implement exactly (the service default when possible, see
[HANDOFF.md](HANDOFF.md#managed-execution-v2-negotiated-implementation)). Configure the daemon through the distinct
`/api/v1/pro/configure/execution` endpoint and verify its exact acknowledgment as
specified in [HANDOFF.md](HANDOFF.md). An old/unknown response is not a fallback
permission.

`GET /v2/workspaces/{workspace}/placement` returns:

```json
{"workspace_id":"w-project","holder_id":"d-home","route_host_id":"device-d-home","epoch":4,"policy_revision":1,"availability":"owned","preferred_installation_id":"i-home","checkpoint_id":"cp-saved","server_now":"2026-09-28T19:00:00Z","expires_at":"2026-09-28T19:01:30Z"}
```

Availability is `owned`, `suspended`, `unowned`, `expired`, or `privacy_disabled`
(clients read any newer value as unknown and never route it). `suspended` is a
cloud machine that went to sleep keeping ownership: its lease reads expired by
design, it keeps its `worker-` route, and it is routable like `owned` (the
account refuses anyone else's acquire with 409 `held`). Viewing it never wakes
it; a send or a permission answer carries wake intent (`X-Chimaera-Wake:
interaction` on HTTP, `wake=interaction` on a socket; any non-GET counts) and
the transport wakes it. A frozen owner cannot answer the native shell's full
project check, so for a `suspended` placement the shell accepts the transport's
scoped `/api/v1/health` answer marked `X-Chimaera-Worker-State: sleeping` (the
transport gives it only for a daemon token that acknowledged scoping while
awake). Only a live
owned or suspended placement has a route, and even a live one may have none: a holder whose
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
answer: the project is gone from this computer, private, unowned, expired (a
suspended owner is not expired),
owned here, or owned at a **newer epoch** than the registered route. A check
that merely fails (account or keeper unreachable, owner connection down or
asleep, the scope probe timing out) keeps the last verified route for at most
150 seconds, longer than one lease plus the takeover grace, so a real owner
change is always seen as a definitive answer first. A kept route also needs its
shared transport, so after a native restart a failed check retires it. A shared
transport stays open while any kept or healthy project still needs it. Failed
probes do not skip healthy siblings; failed retirements remain reported and
retry on the next bounded refresh.
Route currency is stamped **per project**: registering, re-registering or
retiring one project never closes a sibling's sockets or invalidates its preview
tickets on the same transport; only a changed endpoint or credential retires
every project on that host. A project that moves between registered hosts
re-homes its cached session rows at once.
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
rejected. An owner that just thawed from a suspension and is still renewing its
own lease for exactly the requested epoch waits for that renewal (never past its
20 s resume window) before answering a socket's first frame or a scoped request:
chat and terminal sockets get the additive `{"type":"waking"}` meanwhile, and
the socket is admitted (or the request served) only once the fresh proof exists.
A scope the target cannot admit (a refused or unanswered renewal, a changed
epoch, any epoch other than the one it is renewing) is answered at once with the
retryable `workspace_scope_changed` error, and a socket then gets a close frame
(1013, "try again later"); only a wrong bearer token answers `unauthorized`,
which clients treat as final. No authority comes from a header alone.

Before forwarding HTTP or opening a WebSocket, both gateway and native proxy make
a bounded authenticated **passive** scoped `GET /api/v1/health` to the target.
Require successful status and the exact response headers:

- `X-Chimaera-Scope-Version: 1`
- `X-Chimaera-Workspace: <requested logical workspace>`
- `X-Chimaera-Epoch: <requested live epoch>`

This prevents an older daemon that ignores additive fields from receiving a
write. A failed probe is not retried as an unscoped request. The target checks its
registered workspace and live execution grant again for each request and socket
write. The native proxy reuses a positive acknowledgment for 15 seconds for the
same host, project, route stamp and epoch; a "sleeping" answer is never cached.

**Sleeping owner.** A suspended owner cannot echo scope headers: the transport
answers `/api/v1/health` from cache with `X-Chimaera-Worker-State: sleeping`, or
503 `{"error":"worker_asleep"}`. Only the transport sets that header. When it
does, the native proxy relaxes the probe: mutations (and explicitly wake-marked
requests) are forwarded — the transport wakes the owner, which re-checks scope on
every write — with a 120 s response budget; passive reads are forwarded with no
wake marker and are answered from the transport's cache or with `worker_asleep`;
a socket attach stays passive and does not connect. (The transport half —
answering the probe from placement state with the scope headers — is a service
requirement.)

**Reconnects, held input and moves.** Viewing is passive; doing wakes. A native
window's chat/terminal socket to its own daemon stays open while the owner is
unreachable or asleep (`remote_unavailable` / `worker_asleep`, both non-fatal);
the daemon retries the owner on its own (2 s doubling to 30 s). The first real
input — terminal bytes or any chat command — is held (≤64 KiB of typing, ≤4 chat
commands, and ≤64 MiB across every socket of the daemon), opens the owner's
socket with `?wake=interaction`, and is delivered exactly once, in order, right
after the owner's `ready`. When that input finds the owner asleep the viewer
gets the additive `{"type":"waking"}` status; while the wake is pending,
further input is refused rather than held (chat: `command_failed` with
`reason:"waking"`; typing: `read_only` with `reason:"waking"`, at most one
note a second), so a repeated send never becomes a second turn. Input that
cannot be delivered is answered, never dropped: each chat command gets
`command_failed` (the UI puts a refused send's text and pictures back into the
composer); typing gets `read_only` with `reason:"reconnecting"`. Every chat
refusal carries the additive `command` it answers (`send`, `interrupt`,
`permission`…), and a client restores a draft only for `command:"send"`.
Nothing is resent automatically.

A session with no process where a viewer asks is not an exit, and its owning
daemon says why (additively; older clients ignore both and reconnect):
`{"type":"moved","to":"cloud"|"computer"}` only for a real transfer — while the
source exports it, or once this machine may no longer run its project (`to` is
where the session is going; a computer receiving its work back says
`"computer"`) — and otherwise
`{"type":"paused","reason":"restarting"|"needs_provider"|"importing"|"stays_on_computer","provider"?}`:
waiting out a daemon restart on the machine that owns the project, waiting for
its agent (`provider`) to be signed in on the cloud machine, being opened by
its transfer, or a plain terminal that moved with its project and only runs on
a computer. The socket then closes; the session's paused row carries the same
object as its additive `pause` field, and a client reconnects at once when the
row stops being paused. When the project's route changes under an established
socket, the viewer's daemon sends `moved` (to where the new route points) and
closes; a change of only the host's transport (a tunnel rebind or a new
credential for the same owner) closes quietly and the client reconnects to the
same owner. A scoped viewer whose connection changed hears
`workspace_scope_changed` first. Browser views have no local daemon to hold
input: a send or keystroke into a dropped socket reconnects once with wake
intent, and the action is not queued (a terminal says so over the pane).

**Events.** A window's own `/ws/events` loop stays authoritative. For a routed
project it runs a bounded feed from the owner that contributes only that
project's session rows (merged into the local roster), file invalidations (in the
window's paths) and Git/Timeline epochs; the owner's settings, recents, notices,
update and plugin frames are never forwarded. Only paths under the project's
folder are registered with the owner; the window's daemon keeps watching the
rest itself (a pasted upload, a note in the home folder), and an owner drops a
registered path its viewer may not read instead of closing. Local Git watching
is parked until the project is local again. An owner's Git and Timeline epochs
(in its events nudges and in its proxied `/git/status` and Timeline pages) are
reported as `((registration mod 2^20) + 1) << 32 | epoch`: above any daemon's own
counter and different per registration, so a local-to-routed switch (or back,
or between owners) always refetches. A feed ends as soon as its route changes
(a registration notifies it; a 2 s tick backs that up) and restarts with
backoff; the window's socket never closes for an owner change.
`remote_unavailable` and `workspace_scope_changed` on an events socket mean
"reconnect", not "rejected".

**Budgets.** Forwarded HTTP (requests, polls, probes) shares 32 permits; long-lived
sockets have their own 128. Response heads get 30 s (75 min for uploads and exec,
120 s when waking an owner); after the head, a transfer runs for as long as it
makes progress and is abandoned after 120 s of silence.

## Project files and stable tabs

The portable view root is always `/project` (canonical unpadded base64url
`L3Byb2plY3Q` in `X-Chimaera-Viewer-Root` / socket `viewer_root`). It is presentation
metadata, never authority. Browser clients do not persist real filesystem roots,
and the metadata fields below are translated; content is never traversed, so
journals (a conversation's tool paths and saved-image paths), recents, Git
worktree listings and new-session replies still carry the owner's real paths.
Native windows translate their existing local file paths into this alias on the
local computer, then translate returned metadata back. Their window layout stays
local. This keeps existing file tabs stable while the project runs elsewhere.

A native window's `/fs/*` request goes to the owner only when it belongs there:
any path under the project's local root (and requests with no path) go to the
owner; a read naming only paths outside the project asks the owner first (a
conversation that runs there links its own files). For such a path the owner
answers only whether it has something there: 404 `{"error":"not_found"}` when
nothing exists (this computer then answers with its own file; so it does when
the owner is unreachable, or is an older daemon that refuses every outside path
alike) and 403 `{"error":"outside_project"}` when a different file exists that
the viewer may not read (this computer then answers 403
`{"error":"on_other_machine"}` instead of its own same-named file). A write
outside the project stays on this computer when its folder exists here and
otherwise goes to the owner. The document checker (`/fs/check_document`) for a
viewer resolves root-relative links at the project's own folder (the viewer's
`root` query is dropped) and never stats, reads or lists a link target outside
the project; such targets are counted in one "outside this project" note. A file tab opened from an owner's real path is not rewritten
to the local root, so it stops resolving once the project is local again.

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
rosters; reads of the owner's `/health`, `/settings`, `/agents`, `/plugins`,
`/compute` and `/update`; current project timeline, plugin, agent-plugin, skill
and knowledge reads; opening the project, binding or removing its Mastermind and
delivering a Timeline note; session creation and normal session actions; links
between sessions in this project; project Git reads and recents; project
filesystem reads (including the document checker), edits, draft recovery, file
tickets/downloads and watches; and workspace-keyed browser layout state. Reads —
file reads, tickets and the compound resolvers, never writes — may also open the
images this project's own conversations saved in the owner's uploads folder.
`/fs/validate` and `/fs/resolve_targets` drop the candidates a viewer may not read
(they resolve as unknown) instead of refusing the whole batch, and never hand the
resolver a base outside the project.
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
