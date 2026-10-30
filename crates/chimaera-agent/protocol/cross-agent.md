# Cross-agent contract

The rules every driver shares, and the normalized events whose meaning is the same for every agent. Provider details are in [claude-code.md](claude-code.md), [codex.md](codex.md) and [acp.md](acp.md). Each fact names the pass or date where it was recorded. The dated record is [passes.md](passes.md).

## Cross-agent invariants

- One normalized event model (`model.rs`, ACP-shaped); drivers translate.
- Caps apply at event construction, not at sinks (login-node budgets).
- A handshake watchdog is mandatory per driver. A failed launch never switches to a PTY automatically (2026-10-01; see [Startup lifecycle](#startup-lifecycle)).
- An agent→client request that no handler knows is never shown as a permission card, and the user hears about it once per kind: a Notice and a daemon `warn`. The Claude and Codex drivers share `UNHANDLED_REQUESTS_CAP` (64 names remembered) and `UNHANDLED_REQUEST_NAME_MAX` (80 chars in a notice or reply), in `model.rs`. Claude's control requests are parked, because the CLI's deadline or another attached client settles them and an error reply could break flows that rely on that. Codex's JSON-RPC requests get a `-32601` error, because nothing else would answer them. ACP refuses an unsupported client method with `-32601` too (Pass 34; 2026-10-14).
- Model switches emit only `ModelSwitched`, never a second process `Init`. `Init` is a complete driver-process snapshot that expires stale asks; the UI reducer treats every real `Init` as authoritative, and absent optional fields clear the prior process snapshot (Pass 26).
- Every daemon↔UI addition is additive and pinned in `tests/wire_contract.rs`; defaults serialize to nothing, so old journals replay byte-identically (Pass 8 and later).

## Version detection

No wire carries a version handshake chimaera can depend on.

- claude: through 2.1.289 the `initialize` response carried no version field. 2.1.300 adds `claude_code_version`, which the driver journals only when the launcher's probe failed (Pass 43).
- codex: the `initialize` result carries `userAgent` (+ `codexHome`, `platformFamily`, `platformOs`), the server's own phrasing, not a stable contract. The handshake discards it (original probe; Pass 30).
- The version comes from outside the wire. The server probes `bin --version` (`launcher::probe_version`, 2 s budget), stores the first line on `AgentDetection.version` (kept fresh across in-place updates by the cache-staleness stamp, `validate_cache_hit`), and threads it through `ChatRecipe.version` → `SpawnSpec.agent_version` into the driver harness.
- The harness (`run_driver`), once past the handshake, journals it on `Init` (`AgentEvent::Init.agent_version`, additive) so a drifted binary is diagnosable from the journal alone.
- The harness warns, never blocks, on drift: if no whole version token of the probed line equals the driver's `tested_version()` (`version_matches_pin`; a leading `v` is ignored), it emits a non-fatal notice naming both versions. Whole tokens, because a substring test would pass codex 0.157.10 against a 0.157.1 pin (Pass 33). The probe line is the CLI's own phrasing (`"2.1.204 (Claude Code)"`, `"codex-cli 0.142.5"`). A failed probe (`agent_version == None`) skips the check. Refusing to spawn would break every routine update.
- `launcher::is_outdated` still refuses the known-broken codex 0.1.x line: that is a refuse; the drift check is a warn.
- An installed wrapper can disagree with its app-server: one `codex --version` reported 0.144.2 while the initialized app-server advertised `codex-cli 0.144.4` and triggered the drift notice (Pass 25).

## Startup lifecycle

Recorded 2026-10-01 (claude 2.1.287, codex 0.159.3, grok 1.0.46, agy-acp 1.2.1).

- The shared harness allows 60 s for every provider's startup and races startup against the kill signal. Startup includes login-shell initialization, MCP discovery and the agent's own startup hooks; the old 20 s deadline left little cold-start margin. An idle initialized session has no expiry.
- The server closes unused failed launches and preserves existing work. It never switches a failure to a PTY automatically. A stalled Codex launch closed at 60.6 s with no terminal or Recent row and kept its diagnostic journal.
- Drivers journal additive `startup_progress {detail}` phase updates before `Init`: Claude tracks bounded pending hook ids (never hook stdout or context), Codex reports config and thread opening, ACP reports authentication and session opening. They are status telemetry, not transcript messages or proof of readiness. `Init`, a fatal Error and `Exited` clear them in the client. Progress never makes an unused failed launch non-disposable; cancellation still interrupts the handshake.
- Chimaera must not silently disable user hooks to shorten startup.
- A real Claude start with a 25-second launch prelude initialized after 26.2 s and stayed idle in chat. All four providers initialized in the isolated daemon/UI without a prompt and accepted their native model controls.

## Ask lifecycle

Recorded 2026-07-10. The reply route for every ask (question, permission, dialog) is a per-driver-process pending map, while the ask itself is journaled and replayed forever. Every driver implements these rules.

- Every reply gets a definitive outcome. A command whose `request_id` no pending map knows (the ask predates this process: respawn, toggle, resume) emits `QuestionResolved` (empty answers) or `PermissionResolved {option_id:"expired"}` plus a Notice, never a silent drop.
- Driver teardown drains pending asks (`Mapper::drain_pending`, called right before `Exited`): each pending question resolves with empty answers, each pending permission or dialog resolves `expired`. A still-parked claude prompt is re-delivered as a fresh request by the next handshake (`pending_permission_requests`).
- `QuestionResolved` carries the user's answers (`answers: {question_id: [labels]}`, serde-defaulted; empty = resolved without an answer). Clients fold the question and chosen labels into the transcript; replay rebuilds it.
- `QuestionRequest.expires_at_ms` is an optional absolute Unix deadline, derived from codex `autoResolutionMs`. The driver stays authoritative; clients only present the countdown; old journals replay with no deadline; new journals never restart the clock on reconnect (Pass 20).
- `option_id` vocabulary on `PermissionResolved`: the driver's option ids on a user decision; `"cancelled"` when the agent withdrew its own ask (claude `control_cancel_request`, codex `serverRequest/resolved`); `"expired"` when the reply route died.
- Questions render as QuestionCards and emit no `ToolCall` row. `ChatInfo.pending_permission` and the server's `NeedsPermission` rail state cover questions too.
- `PermissionRequest.plan: Option<String>`: present means a plan-approval card. `AgentCommand::Permission.feedback: Option<String>` carries deny reasons and plan-approval comments; absent or empty = the bare decision (Pass 9).

## When the agent says "the harness blocked me"

Recorded 2026-07-10. That blockage usually happened below chimaera's event layer, and no permission card can exist.

- Claude's own hook layer (a repo's destructive-command PreToolUse guard) denies inside the CLI; the wire carries only the failed tool_result. Chimaera does not synthesize a card from prose; the tool card's failure output is the record.
- Codex full access maps to `approvalPolicy: "never"`, so codex auto-declines instead of asking. The driver emits a once-per-turn Notice ("full access never asks — switch to auto mode to be asked") when a declined item lands in full-access mode. Remapping full access to an asking policy would be a product decision.
- Claude unknown dialog kinds are answered `cancelled` (cancel is the safe floor) with a visible Notice naming the kind.
- Claude unknown control-request subtypes stay parked with a once-per-subtype Notice; see [Cross-agent invariants](#cross-agent-invariants).

## User message delivery

- `UserMessage` carries the client-minted id the driver stamps on the outbound frame (`id` = claude's checkpoint uuid / codex's `clientUserMessageId`) and `queued: bool`, true when the agent has not consumed the message at echo time. A later `{"type":"user_message_update", "id", "state":"sent"|"dropped"|"cancelled"}` resolves it. Replay is self-correcting: queued-then-sent renders once, and queued-never-sent replays dropped (Pass 8; `cancelled` Pass 12).
- `sent` means the agent read it at a step (claude `command_lifecycle started`, codex the steer's `userMessage` item), or chimaera delivered it as a turn opener (Pass 38). `dropped` means genuinely undeliverable: the process died, or a write never shipped, or the agent swept it without chimaera asking (Pass 14; Pass 38). `cancelled` means the message never happened: the UI drops the bubble and replay folds it to nothing.
- `AgentCommand::CancelQueued{id}` (`{type:"cancel_queued", id}`) is the universal pull-back and dismiss. For an already-resolved id it emits a tombstone `Cancelled`: the reducer removes a `dropped` bubble and no-ops for a `sent` one (seq order guarantees `sent` folds first) (Pass 12; Pass 14).
- `AgentCommand::SendAfterTurn{blocks}` (`{type:"send_after_turn"}`) and `AgentCommand::SendNow{id}` (`{type:"send_now"}`) are additive; `UserMessage.after_turn` rides only with `queued`. `SteerQueued` stays accepted for old clients (Pass 38).
- A block resolved `sent` while a turn runs is `midTurn` in the UI; the turn-end artifact scan looks past it (Pass 38).
- A fresh-turn send echoes `queued:false` (omitted) and gets no update. Transcript-seeded messages carry no `id` (Pass 8).
- `SendIfRunning` joins the running turn or settles `Dropped`; it never opens a turn (Pass 39). Its echo pairs with its caller-minted key, never by position.
- `UserMessage.origin` tags `remote` (claude Remote Control), `restart` (the restart pick-up message), `agent` and `mastermind` (agent communication). The manager stamps daemon-sent origins through its Send↔echo reservation, so no driver carries the tag (Pass 30; Pass 32; Pass 39).
- Rewind (`find_fork_cut`) tombstones every queued echo before the cut whose delivery is not also before it. The "checkpoint right after its own echo" shortcut matches the echo's id; id-less pre-upgrade echoes still count as their checkpoint's own. The Timeline never titles the next turn with a message read mid-turn (Pass 38).

## User stop: TurnAborted.interrupted

- `TurnAborted.interrupted: true` marks an abort the driver positively knows was user-initiated. The session rail (`chat.rs`) and the chat notice render those as a quiet "interrupted" instead of an error; reason-string matching survives only for pre-upgrade events (Pass 8).
- claude: the `is_error` result's `result` string never reliably says "interrupt", so the driver records its one deterministic fact: it sent the `interrupt` control. The flag arms on `AgentCommand::Interrupt`, is consumed at every result and cleared on a fresh turn; with the flag armed and no result string, the reason is "interrupted". The directive deny (`interrupt:true`) does not set the flag (Pass 8). With `interrupt_send_now_v1`, a Send now's `stopped` receipt arms it and `nothing_waiting` clears it (2026-10-05).
- codex: `turn/completed {status:"interrupted"}` maps to `interrupted: true`; a failed turn stays `false` (Pass 8; Pass 31).

## Interrupt watchdog

Recorded 2026-07-11 (Pass 11, claude 2.1.206 / codex 0.142.5; the `was_active` guard re-verified at every later pin).

- Interrupt-when-idle is a no-op on both native wires: claude's `interrupt` acks nothing about the turn, and codex answers "no active turn to interrupt". A turn wedged with no end had no escape.
- `AgentCommand::Interrupt` arms a grace deadline counted down on the harness `tick` (`INTERRUPT_GRACE_TICKS`, 1.5 s of `COALESCE_INTERVAL_MS` = 100 ms ticks, `driver.rs`). If it expires with a turn still open, the mapper synthesizes `TurnAborted {reason:"interrupted", interrupted:true}` and clears turn state. It is idle-guarded.
- A genuine end (claude's `is_error` result, codex's `turn/completed {interrupted}`) lands within the grace and disarms the watchdog through the per-turn reset. A fresh turn opening also disarms it. Both turn-end paths no-op when the turn is already closed, so a late real end never emits a second turn end.
- On firing, claude flushes its held queue (best-effort write; a failed write tears the driver down and the `flushing` stage drops the batch). Codex resolves held entries `dropped`, because a re-driven `turn/start` would strand against a wedged process (Pass 14).

## Stop and teardown

- The harness SIGTERMs the child before closing stdin on every stop chimaera initiates (`ChildGuard::terminate`: kill, dead receiver, protocol error), waits `KILL_GRACE` (3 s), then SIGKILLs. A child that closed its own output is left to exit with its own status. This covers closing a chat, view switches, rewinds and daemon exit (Pass 32).
- A graceful daemon stop runs that stop for every live chat driver (`chat::stop_all_for_exit`, after the ledger's final flush) instead of the runtime's drop-time SIGKILL (Pass 32).
- Neither agent restarts its own background work on resume (Pass 32). See [Restart carryover](#restart-carryover).

## Restart carryover

- The pump folds a per-session `Carryover`: bridge on, ultracode, turn in flight, running non-ambient background tasks (Pass 32), and the pending wakeup, the jobs and the goal without counters (Pass 47). The ledger snapshots it before the stop, because teardown journals the bridge off and an empty background set.
- Resurrection restores the bridge and ultracode through the spawn. When work was cut off, it sends the agent one message listing the tasks with kinds, labels and ids; the echo carries `UserMessage.origin: "restart"`. The default wording asks the agent to restart each task "the same way you started it, unless it is clearly no longer needed (say which you skipped)"; a softer wording got no restart (Pass 32, claude 2.1.283).
- A lost wakeup counts as interrupted work: the pick-up message names it ("…due in 6 minutes" / "which came due 2 minutes ago") and asks the agent to reschedule it with ScheduleWakeup (Pass 47).
- Claude's jobs and goal seed the new driver (`SpawnSpec.carried_jobs`, `carried_goal`) only when the spawn resumes the conversation (`SpawnSpec.resumes`). A Claude respawn the ledger did not seed (a view switch, a relaunch, a reopen) falls back to the journal's last `Goal` and `Schedule` (`Journal::reopened`). The manager journals `Schedule {jobs}` and `Goal` at every spawn ahead of the driver's events; Codex re-reads its thread's goal (Pass 47).
- A message still waiting when the old process died without its teardown (a hard daemon stop: crash, SIGKILL, power loss, a cluster job ending) is settled `dropped` at the next spawn, ahead of the driver's events (`Journal::reopened().waiting`): the queue it waited in died with that process, and the new driver ignores an id it never minted, so before this the bubble read "next step" on an idle chat forever and Send now did nothing (Pass 57).
- A pick-up is withheld when the chat's last one (`pickup_at_ms`) went out less than 10 minutes earlier, so a crash loop cannot start a billed turn per crash. `chat.resumeAfterRestart` off sends no message (Pass 32).
- The turn-end "done" notice is held while anything is scheduled, so a loop's notice is its last tick's (Pass 47).

## Conversation branches

Recorded 2026-07-19 (Pass 24).

- `POST /api/v1/sessions/{id}/fork` snapshots the normalized journal through a selected sequence, creates a distinct chimaera session, and never stops or rewrites the source. The old rewind endpoint, by contrast, stops the process, truncates the journal and resumes at an earlier point.
- An assistant action branches after the response with an empty composer; a user action branches before that prompt and restores its text as an unsent draft. The path is called native only when the exact preceding boundary is available (claude: [claude-code.md#checkpoints-rewind-and-branches](claude-code.md#checkpoints-rewind-and-branches); codex: [codex.md#rewind-compaction-and-fork](codex.md#rewind-compaction-and-fork)).
- Portable handoff (cross-agent branches, sources without a reusable native id, points the vendor cannot represent): copy the visible journal prefix for replay, render a bounded transcript from user/assistant/tool/question/notice events, and install it on a fresh target. The render drops thinking, configuration, init and lifecycle bookkeeping; an early `queued` echo is included only if a later update inside the cut says it was sent (otherwise both it and its checkpoint are removed). The model-facing prompt is capped to a 32 KiB head plus a 184 KiB recent tail.
- An additive `Forked {source_agent, source_seq, native}` event separates the copied prefix from the destination's own events. After a portable import, copied checkpoint uuids and turn ids belong to the old conversation and are never offered back to the new native session: the UI clears their native affordances, and the server refuses any native boundary at or below the latest portable marker. The floor survives branch-of-a-branch chains; native forks keep prior native boundaries.
- Portable initialization is not an `AgentCommand` and creates no user event: Claude gets the prompt from a mode-0600 file through `--append-system-prompt-file`; Codex gets it in the open request's `developerInstructions`; ACP holds it until the first real prompt. Opening a branch starts no billed turn. The `Forked` marker lets resurrection rebuild the same standing context after a daemon restart.

## Execute commands and sent images

- An execute `ToolCall` (claude `Bash` / `PowerShell`, codex command executions) carries the additive `command: Option<String>`: the full command, clipped to 8 KiB head+tail (`clip_command`). The `title` stays the ~120-char label. `None` is skipped on serialization (Pass 36). The web UI's turn-end block scans `command ?? title`.

### Sent images

- `ContentBlock::Image` gains an additive `path: Option<String>` and `UserMessage` an additive `attachment_paths: Vec<String>`. The daemon's `/ws/chat` ingress (`upload::save_send_images`) saves each image of a `Send` into the session's upload landing pad and stamps `path`; a client-supplied value is always discarded first, and `validate_ingress` bounds it at `COMMAND_PATH_MAX` (Pass 37, claude 2.1.283 and codex 0.157.1).
- Both drivers copy the stamped paths into the echo through `model::image_paths` and never hand `path` to the agent: claude's stream-json `image` block and codex's `data:` URL input are byte-identical to before. `attachment_paths` can be shorter than `attachments` (a failed save; Remote Control and transcript-seeded messages carry none). Consumer: the web UI's `AttachmentStrip` (Pass 37).

## Model observations

Recorded 2026-10-02.

- An assistant error message's `model: "<synthetic>"` is a placeholder. It must not produce `ModelSwitched` or enter resume or preference state.
- Real assistant-message model observations use `reason: "reported"`. Only a successful `set_model` acknowledgement uses `reason: null`, which persists a user preference. The catalog's `value` stays the exact argument sent to `set_model`.

## MCP elicitation forms

Recorded 2026-10-03 (claude 2.1.288; codex 0.159.3 schema). Provider shapes: [claude-code.md](claude-code.md#asks-permissions-plan-approval-questions-dialogs-elicitation), [codex.md](codex.md#mcp-tool-calls-and-elicitation).

- The shared normalized schema (`elicitation.rs`) accepts standard scalar, enum and multiple-choice forms plus nested objects. Unknown constraints refuse acceptance visibly.
- Driver pending maps keep the normalized constraints, validate replies, and withdraw asks on native cancellation or driver teardown. Resolution events contain the action, never the private form values.
- Input requests are distinct from tool approvals. Forms and URL requests never inherit an MCP tool-call consent.
- Live in the isolated daemon UI: a typed form with a nested object was submitted, a pending browser request survived a reload and was then cancelled, and replay kept the card and decision.

## Agent communication carriers

Each runtime's way to deliver another agent's message to a working agent at its next step, without starting a turn the user's wake policy did not allow (Pass 39; Pass 44).

- Claude: a PostToolUse hook's `additionalContext` (the daemon's `type: http` hooks), claimed once; never into a subagent. The daemon journals the delivered message as `AgentMessage` through `ChatManager::annotate`, because nothing on stdout records it. A parent waiting on a foreground subagent has no next step; with `agents.communication.freeSubagents` on (default off), a direct message moves each foreground subagent to the background (`background_tasks {tool_use_id}`), so the parent's PostToolUse carries the message (`Plan::Free` in `comms.rs`). Off, the hook carrier waits for the subagent, and the sender is told so.
- Codex: `SendIfRunning` steers the running turn; a steer also ends a collab `wait` at once.
- ACP: no mid-turn input; the message waits for the turn to end and meets the wake policy.
- `ChatManager::main_wait(id)` names what the running turn waits on (the turn's start and its open `cross_turn: false` calls classed `Subagent` (claude `Agent`/`Task`, codex collab `wait` by `COLLAB_WAIT_TITLE`), `Command` or `Tool`; claude's synthesized `task:` rows excluded). It is not on the session-list wire. `message_agent` waits up to 8 s for a message to be read and otherwise names the wait ("it's running a command (12 min: cargo test); it reads this when that command ends"). Broadcasts never move anyone's subagent (Pass 44).
- Claude Code's own `ListAgents` / `SendMessage` reach machine-wide Claude peers, not workspace agents; chimaera neither denies nor intercepts them (Pass 39).

## Extensions and MCP inventory

- `claude mcp list` reports `name: endpoint - status`. Claude 2.1.284 emits `✔ Connected` and `! Needs authentication` (not the warning glyph). Keep unrecognized statuses unknown and discard endpoints before serialization (Pass 40).
- Both CLIs advertise `mcp login --no-browser <name>`. Local MCP flows accept the complete browser callback URL on stdin. Claude needs both stdin and stdout to be TTYs: piped stdin is refused; a TTY only on stdin prints the link but never reads the pasted callback; an internal raw PTY on all streams prints `Or paste the redirect URL here:` and consumes a URL followed by CR (the URL may use an OSC hyperlink envelope). Codex accepts ordinary pipes and LF (Pass 40, claude 2.1.284, codex 0.157.1/0.158.0).
- Claude's help includes claude.ai connectors. A hosted login printed `Visit this URL to authorize:` and an HTTPS claude.ai `/api/organizations/.../mcp/start-auth/...` URL, then exited 0 at once, with no provider authorization. Following that URL for a custom entry produced Claude's `mcp_registration_failed` page. So the hosted CLI status is not proof that OAuth is the next action: hosted setup opens Claude's connector settings without `mcp login`, and the dialog marks success only after the agent reports connected. Never map a login process exit alone to successful authentication. The daemon cannot observe browser error pages (Pass 40).
- Sign-in jobs are workspace-scoped, in-memory, bounded and cancellable. The agent owns OAuth and credentials; no workbench terminal session is created; account login is separate (Pass 40).
- Claude plugins: `claude plugin list --json` → `[{id, version, scope, enabled, installPath, installedAt, lastUpdated}]`; `--available` adds `{installed:[…], available:[{pluginId, name, description, marketplaceName, source}]}`. `claude plugin details <id>` is human text (component inventory + "Always-on: ~N tok"); only the totals are read, leniently (Pass 35, claude 2.1.259).
- Codex plugin, hook and app inventory: [codex.md#daemon-side-inventory-probes](codex.md#daemon-side-inventory-probes). Grok and Antigravity: [acp.md#native-extension-discovery](acp.md#native-extension-discovery).

## Native-driver compatibility

The latest full results, per gate. Earlier results are in [passes.md](passes.md).

| Gate | Result | Versions | Date |
|---|---|---|---|
| `just chat-smoke` | 31/31 | claude 2.1.300, codex 0.162.1 | 2026-10-14 (Pass 44) |
| `just chat-mods-smoke` | 1/1 | claude 2.1.310 | 2026-10-23 (Pass 52) |
| `just chat-smoke-acp` | passed | grok 1.0.46, agy-acp 1.2.1, cursor-agent 2026.10.14-941492e | 2026-10-15 (Pass 45) |

- Pass 52 ran the Claude half of `just chat-smoke` (21/21) on claude 2.1.310 and moved the Claude pin there; the codex half stays at its own pin.
- Pass 46 ran the codex half of `just chat-smoke` and Pass 47 added three live cases (`claude_wakeup_fires_as_a_named_turn`, `claude_goal_sets_and_clears`, `codex_goal_sets_pauses_and_clears`) on the same pins.
- A full `just chat-smoke` repeats the complete approval, queue, background task, fork, rollback, compaction and driver lifecycle suite, not only an echo prompt (2026-10-01).
- Live suites bill turns. An account weekly limit (`api_error_status: 429`, `is_error: true`, zero model usage) is an environment gate, not wire drift; re-run the gate after the reset before advancing a pin (2026-10-03; Pass 24).
- Model variance can fail a precondition (a model answered without calling `Write`); re-run before calling it drift (Pass 30).
- The `/advisor` command persists to the user's global settings; a claude spawned while it is set inherits it, so do not probe it during a suite run (Pass 33).
