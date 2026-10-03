# Logical project viewing

Opening a synced project on another computer creates or updates a local copy
without acquiring ownership or changing its preferred home. The copy transfer
itself does not wake compute. An explicit user open may carry wake intent for
the current cloud owner; background polls, refreshes and passive attaches never do.
A passive owner view and ordinary input keep their route to the current owner;
only explicit Take over moves execution. Copy edits are not automatically
published. A browser gateway may still apply its separately negotiated sleeping
worker policy ([HANDOFF](HANDOFF.md#explicit-take-over-moves-execution)). Ordinary
SSH and non-Pro local windows keep their existing routes.

The separate [native workspace viewer proposal](NATIVE_VIEWER.md) describes a
disabled account-origin device-bearer surface that would reuse these scope and
placement rules. It neither enables routes/capabilities nor changes the existing
browser/desktop behavior or grants execution ownership to a phone.

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
account refuses anyone else's acquire with 409 `held`). Passive viewing never wakes
it. An explicit user open, a send or a permission answer can carry wake intent (`X-Chimaera-Wake:
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

This repository ships the daemon (send ids, `cancel_send`, the list of acting
commands), the UI and the native proxy. The keeper and the account's browser
gateway belong to the service; what follows is what they do, and the clients
here work with a keeper of either kind and with a daemon that predates send
ids.

A keeping keeper accepts the upgrade of `/ws/chat/{id}`, `/ws/sessions/{id}`
and `/ws/events` whether the machine is awake or asleep, with or without wake
intent, and never closes such a socket because the machine suspended or
resumed. On a suspension it closes only its own connection to the machine;
the viewer's side stays open and quiet (pings are answered). It closes the
viewer's side only when the viewer closes, the outer authorization ends, the
machine was replaced (a new boot or a new daemon credential), the daemon
closed for a reason that is not the suspension (an ownership change,
`workspace_scope_changed` with close 1013, an exit). A cap being reached
does not close it: the frame that does not fit is refused alone (below).

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
socket), a chat's settings commands (below; they have their own bound), and a
terminal's typing, which is its binary frames (up to 64 KiB per socket), at
most 12 MiB across the keeper. The first held frame asks the account to wake
a sleeping machine; the keeper then attaches with wake intent, replays the
authentication, waits for the daemon's `ready` and delivers the held frames
once, in order. Frames that arrive during the wake join the queue. Before it
delivers held acting input the keeper tells the account and waits for its
answer (at most 3 s), so a wake adds up to that to the delivery. An upgrade
that carries the wake marker (`?wake=interaction`) wakes the machine even
with nothing held.

A chat command is one of three kinds, and the keeper and the daemon agree on
which:

- **Acting commands** are exactly the ones the daemon counts as interaction
  (`chimaera-server` `activity::is_interaction`; the two lists change
  together): `send`, `send_after_turn`, `permission`, `answer`, `interrupt`,
  `compact`, `rewind` when it is not a dry run, `background_tool` and
  `stop_task`. Held, in order, and the machine is woken for them.
- **The seven settings commands** the user gives: `set_model`, `set_mode`,
  `set_effort`, `set_ultracode`, `set_remote_control`, `set_mcp_enabled` and
  `reconnect_mcp`. Held too, and the machine is woken for them, but they do
  not count toward the four-command cap: at most 16 per socket, 16 KiB each.
  They are coalesced as "the latest wins unless the user acted in between":
  consecutive picks of one setting collapse to the last, while `set_model X`,
  `send A`, `set_model Y` is delivered in exactly that order, so a model
  picked before a message still applies to it. `set_mcp_enabled` and
  `reconnect_mcp` coalesce per command and `server`.
- **Everything else** is not the user acting: `set_thinking` (a chat pushes
  it by itself), `get_usage`, `get_mcp`, `cancel_send`, `cancel_queued`,
  `steer_queued`, `send_now`, `send_if_running`, a dry-run `rewind`, and any
  command the keeper does not know. Never held, never a reason to wake, and
  counted toward no cap: with nothing attached the keeper drops it, attached
  it passes through.

The last kind also covers a terminal's text frames (folded, as above, and
passed on when attached) and everything an events socket sends (its `watch`
registration, dropped while nothing is attached; the client sends it again
after every attach, below).

When it starts holding input from a socket, the keeper sends that socket the
existing `{"type":"waking"}` frame, before anything else and once per wake.
The frame drives presentation only (a client shows its unconfirmed sends as
"sending…" from it); it does not decide what becomes of a send (next
paragraphs).

A frame that does not fit is refused alone, and what is already held stays
held: `command_failed` tagged with its `command` and, when the command
carried a well-formed one, its `client_id`; with `reason:"waking"` when that
socket already holds input and `reason:"reconnecting"` when the keeper-wide
total is used up. A setting that does not fit its bound is refused the same
way. The keeper copies the `client_id` of any chat command into the refusal
of that command, not only a send's.

When the machine cannot be attached within 150 s, the daemon refuses the
attach, or ownership moved while holding, every held chat command is handed
back with the daemon's own refusals (`command_failed` tagged with its
`command` and `client_id`, so a send returns to the composer) and held typing
with `read_only`, `reason:"reconnecting"`, followed by one
`remote_unavailable`. A hand-back's `remote_unavailable` never carries a
`reason`. A refused wake is handed back the same way with its reason. The
socket stays open and kept after a hand-back, with one exception: a project
that moved while frames were held gets the hand-back, then
`workspace_scope_changed` and close 1013, and that socket is not kept. Held
input lives in memory only and is never logged. Input is delivered only to
the project's owner at the current epoch.

The keeper pings its connection to the daemon every 10 s and detaches after
30 s of silence; the viewer's side stays open, and the next acting input is
held and wakes the machine. After a hand-back at the 150 s bound, and after
it drops a silent daemon connection, the keeper makes one passive attach by
itself when it believes the machine awake. A passive attach the daemon
refuses is answered with `remote_unavailable` (no `reason`), and the socket
stays open and kept.

When the viewer's side of a kept socket closes while frames are still held,
the keeper discards them: nothing is ever delivered for a socket that is
gone. A send the keeper passed on to a machine in the instant it froze is
gone too. Neither loses a message, because the client sends an unconfirmed
send again by its id (next).

**A send is delivered at most once, by its id.** Additive (2026-10-01); this
replaces every rule under which a client guessed what became of a send from
its text, its position or the time.

- `send` and `send_after_turn` take an optional `client_id`: 8 to 64
  characters of `A-Z a-z 0-9 _ -`, minted by the client per send. A daemon
  that predates it ignores the field; one that has it refuses a send whose
  id is not well formed with `invalid_command` and never echoes such an id.
- A daemon with send ids says `send_ids: true` in `ready`. It accepts a send
  under one id at most once, from the moment the command is queued (before
  the agent's driver handles it): a second send under an accepted id is
  dropped silently, and starts no second turn. The `user_message` the send
  produces carries the `client_id`, and so does every refusal of that command
  (`command_failed`, `invalid_command`, `read_only`).
- `cancel_send {client_id}` (not the user acting) withdraws an id. The
  daemon answers `send_cancelled {client_id, cancelled}`: `true` when no send
  under the id had been accepted, after which a send under it is refused
  with `command_failed`; `false` when one had been, which changes nothing.
- The daemon durably retains the newest 128 settled ids plus at most 64
  unresolved dispatches per conversation, separately from the lossy journal.
  It records dispatch before enqueue and withdrawal before acknowledging it.
  Unresolved dispatches are never evicted to make room: further sends are
  refused at the bound. Restart, process replacement and transfer retain this
  evidence; damaged or missing enrolled metadata fails closed. This is bounded
  input deduplication, not exactly-once agent actions or unlimited id retention.
- `send_confirmed {client_id}` reports a durable driver receipt when its
  journal echo may be unavailable. The client retains the text as delivered,
  stops resending and waits for replay to replace that row. An unresolved
  dispatch answers `error {code:"send_uncertain",client_id,command,message}`;
  this is nonfatal and is never a refusal claiming the message was unsent.
  The client keeps its text visible, stops automatic retries/withdrawal, and
  asks the user to check the conversation before sending again. A driver exit
  or terminal fallback likewise cannot prove nondelivery.
- `ready.active_queued_ids` optionally lists at most 64 client IDs still owned
  by the current driver's queue. After replay through `head`, a keyed queued
  echo not in that set is shown as unconfirmed delivery, with no Send now or
  cancellation claim. Apply this snapshot only to echoes at or before `head`;
  later live echoes belong to their own events. A later `sent`, `cancelled` or
  `dropped` update resolves the row normally. Older daemons omit the field.
- A client keeps every send it made until the echo that carries its id.
  A queued echo moves it to the pending queue; a nonqueued echo or explicit
  durable receipt confirms delivery. Only a refusal proving
  nondelivery and carrying its id returns its text to the composer. A delayed
  refusal cannot override a confirmed or uncertain receipt. At every `ready` that
  says `send_ids`, once the replay through `head` has been applied, it sends
  each send still without an echo or receipt again under the same id while the send is
  younger than two minutes. That is right whatever became of the first copy:
  lost, still queued in the daemon, or about to be delivered by a keeper that
  held it. An older one it withdraws with `cancel_send` and returns to the
  composer on `cancelled: true` (on `false` an active holder still owns it;
  confirmed and uncertain daemon receipts are reported explicitly). A client
  sends `cancel_send` only right after a `ready`: a keeper drops it while
  nothing is attached.
- Against a daemon without `send_ids` a client sends nothing twice and
  decides nothing at `ready`. That daemon's echo carries no id: it confirms
  the unconfirmed send with exactly its text, never another one. Its refusal
  names no send and returns the one just made.
- **Every holder treats ids as the daemon does.** A holder is anything that
  keeps a client's frames for an owner that has not answered: this
  computer's relay, a keeper, the account's gateway. The id of a send it
  holds counts as accepted. So a frame whose `client_id` it already holds is
  dropped silently, never refused and never held twice (the first copy will
  be delivered; a refusal would return a message that then runs). A
  `cancel_send` for an id it holds is never passed on: the holder answers
  `send_cancelled {client_id, cancelled:false}` itself (an owner that has not
  seen the send would call it withdrawn, and the held copy would still be
  delivered). And frames a client sends while held frames await delivery are
  delivered after them, in order, never ahead of them to whichever owner
  happens to be attached.
- A client withdraws a send at 120 s while a holder may hold one for up to
  150 s. That is safe because of the rule above: a withdrawal cannot win
  against a send that is held, only against one that is nowhere.
- A client sends a given send again at most once per pause, which starts at
  3 s and doubles to 30 s (a send can be megabytes, and a `ready` resets the
  reconnect backoff); a copy that is not due at a `ready` goes out when it
  is, unless its echo came first. A client's own resend or `cancel_send`
  never redials a socket or asks for a wake.
- The echo of a send is always journaled with its ids. A message too long
  for one journal line (the limit is the size of the largest text a send may
  carry, and characters that JSON escapes need several bytes each) is
  recorded with its text cut in the middle, never replaced.

The guarantee is bounded, and older daemons have narrower semantics:

- Current daemons persist up to 128 settled receipt/withdrawal IDs and retain
  every unresolved ID within the fixed 64-outstanding cap across restart,
  process replacement and transfer. Once a settled ID ages out of that bounded
  retention, it no longer fences a late retry.
- A crash after dispatch but before a durable receipt leaves an uncertain
  outcome. The daemon answers `send_uncertain`, never automatically repeats it
  or claims it was withdrawn. This does not promise exactly-once external
  effects. Queued driver input remains unresolved until `sent`; a queued echo
  alone does not prove survival across process replacement.
- Legacy daemons without durable receipts remember withdrawals only for the
  current process and can repeat input after losing its echo. Clients must not
  treat that older protocol as the current durable guarantee.

When the machine is awake again for any reason, the keeper attaches every
socket it kept, on its own, with the remembered authentication. An events
socket has no `ready`: once attached, frames pass, and the daemon's first
frames are its full snapshots.

What clients do, against either kind of keeper:

- A second `ready` on one socket is a reattach. A chat keeps its transcript
  and drops every event at or below the last `seq` it applied; pending sends
  stay pending until their echoes, and go out again by id as at any `ready`
  (above). A terminal treats it as a reconnect's
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
  bubble until the echo that carries its id; a refusal that carries its id
  returns exactly that send, above whatever is being written in the composer
  (`send_after_turn` is a send). A refusal that names an id the client no
  longer holds (a second copy of a send already confirmed or returned) is
  ignored. A refusal that names no send, from a holder that predates ids in
  front of a daemon that has them, returns nothing, not even when one send
  is unconfirmed (it may answer a second copy of a send that holder still
  delivers): the sends show as pending and the next `ready` sends or
  withdraws each. A prompt the UI sends from outside the composer goes out
  under its own id; refused, it is said and returns nothing.
- Nothing in the UI waits forever on a command that is not the user acting:
  the MCP panel keeps the inventory it has and closes after 10 s without a
  first answer, a rewind's dry-run check closes after 30 s. The thinking
  preference is pushed again when a new agent process starts (`init`) and
  after a reattach (a second `ready` on the same socket: its keeper dropped
  a push sent while nothing was attached), never on a plain reconnect: there
  the process still has it, and pushing again would let one window's default
  override another window's explicit choice at every blip. A toggle made
  while the conversation is not live is pushed at the next `ready`. The
  seven settings commands change nothing in the UI until the daemon confirms
  them (a keeper and the native proxy both hold them, so the old value shows
  until the owner answers; a held one the proxy gives up on is refused by
  name); the client itself never sends one twice.
- An events client sends its `watch` registration again whenever a `settings`
  frame arrives on a gateway socket: the daemon sends one per attach, and a
  registration lives on the daemon's side of one attach.
- The native proxy reads the header on the upgrade it makes, and nothing else
  decides. Kept: it passes every frame straight through in both directions
  from the first moment, holds nothing and tells the viewer nothing, whatever
  its scope probe said (a cached "awake" may be seconds stale); when such a
  socket ends (the keeper's side closes, or the project changes owner, which
  also says `moved`) it only closes the viewer's socket, and the viewer's
  client sends its unconfirmed sends again at the next `ready`. Not kept: the
  next paragraph applies unchanged. For a `worker-` owner its probe reports
  asleep, the proxy makes one passive attach (no wake marker) to find out; a
  refusal, or an unmarked accept that closes before `ready` or says nothing
  for 15 s, is remembered for that host's transport for five minutes, during
  which no further passive attach is made (viewer sockets and feed retries
  behave as before). An accept that closes ends the viewer's socket as any
  owner's close does (what the proxy held is refused first; the viewer
  reconnects onto the path below and its client sends again). One that only
  stays silent is dropped by the proxy after those 15 s, and input held for
  it then wakes the machine as below. A viewer
  already told `remote_unavailable` with no input held is closed when a kept
  attach succeeds, so its reconnect attaches quietly (a setting held alone
  does not keep it open: it is refused by name first). The proxy's events feed
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
input is held (≤64 KiB of typing, ≤4 chat commands, and ≤64 MiB across every
socket of the daemon), opens the owner's socket with `?wake=interaction`, and
is delivered once, in order, right after the owner's `ready`. Real input is a
terminal's typing and a chat's acting commands (the daemon's
`activity::is_interaction` list above, and nothing else). The seven settings
commands are held too, coalesced and bounded as a keeper's are: the latest
pick of a setting wins unless the user acted in between (`set_mcp_enabled`
and `reconnect_mcp` per `server`), at most 16 per socket of 16 KiB each,
outside the four-command cap. A held setting asks for no wake and brings no
work here, and it is delivered only in front of acting input from the same
viewer: it rides ahead of that viewer's next held or forwarded acting
command, so a permission mode picked before a message is in force when that
message runs. It is never delivered by itself. A `ready` with no input from
this viewer leaves it held, and after ten minutes it is refused with
`command_failed` naming its command, so a window left open cannot hand a
mode picked long ago to a turn another device starts. A setting picked while
the owner is attached is forwarded at once and replaces an older pick of it
still held. A setting that cannot be held is refused the same way, as is one
held when its input comes back (a failed wake, a route change, the other
computer keeping the work) or sent on a socket whose route just changed.
Every other chat command (the automatic
`set_thinking`, the reads, `cancel_send`, a command the daemon does not
know) passes to an attached owner and is dropped otherwise. When the first
input finds the owner asleep the viewer
gets the additive `{"type":"waking"}` status; while the wake is pending,
further input is refused rather than held (chat: `command_failed` with
`reason:"waking"`; typing: `read_only` with `reason:"waking"`, at most one
note a second). A fifth chat command while four are held is refused alone,
the four stay held. Input that
cannot be delivered is answered, never dropped: each chat command gets
`command_failed` (the UI puts a refused send's text and pictures back into the
composer); typing gets `read_only` with `reason:"reconnecting"`. Every chat
refusal carries the additive `command` it answers (`send`, `interrupt`,
`permission`…) and the additive `client_id` that command was sent under, and
a client restores a draft only for `command:"send"` or `"send_after_turn"`,
and then exactly the send the id names. The relay is a holder and follows the
holders' rule above (a second copy of a held send is dropped, a
`cancel_send` for it is answered `cancelled:false` here). It sends nothing
twice itself; a viewer socket that closes while this relay still holds input
loses that input here, and the client sends it again by its id at its next
`ready`.

When the route is a `device-` route, ordinary terminal/chat input is forwarded
to the current owner. Opening a local copy or typing never requests a move.
Explicit Take over uses the checked ownership endpoint; only after successful
acquisition and resume does new input route to the local executor. An owner
that is still working may refuse the explicit move with `still_working`.
A browser view gets the same
`bringing` frame with `to:"computer"` from the account's gateway when its
action on a sleeping cloud machine is sent to one of the user's computers
instead; the gateway holds the socket authentication and first input and
delivers them once to that computer's session, or to the woken cloud machine
(`waking`) when no computer took the work. Like a keeper it holds only acting
commands and the seven settings commands while the work is being brought,
never a view's other frames. Every `read_only` refusal (`reason`:
`watching`, `elsewhere`, `busy`, `waking`, `bringing`, `still_working`, `reconnecting`)
and HTTP `409 {"error":"workspace_owned_elsewhere"}` may carry the additive
`owner: "cloud" | "computer" | null`. Only a known typed route or explicit
placement establishes the label. Opaque holder IDs and this daemon's own role
do not identify another owner; `null` stays neutral in the client.

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
a computer. An opaque elsewhere owner uses
`{"type":"paused","reason":"elsewhere","owner":null}` rather than guessing a
`moved.to` destination. The socket then closes; the session's paused row carries the same
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

The browser portable view root is always `/project` (canonical unpadded base64url
`L3Byb2plY3Q` in `X-Chimaera-Viewer-Root` / socket `viewer_root`). It is presentation
metadata, never authority. Browser clients do not persist real filesystem roots,
and the metadata fields below are translated; content is never traversed, so
journals (a conversation's tool paths and saved-image paths), recents and
new-session replies still carry the owner's real paths.
Native windows translate their existing local file paths into this alias on the
local computer, then translate returned metadata back. Their window layout stays
local. This keeps existing file tabs stable while the project runs elsewhere.
For Git reads, a native forward instead supplies its actual local project root
as `X-Chimaera-Viewer-Root`; the owner translates known metadata before
serialization so large diff content needs no additional JSON buffer in transit.
The root remains presentation metadata and grants no path access.

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

Scoped Git reads include status, diffs, repository and worktree listings,
branches, history, commit details and branch comparison. The query workspace
must equal the granted workspace. Repository, file and rename-source paths are
validated against the registered root, including symlinks; a known linked
checkout outside that root is still forbidden. Listings omit external checkouts
and enclosing repositories. History for an enclosing repository is unavailable
through a project scoped to one of its subfolders, because it can reveal files
outside that project. Ordinary unscoped local Git inspection stays unchanged;
forwarded worktree creation and removal remain unavailable.

Git metadata maps `toplevel`, absolute `path`, `orig`, repository `parent` and
those fields in `entries`, `files`, `repos` and `worktrees`. Commit messages,
relative paths and both sides of a diff remain byte-for-byte content; they are
never searched or rewritten as path metadata.

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
replacement. Unscoped local and SSH sockets keep their ordinary routes;
agent PTY input also invalidates stale completion at writer admission and
respects the current local execution fence. Free plain shells keep ordinary
enqueue behavior.

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
