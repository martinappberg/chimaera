# Session bundle protocol — version 1

A bundle transfers one Chimaera session between authenticated daemons. The
public session ID, workspace ID, absolute workspace root and cwd, native
conversation handle, journal sequence numbers, model preferences, pinned title,
linked terminal edges, workspace fallback layout, keep-running preference,
and original creation time survive transfer. It contains no process snapshot.

## HTTP interface

These routes require the normal daemon bearer token. A keeper's authenticated
HTTP adapter may forward them using the target daemon's credential.

- GET `/api/v1/pro/bundles/{session}`: snapshot ZIP; never stops the session.
- POST `/api/v1/pro/bundles/{session}/export` with `{stop:true}`: durably suspend
  and stop an agent, then return its ZIP. `{stop:false}` is a snapshot.
- POST `/api/v1/pro/bundles?fork=false&origin=moved&epoch=7` with an
  `application/zip` body: verify and import. `origin` is `moved` or `home`;
  `fork` requests a native head fork for an offline takeover. Returns
  `{id,workspace_id,paused}`. Refusals are 409 with `{error}`.
  `defer_start=true` installs a suspended entry without spawning; the ownership
  coordinator stages every archive at the same verified epoch, then grants local
  ownership and resumes the complete workspace. A partial import remains fenced
  across restart. Moved plain shells remain paused even after that grant.

Only two archive operations run concurrently. Uploads and archives are bounded
at 100,000,000 bytes, native transcripts at 90,000,000 bytes, the journal at
4 MiB, and metadata at 256 KiB. Oversized data refuses transfer; it is never
silently truncated. Snapshot journals include complete records only. A native
file that changes during a checked snapshot causes a retryable refusal.

The internal export API returns a temporary file owned by its caller, which
must unlink it after consumption. HTTP responses unlink the backing file as soon
as the streaming reader owns it. Neither route buffers the whole ZIP in RAM.

Automatic workspace mirrors have a narrower empty-chat exception than these
explicit export routes. If native history is missing, the mirror can omit a live
unstarted structured Claude chat only with a complete startup-only journal (from seq 1,
at most 256 events / 256 KiB), no resumed/forked context, no submitted input, and
no active or background work. Accepted input is remembered before its journal
echo, so a just-submitted turn cannot qualify. Snapshots leave the source live.
Clean handoff atomically pauses command ingress before proving emptiness, then
durably suspends its ledger entry and stops its process; local ownership return
restores the same session with a fresh empty CLI. Failed or canceled proofs
restore ingress, and late sends receive an explicit paused error. Codex native
history remains strict because its restore path requires an existing thread.
Missing, truncated, corrupt or meaningful history is still a transfer error.

## Archive format

ZIP members use stored compression, regular-file permissions 0600, and exact
names. Directories, symlinks, duplicate names, unexpected members, compression,
length mismatch, and SHA-256 mismatch are rejected before installation.

`manifest.json` contains:

```json
{
  "version": 1,
  "workspace": {"id":"w-example","root":"/home/user/project","name":"project"},
  "session": {"id":"s-example","workspace_id":"w-example","cwd":"/home/user/project"},
  "source_host": "laptop",
  "source_os": "macos",
  "stopped": true,
  "links": {},
  "members": {"native.jsonl":{"bytes":1234,"sha256":"hex digest"}}
}
```

`workspace` is the additive daemon Workspace record; `session` is the additive
session-ledger record including agent kind, surface, native ID, model, and
carryover. Optional members are `journal.jsonl` (unaltered complete SeqEvent
records), `index.json` (model/effort/mode only), `view.json` (workspace fallback
layout), and `native.jsonl` (required for agents). No member controls an extraction path. The daemon derives the native
store path from the validated agent kind, UUID, and cwd. Codex's first
`session_meta` record must match both native ID and cwd.

Native transcripts and journals retain the user's conversation verbatim,
including content the user pasted. Credentials/configuration files, environment
variables, launch commands, and hook files are excluded. Project files are
transferred separately by the workspace mirror. Never mirror disables automatic
workspace/session transfer; the archive does not attempt to rewrite secrets
inside a native conversation.

## Ownership and recovery

The target must already have the original canonical root/cwd unless the explicit
`destination_root=/canonical/existing/path` import option is set. A remap keeps
the cwd relative to the original workspace root and rejects missing directories
or symlink escapes. It preserves the workspace ID. Native transcripts stay
byte-for-byte intact: the ledger carries original header cwd provenance while
the CLI resumes from the destination cwd. A new native fork uses its own header
provenance. Existing workspace IDs must still resolve to the same destination
root; an import never silently retargets another registered project. Import rejects
workspace ID/root collisions, an active session with that ID, a known remote
owner, and an ownership epoch mismatch. An exact successful archive+epoch retry
is idempotent. Imported state is retained as a suspended ledger entry before
spawn, so failure preserves recoverable history. A known Pro workspace remains
suspended after daemon restart until its ownership has been verified.

Clean transfer resumes the same native conversation. Offline takeover requests
the CLI's native head fork: Claude `--fork-session`, Codex `thread/fork` without
`lastTurnId`, or Codex TUI `fork <id>`. Native transcript IDs are never rewritten.
Structured chats receive one attributed `moved`/`home` context message. Native
TUIs receive the same bounded context as one positional prompt on resume/fork. The
Mastermind remains reactive and receives no automatic turn.

Plain terminals remain on the source laptop. Their moved bundle imports as a
paused row, preserving tabs without restarting arbitrary foreground programs.
Returning a shell home recreates its shell at the recorded cwd.

## Placement forwarding

The local app registers a verified remote workspace with POST
`/api/v1/pro/placements`:
`{host_id,endpoint,token,workspace_id,epoch}`. Endpoint is literal
`http://127.0.0.1:<port>`; tokens/listeners remain memory-only. DELETE the same
path with `?host_id=...` removes connectivity while preserving cached unavailable
rows. Older ownership epochs cannot replace newer routes.

Session rows add `placement:"here"` or `placement:{remote:host_id}` and
`placement_available`. The local daemon merges remote session rows under their
stable IDs and forwards session REST, terminal WS and chat WS through the app's
loopback link. First-frame daemon authentication is replaced at the forwarding
boundary; local bearer tokens are never sent to the other host. Forwarding is
bounded to 32 operations, 32 hosts and 512 remote rows. Connection changes close
old sockets. Passive directory reads carry no wake intent.

`/ws/sessions/{id}?read_only=true` and `/ws/chat/{id}?read_only=true` reject input,
commands and terminal resizes, including auth-time dimensions. Normal writer
sockets also re-check known workspace ownership before every mutation. An
explicit `wake=interaction` query passes deliberate worker intent; read-only
always suppresses it. Remote-unavailable errors never fall back to local spawn.

Workspace fallback view state is bounded at 64 KiB and only installed when the
destination has no existing fallback layout. A bundle carries at most 128 linked
terminal edges, all touching its own session; known cross-workspace endpoints
are rejected. Suspended entries retain their edges in the durable ledger.
