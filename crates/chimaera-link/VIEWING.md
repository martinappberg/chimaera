# Logical project viewing

Opening a project on another device is a view onto its current owner. It never
acquires ownership, wakes a worker, releases the home computer, or repeats an
input that may already have been accepted. Acting on it can move the work:
input on another of the user's computers brings the project there, and a
phone's input while the cloud sleeps may bring it to an online computer
([HANDOFF](HANDOFF.md#acting-brings-the-work-to-you)). Ordinary SSH and non-Pro
local windows keep their existing routes.

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
a socket attach stays passive: for a cloud machine the proxy makes one attach
without a wake marker (next paragraph), and when the transport refuses that
attach, or refused one in the last five minutes, it does not connect. (The transport half — answering the probe from
placement state with the scope headers — is a service requirement.)

**A sleeping cloud machine's sockets.** Additive (2026-09-30); a contract for
a keeper that keeps a cloud machine's sockets, which a keeper need not be.
Such a keeper is the machine's front door, so that no device has to be told
the machine is asleep. It marks **every** WebSocket upgrade it accepts for a
`worker-` host with the response header `X-Chimaera-Sockets: kept`, whether
the machine is awake or asleep at that moment; a keeper that does not send it
is the older kind (it answers a passive upgrade to a sleeping machine 503
`worker_asleep` and closes sockets when the machine suspends), and clients
must work with both. The header alone tells the two apart: what a probe or a
placement read said a moment ago does not.

A keeping keeper accepts the upgrade of `/ws/chat/{id}`, `/ws/sessions/{id}`
and `/ws/events` whether the machine is awake or asleep, with or without wake
intent, and never closes such a socket because the machine suspended or
resumed. On a suspension it closes only its own connection to the machine;
the viewer's side stays open and quiet (pings are answered). It closes the
viewer's side only when the viewer closes, the outer authorization ends, the
machine was replaced (a new boot or a new daemon credential), the daemon
closed for a reason that is not the suspension (an ownership change,
`workspace_scope_changed` with close 1013, an exit), or a cap below is
exceeded.

The viewer's first frame is its authentication. The keeper remembers it, with
the request it forwarded, and attaches with it again later. A chat's
`last_seq` is raised to the highest sequence number relayed on this socket
(`seq` of an `ev` frame and of every entry of a `batch`), so the daemon
replays exactly the gap. A terminal's later `resize`, `park` and `unpark`
text frames are folded into the remembered frame (its `cols`/`rows` and
`parked`), so every attach uses the grid and park state the viewer has now.

What the user does is held whenever the daemon has not answered `ready` on
the current attach: while nothing is attached, and also after an attach until
its `ready`. Held are a chat's acting commands (up to 4 frames and 12 MiB per
socket) and a terminal's typing, which is its binary frames (up to 64 KiB per
socket), at most 12 MiB across the keeper. The first held frame asks the
account to wake a sleeping machine; the keeper then attaches with wake
intent, replays the authentication, waits for the daemon's `ready` and
delivers the held frames once, in order. Frames that arrive during the wake
join the queue.

A chat's acting commands are exactly the ones the daemon counts as
interaction (`chimaera-server` `activity::is_interaction`; the two lists
change together): `send`, `send_after_turn`, `permission`, `answer`,
`interrupt`, `compact`, `rewind` when it is not a dry run, `background_tool`
and `stop_task`. Every other chat command is not the user acting (`set_mode`,
`set_model`, `set_effort`, `set_thinking`, `set_ultracode`, `get_usage`,
`get_mcp`, `set_mcp_enabled`, `reconnect_mcp`, `set_remote_control`,
`cancel_queued`, `steer_queued`, `send_now`, `send_if_running`, a dry-run
`rewind`, and any command a keeper does not know): it is never held, never
wakes the machine and counts toward no cap; with nothing attached the keeper
drops it, attached it passes through. The same holds for a terminal's text
frames (folded, as above, and passed on when attached) and for everything an
events socket sends (its `watch` registration, dropped while nothing is
attached; the client sends it again after every attach, below).

When it starts holding input from a socket, the keeper sends that socket the
existing `{"type":"waking"}` frame, before anything else and once per holding
period. For a socket that has already heard `ready` this is required, not
optional: it is the only thing that tells the client its send is waiting in
the keeper's queue for the next `ready` rather than lost (next paragraph),
and a client shows the send as "sending…" from that frame.

When the machine cannot be attached within 150 s, a cap is exceeded, the
daemon refuses the attach, or ownership moved while holding, every held chat
command is handed back with the refusals of the next paragraph
(`command_failed` tagged with its `command`, so a send returns to the
composer) and held typing with `read_only`, `reason:"reconnecting"`, followed
by one `remote_unavailable` without a `reason`. A refused wake is handed back
the same way with its reason. The socket stays open and kept after a
hand-back. Held input lives in memory only and is never logged. Input is
delivered only to the project's owner at the current epoch.

**Delivered, handed back, or discarded.** When the viewer's side of a kept
socket closes while frames are still held, the keeper discards them: nothing
is ever delivered for a socket that is gone. And a send the keeper passed on
to a machine in the instant it froze is simply gone. So the client decides
every send itself: its echo (`user_message`) confirms it, a refusal returns
it, and otherwise the next `ready` it predates settles it: once that attach's
replay has arrived (through its `head`), a send still without an echo never
reached the agent, and the client puts its text back into the composer as it
does for a refusal. This holds for every `ready`, a reattach on the same open
socket included. One kind of send is not settled by a `ready`: a send that is
waiting in a queue for exactly that `ready`, which delivers it afterwards.
The client takes a send for queued when it was made on a socket that had not
heard `ready` yet, after a `waking` or `bringing` that no `ready` has
followed, or within 15 s before such a frame arrived (the frame answers the
send that started the holding); the following `ready` settles it if its echo
still has not come. A socket that ends, or `remote_unavailable`, means
nothing is queued any more.

When the machine is awake again for any reason, the keeper attaches every
socket it kept, on its own, with the remembered authentication. An events
socket has no `ready`: once attached, frames pass, and the daemon's first
frames are its full snapshots.

What clients do, against either kind of keeper:

- A second `ready` on one socket is a reattach. A chat keeps its transcript
  and drops every event at or below the last `seq` it applied; pending sends
  stay pending until their echoes. A terminal treats it as a reconnect's
  `ready`, answering its auth frame with the `park`/`unpark` it sent since
  folded in. After a shown attach it resets and takes the snapshot that
  follows, then sends its grid when `ready` names another one; hidden with
  its `park` not yet sent, it sends only `park` (a hidden terminal never
  resizes the PTY). After a parked attach no snapshot follows: a hidden
  terminal discards its buffer and repaints when shown, and a shown one
  whose `unpark` never went out resets, sends its grid and `unpark` (which
  repaints).
- A browser cannot read the upgrade's header, so the UI infers a kept socket:
  open, authenticated and without a frame for 1.5 s is kept (no
  "Reconnecting…", no retry timer, no reconnecting indicator). A send, a
  decision or a keystroke on an open socket is simply sent. A kept socket
  that later drops is dialed again before anything is concluded from the
  placement. `worker_asleep` at any time means the socket is not kept.
- `remote_unavailable` ends a wake that was under way and means "not live
  until the next `ready`". With `reason:"reconnecting"` it is a relay that
  cannot reach the owner and keeps trying: the view says "Reconnecting…"
  until the next frame. Without it (a hand-back) the socket is still kept,
  and quiet for 1.5 s again returns to the kept presentation.
- Several sends can be pending at once, each shown as its own "sending…"
  bubble. An echo confirms the send whose text it carries (and returns an
  older one that was already waiting at the last `ready`: sends arrive in
  order, so it was skipped); a refusal answers the oldest (the newest for
  `reason:"waking"`/`"bringing"`, which refuse the send that just arrived);
  `send_after_turn` is a send.
- Nothing in the UI waits forever on a command that is not the user acting:
  the thinking preference is pushed again after every `ready`, the MCP panel
  keeps the inventory it has and closes after 10 s without a first answer,
  a rewind's dry-run check closes after 30 s. The other settings commands
  (`set_model`, `set_mode`, `set_effort`, `set_ultracode`,
  `set_remote_control`, `set_mcp_enabled`, `reconnect_mcp`) change nothing in
  the UI until the daemon confirms them, so a dropped one leaves the old
  value showing; they are not sent again.
- An events client sends its `watch` registration again whenever a `settings`
  frame arrives on a gateway socket: the daemon sends one per attach, and a
  registration lives on the daemon's side of one attach.
- The native proxy reads the header on the upgrade it makes, and nothing else
  decides. Kept: it passes every frame straight through in both directions
  from the first moment, holds nothing and tells the viewer nothing, whatever
  its scope probe said (a cached "awake" may be seconds stale); when such a
  socket ends (the keeper's side closes, or the project changes owner, which
  also says `moved`) it only closes the viewer's socket, and the viewer's
  client decides its pending sends at the next `ready`. Not kept: the next
  paragraph applies unchanged. For a `worker-` owner its probe reports
  asleep, the proxy makes one passive attach (no wake marker) to find out; a
  refusal, or an unmarked accept that closes before `ready`, is remembered
  for that host's transport for five minutes, during which no further passive
  attach is made (viewer sockets and feed retries behave as before). A viewer
  already told `remote_unavailable` with nothing held is closed when a kept
  attach succeeds, so its reconnect attaches quietly. The proxy's events feed
  attaches the same way, sends no registration before the owner's first frame
  on a kept or sleeping attach, and registers again on every `settings` frame
  of a `worker-` owner (a computer's feed is as before).

**Reconnects, held input and moves.** Viewing is passive; doing wakes. This is
what the native proxy does for an owner that is another computer, for one
that cannot be reached, and for a cloud machine whose keeper does not mark its
sockets kept (the paragraphs above replace it otherwise). A native
window's chat/terminal socket to its own daemon stays open while the owner is
unreachable or asleep (`remote_unavailable` with the additive
`reason:"reconnecting"`, the relay's own lasting state / `worker_asleep`, both
non-fatal); the daemon retries the owner on its own (2 s doubling to 30 s). The first real
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
`permission`…), and a client restores a draft only for `command:"send"` or
`"send_after_turn"`. Nothing is resent automatically. A viewer socket that
closes while this relay still holds input loses that input here; the client
then finds no echo after its next `ready` and restores the text itself.

When the route is a `device-` route (another of the user's computers owns the
project) and this computer can take it, the first real input instead brings
the work here: the relay holds it (the same budget), says
`{"type":"bringing","to":"here"}`, stops forwarding input to the owner and
hides the owner's `moved`/`paused` frames and its closing socket for this move.
Once this computer holds the project and its session resumed, the held input
is delivered once to the local session and the socket closes quietly (the
viewer reconnects to the session here and its replay carries the message).
When the other computer keeps the work, each held chat command is refused
with `command_failed`, `reason:"still_working"` and the plain line "Your other
computer is still working on this. Try again when it pauses." (typing: one
`read_only` with the same reason); input beyond what is held while the work
is coming is refused with `reason:"bringing"`. A browser view gets the same
`bringing` frame with `to:"computer"` from the account's gateway when its
action on a sleeping cloud machine is sent to one of the user's computers
instead; the gateway holds the socket authentication and first input and
delivers them once to that computer's session, or to the woken cloud machine
(`waking`) when no computer took the work. Every `read_only` refusal (`reason`:
`watching`, `elsewhere`, `busy`, `waking`, `bringing`, `still_working`, `reconnecting`) and every HTTP
`409 {"error":"workspace_owned_elsewhere"}` also carry the additive
`owner: "cloud" | "computer"`: where the project's work runs now, so a client
can say "running in the cloud" / "running on your computer" without guessing.
An owning daemon names its recorded owner (the other machine while another
owner holds the project — a cloud machine's other owner is the user's
computer, a computer's is the cloud — else itself); a viewing daemon's relay
names its route's owner (a `worker-` route is the cloud).

A session with no process where a viewer asks is not an exit, and its owning
daemon says why (additively; older clients ignore both and reconnect):
`{"type":"moved","to":"cloud"|"computer"}` (with the additive `other:true` when
the work went to another of the user's computers, not the cloud) only for a real transfer — while the
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
input: an open socket takes it (a keeper that keeps a cloud machine's sockets
holds it), and a send or keystroke into a dropped socket reconnects once with
wake intent, and that action is not queued (a terminal says so over the pane).

**Events.** A window's own `/ws/events` loop stays authoritative. For a routed
project it runs a bounded feed from the owner that contributes only that
project's session rows (merged into the local roster), file invalidations (in the
window's paths) and Git/Timeline epochs; the owner's settings, recents, update
and plugin frames are never forwarded. Nor are its notices, as frames: the
owner scopes them to the project, and the viewing daemon relays a finished turn
(`done`/`input`), a permission, a question and the agent's own `notify` message
into its own notice feed (so the
native app and browser tabs alert as for local work) — once per notice across
every window's feed (keyed by session, kind and the owner's `at_ms`, a bounded
set), never for a session that runs on the viewing computer, and only for kinds
its own `notifications.*` settings allow. A routed conversation blocked on a
decision joins the viewing daemon's attention set (the Dock badge), so its
alert is taken back once answered on either machine. Only paths under the project's
folder are registered with the owner; the window's daemon keeps watching the
rest itself (a pasted upload, a note in the home folder), and an owner drops a
registered path its viewer may not read instead of closing. Local Git watching
is parked until the project is local again. An owner's Git and Timeline epochs
(in its events nudges and in its proxied `/git/status` and Timeline pages) are
reported as `((registration mod 2^20) + 1) << 32 | epoch`: above any daemon's own
counter and different per registration, so a local-to-routed switch (or back,
or between owners) always refetches. A feed ends as soon as its route changes
(a registration notifies it; a 2 s tick backs that up) and restarts with
backoff; the window's socket never closes for an owner change. A sleeping
cloud machine's feed stays open and quiet when its keeper keeps it, and ends
(to be retried, 1 s doubling to 30 s, without another attach while the
refusal is remembered) when the keeper refuses it; a sleeping computer's is
not opened.
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
