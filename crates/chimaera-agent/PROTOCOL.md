# Agent protocol facts (live-verified + extension-mined)

The wire formats this crate speaks are unversioned. This file records what we
KNOW, how we know it, and what we have not adopted yet. Re-verify with
`just chat-smoke` whenever a CLI updates; pins live in `claude.rs` / `codex.rs`
(`TESTED_*_VERSION`).

Sources:
- **live**: probed against the real CLIs (claude 2.1.206, codex 0.142.5;
  codex 0.144.2 from Pass 16 on).
- **vsix**: mined from the official VS Code extension bundles
  (Anthropic.claude-code 2.1.204, openai.chatgpt 26.5623.141536 — the
  extensions are GUIs over these same protocols).

## Claude Code — bidirectional stream-json

### Spawn surface (vsix + live)

The extension's embedded SDK builds argv from:
`--output-format stream-json --verbose --input-format stream-json`, plus:

| Flag | Notes | Adopted? |
|---|---|---|
| `--permission-prompt-tool stdio` | routes permissions to `can_use_tool` control requests | yes |
| `--include-partial-messages` | token-level `stream_event` deltas | yes |
| `--session-id <uuid>` | pins the native session id at spawn (live: init echoes it) | yes |
| `--resume <id>` / `--continue` / `--fork-session` / `--resume-session-at` | session continuity | `--resume` yes; fork flags = the rewind endpoint |
| `--settings <file>` / `--mcp-config <file>` | same files as our TUI spawns | yes |
| `--model` / `--fallback-model <m>` | fallback when primary is overloaded | model yes / fallback no |
| `--thinking adaptive` or `--thinking` + budget | thinking toggle | not yet |
| `--include-hook-events` | hook events ride the stream itself | not yet (we ingest hooks via HTTP) |
| `--add-dir <dir>` | extra working dirs | not yet |
| `--no-session-persistence`, `--session-mirror` | | no |

Env: `DISABLE_AUTOUPDATER=1` (ours; pins the verified binary).

### Handshake (live)

`system/init` is NOT emitted at spawn — only after the first user message.
The spawn handshake is a client-initiated `initialize` control request; the
CLI answers immediately with the slash-command catalog (names+descriptions).
That catalog is what our composer popover offers; commands ABSENT from it are
unavailable in this mode (the CLI answers "/x isn't available in this
environment") and get native-UI interception instead.

### Control protocol (vsix subtype inventory)

`control_response` MUST nest `request_id` inside `response` (live: top-level
id is ignored and the CLI hangs).

Client→CLI subtypes we implement: `initialize`, `interrupt`,
`set_permission_mode`, `set_model`, `set_max_thinking_tokens`,
`get_context_usage`, `get_usage`, `generate_session_title`, `rewind_files`,
`mcp_status`, `mcp_toggle`, `mcp_reconnect`. CLI→client: `can_use_tool`
(carries `tool_use_id`, `permission_suggestions`, `blocked_path`).

Subtypes the extension ALSO uses (future parity tiers): `get_settings`,
`set_cwd`, `stop_task`, `side_question`, `mcp_set_servers`,
`mcp_authenticate` / `mcp_clear_auth` / `mcp_oauth_callback_url`,
`background_tasks`, `cancel_async_message` (defined but never called by the
extension either), `channel_enable`, `apply_flag_settings`, `reload_skills`,
`reload_plugins`, `message_rated`, `submit_feedback`, `remote_control`,
`claude_authenticate` (+oauth), `seed_read_state`, `ultrareview_launch`.

### Quirks (live)

- Hooks fire normally under stream-json (`--settings` http hooks) — EXCEPT
  `UserPromptSubmit`, which does not fire for stdin `user` messages. Anything
  that hung off that hook (first-prompt capture, `@term:` autolink) must run
  off the protocol's own input path in chat mode.
- A `--resume` forks a NEW native session id; it arrives via `system/init`
  (never pin `--session-id` together with `--resume`).
- Bonus stream frames: `rate_limit_event` (adopted — see pass 4),
  `system/thinking_tokens` `{estimated_tokens, estimated_tokens_delta,
  uuid}` (ADOPTED pass 5: fires during thinking even when the display is
  summarized and no thinking_delta streams — the driver maps it to
  ThinkingTokens, throttled to every ~256 tokens, and the status row shows
  "thinking · ~N tokens"), `system/status` `{status}` (seen at turn start,
  value semantics unprobed), `system/post_turn_summary` (`status_category`,
  `needs_action`, `status_detail`, `summarizes_uuid`) — ADOPTED pass 17
  (mapped to `SessionStatus`, latest-wins), but emission is CONDITIONAL:
  see pass 17 for what actually fires it.
- Never attach two live processes to one native session id (transcripts
  interleave); the view toggle serializes on the chimaera session id.

## Codex — `codex app-server` JSON-RPC 2.0 (JSONL, header omitted)

### Lifecycle (live)

`initialize{clientInfo}` → result → client MUST send `initialized`
notification → `thread/start{cwd, approvalPolicy?}` → `result.thread.id`
(= sessionId; rollout path under ~/.codex/sessions). Turns:
`turn/start{threadId, input:[{type:"text",text}]}`.

### Notifications (live)

`thread/status/changed{status:{type:idle|active}}`, `turn/started`,
`item/started|completed` (item types seen: `userMessage`, `reasoning`,
`agentMessage` with `phase`, `commandExecution`), `item/agentMessage/delta`
{itemId, delta}, `thread/tokenUsage/updated` (totals + last +
modelContextWindow), `account/rateLimits/updated` (usedPercent, resetsAt,
planType), `turn/completed{turn:{status, durationMs}}`.

`commandExecution` item: `{id, command, cwd, status:
inProgress|completed|declined, commandActions[{type, command}],
aggregatedOutput, exitCode, durationMs}` — `commandActions[].command` is the
bare command (nicer title than the `/bin/zsh -lc '…'` wrapper).

### Approvals (live)

Server→client JSON-RPC REQUEST `item/commandExecution/requestApproval`
`{threadId, turnId, itemId, command, cwd, commandActions,
proposedExecpolicyAmendment, availableDecisions:["accept",
{acceptWithExecpolicyAmendment:{…}}, …]}` — answer by JSON-RPC id with
`{"decision":"accept"}` / `{"decision":"decline"}`. **Any unrecognized
decision string is silently treated as a decline** (live: "approved" declined
the command). File changes have an analogous `requestApproval` (shape TBD —
capture before mapping).

## Cross-agent invariants

- One normalized event model (`model.rs`, ACP-shaped); drivers translate.
- Caps at event construction, not sinks (login-node budgets).
- Handshake watchdog + degrade-to-PTY is per-driver mandatory behavior.

## Version detection (both drivers)

Neither wire protocol carries a version handshake we can depend on:

- **claude**: the `initialize` control response is the command + model catalog
  (see pass 5) — it carries NO version field.
- **codex**: the `initialize` RESULT carries `userAgent` (+ `codexHome`,
  `platform`), but it is the server's own phrasing, not a stable version
  contract, and the driver handshake discards it.

So the version comes from OUTSIDE the wire: the server probes `bin --version`
(`launcher::probe_version`, 2s budget), stores the first line on
`AgentDetection.version`, and — kept fresh across in-place updates by the
cache-staleness stamp (see `validate_cache_hit`) — threads it through
`ChatRecipe.version` → `SpawnSpec.agent_version` into the driver harness.

The harness (`run_driver`) then, once past the handshake:

1. **Journals it on `Init`** (`AgentEvent::Init.agent_version`, additive/
   optional) so a drifted binary is diagnosable after the fact from the
   journal alone. Both mappers echo `spec.agent_version` verbatim.
2. **Warns, never blocks, on drift**: if the probed line does not *contain*
   the driver's `TESTED_*_VERSION` (`Driver::tested_version()`), it emits a
   NON-FATAL `Notice` naming both versions. Substring (not equality) because
   the probe line is the CLI's own phrasing — `"2.1.204 (Claude Code)"`,
   `"codex-cli 0.142.5"`. Refusing to spawn would break every routine update;
   the wire is *usually* compatible, and the journaled notice is the
   ready-made diagnosis the one time it isn't. A probe that failed
   (`agent_version == None`) skips the check entirely.

The old hard gate stays orthogonal: `launcher::is_outdated` still refuses the
known-broken codex 0.1.x line — that is a *refuse*, this is a *warn*.

### Ask lifecycle (questions + permissions) — 2026-07-10

The reply route for every ask (question / permission / dialog) is a
per-driver-process pending map, while the ask itself is journaled and
replayed forever. Three rules reconcile those lifetimes; both drivers
implement all three:

- **Every reply gets a definitive outcome.** A command whose `request_id`
  no pending map knows (the ask predates this driver process — respawn,
  toggle, resume) emits `QuestionResolved` (empty answers) /
  `PermissionResolved{option_id:"expired"}` plus a `Notice`, never a
  silent drop. The journaled resolution un-wedges every attached client
  and every future replay.
- **Driver teardown drains pending asks** (`Mapper::drain_pending`, called
  by the harness right before `Exited`): each pending question resolves
  with empty answers, each pending permission/dialog resolves `expired` —
  so no journal ever ends on a dangling ask. A still-parked claude prompt
  is re-delivered as a fresh request by the next handshake
  (`pending_permission_requests`), so nothing answerable is lost.
- **`QuestionResolved` carries the user's answers** (`answers:
  {question_id: [labels]}`, serde-defaulted — additive; empty = resolved
  without an answer: cancelled/expired/old journal). Clients fold the
  question + chosen labels into the transcript; replay rebuilds it.

`option_id` vocabulary on `PermissionResolved`: driver option ids on a
user decision, `"cancelled"` when the agent withdrew its own ask
(claude `control_cancel_request`, codex `serverRequest/resolved`),
`"expired"` when the reply route died (teardown drain / stale reply).

Related: claude's `AskUserQuestion` tool_use no longer emits a `ToolCall`
row (the QuestionCard is the surface; a bare "AskUserQuestion" row with a
stuck spinner was noise) — codex's `requestUserInput` parent item never
produced one. `ChatInfo.pending_permission` and the server's
`NeedsPermission` rail state now cover questions too.

### "The harness blocked me" — what it is and what surfaces (2026-07-10)

When the agent's prose says a "harness" blocked it, that blockage usually
happened BELOW chimaera's event layer and no permission card can exist:

- **claude's own hook layer** (e.g. a repo's destructive-command
  PreToolUse guard) denies the tool call inside the CLI; the wire carries
  only the failed tool_result. Chimaera cannot (and should not) synthesize
  a card from prose — the tool card's failure output is the record.
- **codex full-access** maps to approvalPolicy `"never"` (the official
  extension's exact table, kept deliberately): codex auto-declines
  instead of asking, so no `requestApproval` exists. The driver now emits
  a once-per-turn Notice naming the mechanism ("full access never asks —
  switch to auto mode to be asked") when a declined item lands in
  full-access mode. Remapping full-access to an asking policy would
  diverge from the mined table — a product decision, not taken here.
- **claude unknown dialog kinds** are answered `cancelled` (result
  strings for unmined kinds are unknown — cancel is the safe floor) but
  now with a visible Notice naming the kind.
- **claude unknown control_request subtypes** (hook_callback,
  mcp_message, elicitation, oauth refreshes…) are deliberately left
  unanswered — the CLI parks them until its own deadline or another
  client settles them, and an error reply could break flows that rely on
  that fallback — but a once-per-subtype Notice names what is waiting.

## Extension mining, pass 2 (2026-07-08 — vsix)

### Claude: slash-command execution model

The extension hardcodes NO command list — the palette mirrors the CLI's
reported catalog (`claudeConfig.commands`, each `{name, description,
argumentHint, aliases}`; plugin duplicates invoke via a namespaced
`plugin:name` alias). Everything is sent to the CLI as `/name` prompt text
except: `/remote-control|/rc` (client-side toggle), `/context` and `/usage`
(open native panels). **Slash sends bypass the message queue.** Command
results round-trip as user-message text wrapped in
`<local-command-stdout>`/`<local-command-stderr>`. The permission-mode cycle
is `default → acceptEdits → plan` (+ `auto` when `autoModeAvailability` is
`"available"`); thinking toggles via `setThinkingLevel("off"|"default_on")`.

### Claude: permission dialog semantics

Accept → `{behavior:"allow", updatedInput, updatedPermissions}`; the
"always" button re-stamps `permission_suggestions` with a user-chosen
destination — cycler over `localSettings` ("this project (just you)",
.claude/settings.local.json) / `userSettings` ("all projects") /
`projectSettings` ("this project (shared)") / `session` ("this session",
unsaved). Suggestion types: `addRules`, `addDirectories`, `setMode`.
Deny → `{behavior:"deny", message, interrupt}` with the directive constant
("The user doesn't want to proceed… STOP what you are doing and wait…",
`interrupt:true`); feedback-denials append the user's reason with
`interrupt:false`. Plan approvals: "Yes, and auto-accept" / "No, keep
planning"; plan comments ride `updatedInput.{userFeedback,userComments}`.

> **Deny → abort (needs live re-verify in chat-smoke).** Because the standard
> deny carries `interrupt:true`, the CLI ABORTS the turn — it emits an
> `is_error:true` result (→ `TurnAborted`), NOT a success result. `fake-claude`
> now mirrors this. UNVERIFIED: `on_result` clearing the `queued_sends` FIFO
> on any `is_error` result assumes the CLI drops its native stdin queue with
> the aborted turn (each cleared uuid now also emits
> `UserMessageUpdate{dropped}` — see pass 8 — so the journal records the
> drop); the driver also defensively opens an implicit turn if a
> stream/assistant/tool frame arrives with `turn_active == false`, so a wrong
> assumption degrades to a correct boundary instead of a phantom turn. Confirm
> the real queue-after-abort behavior and delete this note.

### Claude: checkpoints / rewind (superseded by pass 4 — now built)

Checkpoint key = the USER MESSAGE UUID. Control request
`{subtype:"rewind_files", user_message_id, dry_run}` →
`{canRewind, filesChanged[]}` (dry-run feeds the confirm dialog). The
conversation side forks: `--fork-session --resume-session-at <uuid>` where
the uuid is the message PRECEDING the selected user message. To support
this, our journal must record the CLI's message uuids.

### Codex: the settings/model/effort truth

`turn/start` carries the full per-turn config: `{threadId,
clientUserMessageId, additionalContext, input, environments, cwd,
approvalPolicy, approvalsReviewer, sandboxPolicy, permissions ( ":read-only"
| ":workspace" | ":danger-full-access" ), runtimeWorkspaceRoots, model,
serviceTier, effort, multiAgentMode, summary, personality, outputSchema,
collaborationMode, attachments}`. `thread/settings/update {threadId,
...settings}` changes them mid-thread — the extension probes it and falls
back to per-turn fields on method-not-found. Efforts: `minimal|low|medium|
high|xhigh` (+gated `max`/`none`/`ultra`); default model `gpt-5.5`, default
effort `medium`. Wire param is `effort` (webview calls it reasoningEffort;
collaborationMode.settings uses snake_case reasoning_effort).
**`model/list` `{includeHidden, cursor, limit}` → `{data:[{model,
defaultReasoningEffort}]}` — adopt for the model picker instead of a curated
list.** `collaborationMode/list` → plan/default modes
(`{mode, settings:{model, reasoning_effort, developer_instructions}}`).

### Codex: approvals, fully

Decision is a STRING-OR-OBJECT union: `"accept"` | `"acceptForSession"` |
`"decline"` | `{acceptWithExecpolicyAmendment:{execpolicy_amendment}}` |
`{applyNetworkPolicyAmendment:{network_policy_amendment}}`. Approval kinds:
exec, patch (`item/fileChange/requestApproval`, params carry `{itemId,
grantRoot?, reason?}`), network ("allow this host…"). File-change approvals
accept only accept/acceptForSession/decline. UI wording: "Yes" / "Yes, and
don't ask again this session" / "Yes, and don't ask again for commands that
start with {cmd}".

Pass-4 corrections (adopted): `availableDecisions` does NOT exist in the
current extension — the CLIENT composes object decisions from request params:
exec approvals carry `proposedExecpolicyAmendment` (array of command tokens;
invalid if joining them would contain a newline) and network approvals are
regular `item/commandExecution/requestApproval`s with
`networkApprovalContext.host` + `proposedNetworkPolicyAmendments` (pick the
`action:"allow"` entry, send it back VERBATIM inside
`applyNetworkPolicyAmendment.network_policy_amendment` — snake_case key,
camelCase amendment). File-change approvals resolve their diff by `itemId`
against the already-streamed fileChange item.

### Codex: fileChange item

`{type:"fileChange", id, status: inProgress|completed|failed|declined,
changes:[{path, diff, kind:{type: add|delete|update, move_path?}}]}` —
`diff` is FULL CONTENT for add/delete, unified hunks for update
(`move_path` = rename). Live patches stream via `item/fileChange/patchUpdated`
`{itemId, changes}` (wholesale replace) — **ADOPTED**: the driver re-runs the
fileChange upsert on each patchUpdated so `item_locations` and the Edit card
stay current, and an approval arriving after it names the right files.
Reasoning deltas stream via `item/reasoning/textDelta` and
`item/reasoning/summaryTextDelta`.

### Codex: notification inventory beyond ours

`turn/plan/updated`, `turn/diff/updated`, `item/plan/delta`,
`item/commandExecution/{outputDelta,terminalInteraction}`,
`item/mcpToolCall/progress`, `item/tool/requestUserInput`,
`serverRequest/resolved`, `model/rerouted`, `thread/name/updated`,
`thread/compacted`, plus the full webview method allowlist (thread/fork,
thread/rollback, thread/compact/start, permissionProfile/list,
fuzzyFileSearch, gitDiffToRemote, …) — see the mining transcripts.

## Extension mining, pass 3 (2026-07-08 — vsix, spec completion)

### Claude: usage + context (adopted)

`{"subtype":"get_context_usage"}` → response `{model, totalTokens,
rawMaxTokens, percentage, categories:[{name,tokens,isDeferred}],
memoryFiles:[{path,tokens}], agents:[{agentType,tokens}]}` (camelCase).
`{"subtype":"get_usage"}` (SDK marks it EXPERIMENTAL) → `{subscription_type,
rate_limits:{five_hour, seven_day, seven_day_sonnet, seven_day_opus, …:
{utilization, resets_at}, model_scoped:[{display_name, utilization,
resets_at}]}}` — utilization is 0–100 HERE, but the streamed
`rate_limit_event` uses a 0–1 fraction and epoch-seconds `resetsAt`. Labels:
five_hour="session limit", seven_day="weekly limit", per-model "weekly {name}".

### Claude: thinking toggle (adopted)

Binary: `{"subtype":"set_max_thinking_tokens","max_thinking_tokens":31999,
"thinking_display":"summarized"|null}` (on) / `max_thinking_tokens: 0` (off).
No tiers in this build; spawn-time equivalent is `--thinking`.

### Claude: mentions/attachments wire format

File mentions ride the PROMPT TEXT: `@{rel}`, `@{rel}#L{a}-{b}` (also
`#{a}-{b}` / `:{a}` accepted). Selection context is a separate text block:
`<ide_selection>The user selected the lines {a} to {b} from {path}:\n{text}
\n\nThis may or may not be related to the current task.</ide_selection>`;
terminal grabs are `<terminal name="N">…</terminal>` blocks. Images:
standard base64 image blocks; text files as `document` blocks with `title`.

### Claude: subagents + queueing truth

Subagent status frames: `system/task_started {task_id, task_type,
description, prompt}`, `system/task_progress {task_id, last_tool_name?,
summary?, usage:{total_tokens, tool_uses, duration_ms}}`,
`system/task_notification` (close, with verdict — see below). The official
client HIDES parent_tool_use_id-tagged transcript frames — the visible
surface is the Task tool row ("Agent: {description}"). No client-side
message queue exists: mid-turn user frames go straight to stdin (the CLI
queues); `{"subtype":"cancel_async_message","message_uuid"}` un-queues.
Slash sends bypass queueing.

Live-verified at 2.1.207 (probe 2026-07-15, `-p --output-format
stream-json` stdout): **the subagent tool itself was renamed `Task` →
`Agent`** (the tool_use block's `name`; input still carries `description` +
`prompt` — drivers must match both names), the task frames grew richer, and
background work now rides the same lane. `task_started` carries `tool_use_id` (exact binding to
the spawning tool card — the old opaque-key/description-match caveat is
solved wire-side) plus `subagent_type` for agents; `task_type` values seen:
`local_agent` (Task subagent), `local_bash` (backgrounded Bash — binary
mining also names local_command/local_workflow/remote_agent).
`task_notification` is a rich close: `{task_id, tool_use_id, status
("completed"|"failed"|"stopped"), output_file, summary, usage:{total_tokens,
tool_uses, duration_ms}}`. New subtypes on the same wire:
`task_updated {task_id, patch:{status, end_time}}` (patch semantics) and
`background_tasks_changed {tasks:[{task_id, task_type, description}]}` — a
REPLACE-the-set signal (empty array = none left). The driver binds by
tool_use_id when present (description fallback for older CLIs) and honors
the notification verdict. The background lane (non-`local_agent`
task_started + task_updated + background_tasks_changed + the notification
close) feeds the normalized `background_tasks` level-set event and the
chat UI's background tray.

**Background frame ORDER, live-verified 2.1.207** (raw `-p stream-json`
probe, backgrounded `sleep 8`): at spawn, `background_tasks_changed`
(set WITH the task) arrives immediately BEFORE `task_started` — same
instant. At settle, the removal precedes the verdict by ~10 ms:
`background_tasks_changed {tasks:[]}` → `task_updated {patch:{status:
"completed", end_time}}` → `task_notification {status, summary
("Background command … completed (exit code 0)"), output_file}` — then a
fresh `system/init` and a bare no-turn `result` (the CLI telling the
model). So a driver that forgets a task at the set-change drops the
verdict: chimaera parks removed-but-unverdicted tasks in a bounded
departed buffer and folds the close from the notification (the only frame
carrying summary + output_file). No `task_progress` fires for background
tasks.

**stop_task is generic over the task registry** (binary mining +
LIVE-VERIFIED 2.1.207: a `stop_task {task_id}` with a running local_bash
key killed the shell and produced `task_notification {status:"stopped",
summary: <the description>, output_file}`; a natural finish produces
`status:"completed", summary:"Background command \"…\" completed (exit
code 0)"`):
the control's schema is `{subtype:"stop_task", task_id}` described plainly
as "Stops a running task"; the handler resolves the id from the SAME
registry that holds every task type (local_agent, local_bash,
remote_agent, …) and dispatches a per-type kill driver ("Unsupported task
type" error otherwise). `not_found`/`not_running` errors are acked as
SUCCESS (`{}`), so a stop racing the task's own finish is harmless; the
kill emits a `task_notification {status:"stopped"}`. So the background
tray's kill button sends the native background task_id through the
existing stop_task control — no separate shell-kill control needed (the
`{task_id, shell_id}` shape seen in earlier mining is registry-internal
state, not a distinct control). The `background_tasks` CONTROL (Ctrl-B
parity) is the other direction: `tool_use_id` present backgrounds that one
task, ABSENT backgrounds all foreground tasks (Bash commands and
subagents); each backgrounded tool call returns a "running in the
background" tool_result immediately and settles later via
task_notification.

### Codex: model/list + settings + steer (partially adopted)

`model/list {includeHidden, cursor, limit}` → `{data:[{model, hidden,
isDefault, defaultReasoningEffort, supportedReasoningEfforts:
[{reasoningEffort, description}]}]}` — the model picker's source of truth.
`thread/settings/update {threadId, model|effort|collaborationMode|
permissions|personality|serviceTier|multiAgentMode}` with feature-detect
fallback to per-turn `turn/start` fields (our current behavior IS the
fallback path). `turn/steer {threadId, clientUserMessageId, input,
expectedTurnId}` — on mismatch parse the live turn id from the error and
retry. collaborationMode: `{mode: plan|default, settings:{model,
reasoning_effort, developer_instructions}}`. `personality`:
friendly|pragmatic. `summary` is hardwired "none". No `review/*` RPCs exist
(tolerate `enteredReviewMode`/`exitedReviewMode` items silently). Sandbox
RPC spelling is camelCase (`workspaceWrite`), config spelling kebab-case.
Composer agent modes (UI → wire): read-only→(:read-only, on-request),
auto→(:workspace, on-request), full-access→(:danger-full-access, never;
confirm dialog), guardian→approvalsReviewer guardian_subagent; approvalPolicy
may be a granular OBJECT, not just an enum.

## Extension mining, pass 4 (2026-07-08 — vsix, adoption pass)

Everything below is IMPLEMENTED in the drivers; live assertions ride
`just chat-smoke`.

### Claude: checkpoints/rewind (adopted)

The user-message uuid is CLIENT-MINTED: the extension writes
`{type:"user", uuid: crypto.randomUUID(), session_id:"", parent_tool_use_id:
null, message:{…}}` to stdin — our driver does the same, so every send has a
checkpoint anchor before any frame returns. `rewind_files
{user_message_id, dry_run}` → `{canRewind, filesChanged[], insertions,
deletions, error?}` restores FILES on the live channel (no restart; the
extension then inserts "Code rewind successful"). The conversation side
forks with the uuid of the message PRECEDING the selected user message
(inbound assistant/user frames carry `uuid` — the driver tracks the last
one seen); the extension rewrites transcripts client-side, but the CLI's
`--fork-session --resume-session-at <preceding-uuid>` flags exist in the
bundled SDK and are what chimaera uses (live-verified via chat-smoke +
playground). Fork EXCLUDES the selected message; rewind_files uses the
selected message's OWN uuid. **Checkpointing is OFF under `-p` unless the
spawn env carries `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true`** (the
SDK's `enableFileCheckpointing` option; live: every rewind_files answers
`{"canRewind":false,"error":"File rewinding is not enabled."}` without it).
Checkpoints track the FILE tools (Write/Edit) only — a Bash-created file
reports `filesChanged:[]` and survives the rewind (live). The rewind UI's
file list is therefore honest about exactly what will revert.

### Claude: adopted control shapes

- `generate_session_title {description, persist:false}` → `.title`; the
  extension fires it at the FIRST user send with description = message text
  (ours does too; result feeds the workbench naming chain as an ai-title).
- `mcp_status {}` → `{mcpServers:[{name, status: connected|failed|
  needs-auth|pending|disabled, scope, config, error?, tools:[{name,
  annotations}]}]}` (own IDE server filtered out). `mcp_toggle {serverName,
  enabled}`, `mcp_reconnect {serverName}`, `mcp_authenticate {serverName,
  redirectUri}` — params are camelCase.
- `rate_limit_event` frame: `{type, rate_limit_info:{status ("allowed"
  clears; "rejected" = blocked), rateLimitType (five_hour|seven_day|
  seven_day_opus|seven_day_sonnet|seven_day_overage_included|overage),
  utilization (0-1), resetsAt (epoch s), overageInUse}}`. No client-side
  thresholds — render whatever non-allowed arrives.
- Queueing: NO client queue exists — mid-turn user frames go straight to
  stdin and the CLI queues them (live-verified: two results, in order).
  `cancel_async_message {message_uuid}` exists in the SDK; the extension
  never calls it, but ours DOES now, for the `CancelQueued` command — see
  Pass 12 for the un-queue reliability.
- Subagents: `task_started {task_id, task_type, description, prompt}`,
  `task_progress {task_id, description?, last_tool_name?, summary?,
  usage:{total_tokens, tool_uses, duration_ms}}`, `task_notification` =
  close with verdict. task_id is an OPAQUE key — but since 2.1.207 the
  frames also carry `tool_use_id`, so chimaera binds exactly when present
  and falls back to description-matching (older CLIs), synthesizing an
  "Agent:" row when no Task card matches. Maps wipe on `result`; an
  errored/interrupted turn first closes still-open rows as failed. See
  "Claude: subagents + queueing truth" for the full 2.1.207 shapes
  (status enum, task_updated, background_tasks_changed).
- Permission destinations: rule/suggestion field is `destination`
  (localSettings|userSettings|projectSettings|session|cliArg); cycler order
  is that list minus cliArg; the chosen destination re-stamps every
  suggestion EXCEPT setMode (which keeps its own). Labels: "this project
  (just you)" / "all projects" / "this project (shared)" / "this session".
- Thinking: extension-persisted state, not read back from the CLI; spawn
  flags are `--max-thinking-tokens 31999 [--thinking-display summarized]` or
  `--thinking disabled`; mid-session = `set_max_thinking_tokens`.
- `post_turn_summary` is UNUSED by the extension (routed, never consumed) —
  chimaera maps it anyway (`SessionStatus`, latest-wins; pass 17): the
  rail's status line + attention flag.

### Codex: adopted wire facts

- Images ride `input`: `{type:"image", url:<data URL>}` (or
  `{type:"localImage", path}` when a shared fs exists — we use data URLs).
  `turn/start` also carries `clientUserMessageId` (client-minted uuid).
- `turn/steer {threadId, clientUserMessageId, input, expectedTurnId}`; on
  mismatch the live turn id is parsed from the error text
  (``expected active turn id `x` but found `y` ``) and retried ONCE. Used
  whenever a turn is in progress (the composer's type-through).
- `initialize` MUST declare `capabilities:{experimentalApi:true}` or
  `thread/settings/update` answers -32600 "requires experimentalApi
  capability" (live). The extension also declares
  `mcpServerOpenaiFormElicitation` and `requestAttestation:false` — we
  deliberately do not (they change elicitation frames we don't render).
- `thread/settings/update {threadId, ...settings}` FLATTENED camelCase keys
  (model, effort, collaborationMode, permissions, personality,
  multiAgentMode); feature-detect fallback on -32601 / "method not found" /
  "unknown method|variant" → the fields ride each `turn/start` instead.
- Approval-mode table (adopted): read-only → permissions ":read-only" +
  approvalPolicy on-request; auto → ":workspace" + on-request; full-access →
  ":danger-full-access" + never. `permissions` (profile id) and
  `sandboxPolicy` are mutually exclusive on the wire — we send profiles.
  approvalPolicy enum: untrusted|on-failure|on-request|never (granular mode
  sends an object). Plan mode = collaborationMode
  `{mode:"plan", settings:{model, reasoning_effort,
  developer_instructions}}` (snake_case INSIDE settings).
- `item/commandExecution/outputDelta {itemId, delta, threadId, turnId}` —
  plain string, appended live (we cap the stream at TOOL_OUTPUT_HEAD; the
  completed item's aggregatedOutput replaces it).
- Items: `mcpToolCall {server, tool, arguments, status, result (MCP
  CallToolResult), error, durationMs}`; `webSearch {query, action:{type,
  url}}`; `contextCompaction`; `enteredReviewMode`/`exitedReviewMode`/
  `sleep` render nothing. Plans: `turn/plan/updated {plan:[{step, status:
  pending|inProgress|completed}], explanation}` is the todo list;
  `item/plan/delta` streams the PROPOSED plan markdown (plan mode).
- `thread/name/updated {threadId, threadName}` (codex names threads
  itself → feeds chimaera naming); `thread/name/set {threadId, name}` to
  write one.
- Context meter math: `tokenUsage.last.totalTokens` (min'd against
  `modelContextWindow`) — NOT the cumulative total. No baseline subtraction.
- Rate limits: `account/rateLimits/updated` params are ignored by the
  extension; the source of truth is `account/read {refreshToken:false}` →
  `{rate_limit:{primary_window,secondary_window:{used_percent (0-100),
  limit_window_seconds, reset_at (epoch s)}, limit_reached}, plan_type,
  credits}`. UI warns at >=90, blocked at >=100.
- `turn/interrupt {threadId, turnId}`; "no active turn to interrupt" is a
  benign race, treated as already-interrupted.
- `error` notification: `{error:{message, codexErrorInfo}, willRetry,
  threadId, turnId}` — willRetry renders as a transient notice.

## Pass 5 (2026-07-08 — live probe + vsix): models, effort, ultracode

- **The `initialize` control response is the account model catalog**, not
  just the command list: `models:[{value, displayName, description,
  resolvedModel, supportedEffortLevels (low|medium|high|xhigh|max),
  supportsEffort, supportsAutoMode, supportsFastMode,
  supportsAdaptiveThinking}]` plus `account{subscriptionType,…}`, `agents`,
  `available_output_styles`. `value` is what `set_model` accepts;
  `system/init`'s `model` field reports the RESOLVED id, so current-model
  matching must check both. Haiku reports no effort levels (no knob).
- **Effort**: read via `get_settings` → `applied.{effort, ultracode,
  model}`; set via `apply_flag_settings {settings:{effortLevel}}` —
  session-scoped (never persisted to settings files from here). The chips
  re-read after every apply instead of trusting the request.
- **Ultracode**: settings flag (`apply_flag_settings
  {settings:{ultracode:bool}}`), "xhigh effort plus standing
  dynamic-workflow orchestration", session-scoped by design ("interactive
  toggles never persist it" — schema docstring). Gate: model supports
  xhigh && workflows not disabled. Live: enabling forces applied.effort to
  xhigh; disabling keeps the elevated effort until reset.
- **/effort, /ultracode, /workflows, /model, /mcp are NOT in the -p slash
  catalog** (dialog commands) — native UI interception is the only path.
  The "ultracode" PROMPT KEYWORD still works in chat mode
  (workflowKeywordTriggerEnabled, default true): it opts that turn into
  the Workflow tool, whose runs render as ordinary tool cards. /workflows
  the PANEL is TUI-only; the official extension has no equivalent either.
- codex model/list with `includeHidden:false` (the extension's own choice)
  IS the complete account list — two models on a plus plan is correct.

## Pass 6 (2026-07-08 — vsix, model-switch edge cases). ADOPTED.

The "asked Fable a biology question and got rerouted" family — every frame
that can change the serving model mid-conversation:

### Claude

- `system/model_refusal_fallback` `{direction: "retry"|"revert"|"sticky",
  original_model, fallback_model, content (the CLI's own banner text),
  request_id?, api_refusal_category? (e.g. "bio"|"cyber"),
  api_refusal_explanation?, retracted_message_uuids[], uuid, session_id}` —
  safety flagged the reply; the CLI switches to fallback_model, WITHDRAWS
  the flagged output (retry/revert), and retries there. Driver:
  ModelSwitched{retract_current_turn} + Notice(content) + fresh Init (chip
  follows truth); the client drops the turn's trailing prose. The
  `switchModelsOnFlag` setting gates the auto-switch ("When off, your
  session will pause instead").
- `system/model_consent_fallback` `{choice: "consent"|"switch_default"|…,
  fallback_model, persisted_as_default}` — Fable required usage credits;
  the CLI switched to the default model. Mapped the same way.
- assistant frames may carry `supersedes: [uuids]` — the message REPLACES
  earlier output (refusal retries). Driver emits MessagesSuperseded before
  the new content; the client drops trailing prose instead of appending a
  duplicate.
- `system/status` `{status, permissionMode?}` — CLI-initiated mode changes
  (plan exits, applied setMode suggestions) ride here; mapped to
  ModeChanged when the mode actually changed.
- `system/compact_boundary` `{compact_metadata:{trigger, pre_tokens}}` —
  auto-compaction marker → Notice.
- `user_dialog_request` (dialogKinds `fable_overage_consent_prompt`,
  `refusal_fallback_prompt`) only flows when the client declares
  `supportedDialogKinds`; we don't, so the CLI resolves these itself per
  settings — the fallback frames above still tell us what happened.
- NOT mapped, deliberately: `prompt_suggestion`, `system/task_summary`
  (unused by the extension too).

### Codex

- `model/rerouted` `{threadId, turnId, fromModel, toModel, reason}` —
  reasons include safety reroutes (`highRiskCyberActivity`). Driver:
  ModelSwitched + the extension's divider wording ("Your request was
  routed to {toModel}.") + fresh Init. NOTE: the field names are
  fromModel/toModel — the first guess (`params.model`/`params.to`) was
  wrong and silently missed reroutes; fixed with this pass.
- Tolerated silently (as the official client does): `model/verification`,
  `model/safetyBuffering/updated`, `turn/moderationMetadata`,
  `enteredReviewMode`/`exitedReviewMode`/`sleep`, `imageGeneration`,
  `planImplementation` items.

## Pass 7 (2026-07-08 — vsix, the long tail). ADOPTED.

### Claude

- **AskUserQuestion** is a normal `can_use_tool` request (tool_name
  "AskUserQuestion"); input `{questions:[{question, header,
  options:[{label, description?}], multiSelect}]}`. Answer = permission
  ALLOW with `updatedInput:{questions (echoed), answers}` where `answers`
  is keyed by the QUESTION TEXT and each value is the chosen labels joined
  ", " (free text rides as a label; "Other" is client furniture). Esc =
  the standard directive deny. Chimaera renders a QuestionCard, not a
  permission card. Related setting: askUserQuestionTimeout
  (60s/5m/10m/never auto-continue).
- **request_user_dialog**: declared via `supportedDialogKinds` on the
  `initialize` request (we declare refusal_fallback_prompt +
  fable_overage_consent_prompt — declared kinds MUST be answered or they
  park). CLI→client control_request `{subtype:"request_user_dialog",
  dialog_kind, payload (camelCase), tool_use_id}`; answer
  `{behavior:"completed", result}` / `{behavior:"cancelled"}`. Result
  strings: overage `consent`|`switch_default`; refusal `retry_fallback`|
  `edit_prompt`. Outcomes echo later as model_consent_fallback /
  model_refusal_fallback system frames (pass 6).
- **Parked-prompt redelivery**: the initialize response can carry
  `pending_permission_requests` / `pending_user_dialog_requests` (full
  request envelopes) — replayed through the mapper at handshake so a
  reattached client shows the cards instead of a wedged session.
- **prompt_suggestion** `{type, suggestion}`: idle-composer suggestion
  (official: placeholder + Tab; ours: a click-to-insert chip). Cleared on
  send. Setting: promptSuggestionEnabled.
- **background_tasks** `{tool_use_id}` → `{backgrounded}`: backgrounds a
  RUNNING TOOL CALL (Ctrl-B parity) — not a list call despite the name.
  **stop_task** `{task_id}` stops a subagent. Both adopted as commands.
- **system/task_summary**: shape/consumer NOT FOUND anywhere (the SDK
  routes it, nothing reads it) — tolerated.
- Other CLI→client control subtypes seen: hook_callback, mcp_message,
  elicitation, oauth_token_refresh, host_auth_token_refresh; plus
  `{type:"keep_alive"}` frames. Unanswered prompts may be settled by
  another attached client or the CLI's park deadline.

### Codex

- **item/tool/requestUserInput** (server→client REQUEST):
  `{threadId, turnId, itemId, questions:[{id, header, question, isOther?,
  options?:[{label, description}]}], autoResolutionMs?}`. Answer by rpc id:
  `{answers:{<questionId>:{answers:[string,…]}}}` (empty `{answers:{}}` =
  skip; the official client auto-sends empty after autoResolutionMs).
  No multiSelect on this method. Rendered as a QuestionCard.
- **serverRequest/resolved** `{threadId, requestId}`: the server settled
  its own request (timeout / another client / interrupt) — withdraw the
  matching approval/question card.
- **imageGeneration** items `{id, status, revisedPrompt, result,
  savedPath?}` — savedPath is a FILESYSTEM PATH (preferred; result may be
  URL/data-URL/raw base64). Mapped to a tool row whose location opens the
  image in the native preview; the completed re-emit upserts by id
  (clients must upsert tool rows, not duplicate).
- **model/safetyBuffering/updated** `{threadId, turnId, model,
  showBufferingUi, fasterModel?, reasons[], useCases[] (bio/cyber…)}` —
  "additional safety checks" latency notice (once per turn).
- **thread/status/changed** status union: active{activeFlags:
  waitingOnApproval|waitingOnUserInput}|idle|notLoaded|systemError — our
  running/attention states already derive from turn + request events.
- **summaryPartAdded**: official handler is a no-op; sections are really
  delimited by summaryTextDelta's `summaryIndex`. We insert a thought
  paragraph break.
- **steered** is NOT a wire type: steering acceptance = an extra
  `userMessage` item completing inside an in-progress turn (we already
  echo the send; the item is ignored — no dupes).
- **item/autoApprovalReview/{started,completed}** `{reviewId,
  targetItemId, action, review:{status: inProgress|approved|denied|
  timedOut|aborted, riskLevel?, rationale?}}` — the guardian reviewer's
  verdicts; not yet rendered (we don't offer guardian mode).
- item/mcpToolCall/progress: confirmed ignored by the official client too.

## Pass 8 (2026-07-10 — normalized-wire additions): delivery + user-stop

Additive fields/events on OUR normalized model (`model.rs`), pinned in
`tests/wire_contract.rs`; all defaults serialize to nothing, so pre-upgrade
journals replay and failure aborts stay byte-identical on the wire.

### UserMessage delivery: `id` + `queued` + `user_message_update`

`UserMessage` now carries the client-minted uuid the driver already stamps
on the outbound frame (`id` = claude's checkpoint uuid / codex's
`clientUserMessageId`) plus `queued: bool` — true when the agent has NOT
consumed the message at echo time. A later
`{"type":"user_message_update","id","state":"sent"|"dropped"}` resolves it.
Replay is self-correcting: the journal carries the queued echo and the
update through the same reducer, so queued-then-sent renders exactly once
and queued-never-sent replays dropped.

Emission points, per driver:

- **claude** — a mid-turn send echoes `queued:true` and its uuid joins a
  FIFO (`queued_sends`); the CLI queues the stdin frame natively. When the
  running turn's result lands, the oldest uuid resolves `sent` — and NOTHING
  more: the turn boundary opens LAZILY (`ensure_turn`, on that message's
  first real frame), never a synthetic `TurnStarted` per queued pop. See
  Pass 11 — the eager synthetic open was the "stuck running" bug, because
  the CLI produces FEWER results than rapid queued sends. An `is_error`
  result drops the CLI's queue with the turn: every queued uuid resolves
  `dropped` before the `TurnAborted`. A coalesced SURPLUS the CLI never runs
  as its own turn resolves `sent` on the idle-flush (Pass 12), never stuck
  "queued". `cancel_async_message` un-queueing is now adopted for the
  `CancelQueued` command (Pass 12).
- **codex** — a steered send (`turn/steer`, incl. sends buffered during
  the turn/start window and flushed on `turn/started`) echoes
  `queued:true` and resolves `sent` when the steer RPC succeeds (steering
  has no follow-up item we consume — the echoed `userMessage` item is
  deliberately ignored). A steer that fails for good (after the one
  expectedTurnId retry, while a turn is still active) resolves `dropped`
  next to the Error notice. A steer/buffered send re-driven as a fresh
  `turn/start` (the turn ended under it) resolves `sent` at the re-drive —
  it has the same standing as a fresh send from there on.
- Fresh-turn sends on both drivers echo `queued:false` (field omitted) and
  never get an update. Transcript-seeded UserMessages carry no `id`.

### TurnAborted `interrupted: bool` — the structural user-stop signal

`TurnAborted` gains `interrupted: true` when the driver positively knows
the abort was user-initiated; consumers (the session-rail state machine in
chimaera-server `chat.rs`, the chat UI notice) render those as a quiet
"interrupted" instead of an error, keying on the flag — the old
reason-string matching survives only for pre-upgrade events.

- **claude** — the CLI's `is_error` result carries a free-text (often
  absent) `result` string that NEVER reliably says "interrupt", so the
  driver records the one deterministic fact it has: it sent the
  `interrupt` control request. The flag arms on `AgentCommand::Interrupt`
  and is consumed at EVERY result (and cleared on opening a fresh turn),
  so a raced/stale interrupt cannot mislabel the next turn's genuine
  failure. When armed and the result string is absent, the reason falls
  back to "interrupted" (not "turn failed"). The deny-with-`interrupt:true`
  permission path deliberately does NOT set the flag — whether a directive
  deny should read as a quiet stop is a rail-semantics call for the
  maintainer.
- **codex** — `turn/completed` with `status:"interrupted"` (which only
  follows a `turn/interrupt` RPC) maps to `interrupted: true`, reason
  stays codex's own word "interrupted"; `turn/failed` stays
  `interrupted: false`.

## Pass 9 (2026-07-10 — permission-UX parity). ADOPTED.

Plan approvals and deny-with-feedback, closing the two biggest permission
gaps vs the official clients.

### Claude: ExitPlanMode is a plan approval, not a tool permission

- The plan proposal is an ordinary `can_use_tool` request with
  `tool_name:"ExitPlanMode"`; `input.plan` is the plan MARKDOWN (live).
  The driver maps it to a `PermissionRequest` whose additive `plan` field
  carries the (capped) markdown — the client renders a plan-approval card,
  and `input_preview` drops the `plan` key so the journal never stores the
  text twice.
- Options mirror the official card, verbatim and in order: **"Yes, and
  auto-accept edits"** / **"Yes, manually approve"** / **"No, keep
  planning"**.
- Approval = `{behavior:"allow", updatedInput}` where updatedInput echoes
  the input; optional user comments ride
  `updatedInput.{userFeedback,userComments}` (both fields, same text — the
  extension's shape; live: the CLI accepts the injected keys and the turn
  completes). The CLI exits plan mode itself — the mode change rides
  `system/status` (pass 6).
- "Yes, and auto-accept edits": chimaera sends the allow, then a
  `set_permission_mode acceptEdits` control request in the same step (a
  verified control; its ack → ModeChanged). The extension re-stamps a
  `setMode` permission_suggestion instead — we deliberately use the
  explicit control so the behavior doesn't depend on which suggestions the
  CLI happened to attach.
- "No, keep planning" is the deny path: bare = the directive constant with
  `interrupt:true` (model stops, still in plan mode, waits); with comments
  = the feedback-denial below, so the model revises the plan immediately.

### Claude: feedback-denials (live-verified)

`{behavior:"deny", message: <directive constant> + "\n\nThe user's
feedback: " + <reason>, interrupt:false}` — the tool errors (is_error
tool_result) but the turn is NOT aborted: it runs on and ends with a
SUCCESS result (contrast the bare deny's `interrupt:true` → is_error
result → TurnAborted). The driver journals the reason as a `UserMessage`
event, since the model really received it.

### Codex: decline has no message field — feedback steers

The app-server decision union carries no free-text slot, so a decline with
feedback answers the rpc with `{"decision":"decline"}` and then delivers
the reason as user input into the still-running turn via the normal
`turn/steer` path (buffered/turn-started like any send). Same UX as
claude's feedback-denial, realized per this protocol's capability.

### Wire additions (daemon↔UI, strictly additive)

- `PermissionRequest.plan: Option<String>` — present ⇒ plan-approval card.
- `AgentCommand::Permission.feedback: Option<String>` — deny reasons and
  plan-approval comments; absent/empty = the bare decision.

## Pass 10 (2026-07-10 — live probe 0.142.5 + vsix 26.623.101652): codex
rewind, compact, question timeouts. ADOPTED.

### Codex: thread/rollback (live-verified)

`thread/rollback {threadId, numTurns}` → result `{thread:{…}}` (the updated
thread object; same shape as thread/start's). Drops the LAST `numTurns`
turns from the thread in place — the thread id survives, and a follow-up
turn confirms the model no longer sees the rolled-back content. Works
immediately after `thread/resume` (the rewind-respawn path). **An overcount
does NOT error — it silently clamps** (numTurns:99 on a 2-turn thread
empties it), so the count must be exact: an overcount would eat good turns.
The extension's own uses: edit-last-message = rollback 1 on the live
thread + re-send; fork-from-turn = `thread/fork` (thread/start-shaped
params, `ephemeral` flag) then rollback `total - target - 1` on the fork.

Chimaera's rewind: codex Checkpoint events anchor turn-OPENING sends only
(steers join a running turn; rollback can't cut mid-turn). The server
truncates the journal at the anchor, counts the dropped `TurnStarted`
events, and respawns with `thread/resume` + `thread/rollback` of that
count. Known seam: turns run outside the journal (TUI-interleaved via the
view toggle) are invisible to the count — the rollback is only as complete
as the journal. `thread/resume` also answers `initialTurnsPage` (null in
our probes — likely needs a paging param) and a settings echo
(approvalPolicy, permissions, reasoningEffort, …).

### Codex: thread/compact/start (live-verified)

`thread/compact/start {threadId}` → `{}` ack. The compaction then runs AS
ITS OWN TURN: `thread/status/changed active` → `turn/started` →
`item/started`/`item/completed` of a `contextCompaction` item →
`turn/completed`. **No `thread/compacted` notification fires on 0.142.5**
(it exists in the extension's routing table, but the item is the real
signal — our contextCompaction→Notice mapping already covers it).

### Codex: question auto-resolution (adopted)

`item/tool/requestUserInput`'s `autoResolutionMs` is honored driver-side:
at the deadline the driver answers `{answers:{}}` (the official client's
empty-skip), withdraws the card (QuestionResolved), and drops a visible
notice. Claude needs no equivalent — its `askUserQuestionTimeout` runs
CLI-side and unanswered prompts settle via the park deadline.

### Codex: misc notifications seen live (tolerated silently)

`thread/started` (first turn/start), `thread/goal/cleared` (after
rollback), `mcpServer/startupStatus/updated`, `remoteControl/status/changed`.
`userMessage` items carry `clientId` (null unless the client sent
`clientUserMessageId`). `account/rateLimits/updated` params here were
`{rateLimits:{limitId, primary:{usedPercent, windowDurationMins,
resetsAt}, secondary:…}}` — still ignored (account/read is the source).

## Pass 11 (2026-07-11 — turn-boundary robustness): the "stuck running" fix

Two turn-boundary corrections. Both change only WHEN normalized turn events
are emitted, never their wire SHAPE (`tests/wire_contract.rs` unchanged).

### Claude coalesces rapid queued sends — fewer results than sends (live)

> **Superseded for claude by Pass 13.** The lazy-open + per-result FIFO pop
> below was a best-effort *guess* at what the CLI's opaque coalescer did, and it
> could strand a middle message or fire a phantom turn. Pass 13 stops dumping
> queued sends mid-turn entirely (hold-until-flush), so the CLI never coalesces
> *our* sends and the guess is gone. The `was_active` guard and the interrupt
> watchdog below still stand. Kept here as the historical record of why.

**Confirmed live: three rapid mid-turn sends produced only TWO turns.** The
CLI queues mid-turn stdin frames natively (Pass 4), but when several arrive
in quick succession it COALESCES them — it runs fewer follow-up turns, and
emits fewer `result` frames, than there were messages. The count is not
fixed (it depends on timing); the invariant is "results ≤ queued sends".

Consequence for the old driver: it opened a synthetic `TurnStarted` for
EACH queued message the instant the previous `result` landed (eager open).
When the CLI coalesced N sends into M<N results, the surplus synthetic turns
never got a result — `turn_active` stuck true and the UI stuck on
"running"/"starting" forever.

**Fix — lazy queued-turn open + a `was_active` guard** (`claude.rs`
`on_result`):

- A queued pop resolves `UserMessageUpdate{sent}` ONLY. It does NOT mint a
  `TurnStarted`. The turn boundary opens LAZILY through `ensure_turn` when
  that message's first real stream/assistant/tool frame arrives — and a real
  turn always streams content, so a genuine turn still opens exactly once.
- `on_result` captures `was_active = self.turn_active` BEFORE clearing it,
  and emits the turn-END event (`TurnCompleted` on success, `TurnAborted` on
  `is_error`) — plus the once-per-turn `get_context_usage` refresh — ONLY
  when `was_active`. A bare/coalesced result that opened no turn thus emits
  no phantom turn-end. The queued-drop (`is_error` → `dropped`) and the
  `sent` resolution happen regardless of `was_active` (the CLI dropped/
  accepted its native queue either way).

Net: normal turns are byte-identical (content → `ensure_turn` fired →
`was_active` true); coalesced/bare results resolve delivery without a
phantom turn. Codex was already lazy here (turns open on `turn/started`),
so only claude changed.

### Interrupt watchdog (both drivers) — stop ALWAYS recovers

Interrupt-when-idle is a no-op on both wires: claude's `interrupt` control
acks nothing about the turn, and codex answers "no active turn to
interrupt" (Pass 4). A turn wedged with no result / no `turn/completed`
therefore had no escape — pressing stop did nothing.

**Fix — a grace deadline armed on `AgentCommand::Interrupt`**, counted down
on the harness `tick` (`INTERRUPT_GRACE_TICKS` ≈ 1.5s of
`COALESCE_INTERVAL_MS` ticks, in `driver.rs`). When it expires with a turn
STILL open, the mapper synthesizes the abort the CLI never sent:
`TurnAborted{reason:"interrupted", interrupted:true}` + the queue drops
(claude `queued_sends`, codex `drain_queued_sends()`) + turn state cleared.
Idle-guarded, so interrupting nothing stays a no-op.

Why a deadline and not the ack: a GENUINE interrupt lands its real end
(claude's `is_error` result / codex's `turn/completed{interrupted}`) well
within the grace, and that end DISARMS the watchdog through the per-turn
reset (`on_result` / `reset_turn_state`) — so a live turn is never
double-aborted. A fresh turn opening (`ensure_turn` / claude fresh-send /
codex `turn/started`) also disarms it, so an idle-armed grace can't abort
the next legitimate turn. claude gained a `tick` override; codex extended
its existing `tick` (which also runs `expire_questions`). Symmetric.

Belt-and-suspenders for the rare race where a real end lands JUST after the
watchdog fired: both turn-end paths now no-op when the turn is already
closed. claude's `on_result` already gated on `was_active`; codex's
`turn/completed` / `turn/failed` gained the same `was_active` guard — so a
late real end never emits a second `TurnAborted`/`TurnCompleted`.

## Pass 12 (2026-07-11 — queued-message lifecycle): idle-flush + CancelQueued

> **Superseded for claude by Pass 13.** The idle-flush existed only to reconcile
> a *coalesced surplus*; hold-until-flush produces no surplus, so claude's
> idle-flush is deleted. claude's `CancelQueued` no longer sends
> `cancel_async_message` (a held message never reached the CLI — cancel is a
> local removal). The wire additions (`UserMessageState::Cancelled`,
> `AgentCommand::CancelQueued`) and codex's behavior are unchanged.

Two maintainer decisions on the queued (faded) user bubble. Both are strictly
additive on the normalized wire (`UserMessageState::Cancelled` appended;
`AgentCommand::CancelQueued` appended — `tests/wire_contract.rs` pins both), so
pre-upgrade journals replay and old clients are unaffected.

### Idle-flush: a coalesced surplus resolves `sent`, never stuck "queued"

Because claude COALESCES rapid mid-turn sends (Pass 11 — fewer `result` frames
than messages), a surplus queued uuid is never popped by a result and its
bubble would stay faded "queued" forever (until teardown drops it). The
maintainer kept the native coalescing (NOT client-side sequential delivery), so
the fix is at resolution time: once the driver is IDLE with the queue still
non-empty, the CLI has drained/coalesced it — the messages reached stdin the
instant they were written — so resolve every remaining uuid `sent`.

- Mechanism: a tick-counted grace (`IDLE_FLUSH_GRACE_TICKS` ≈ 1.5s in
  `driver.rs`), armed in the harness `tick` when `!turn_active` and the queue is
  non-empty, reset while a turn is active or the queue empties. On expiry every
  remaining queued uuid → `UserMessageUpdate{sent}`. Guarded so a genuinely
  in-flight next turn preempts it (a real turn opening disarms it via
  `ensure_turn`/fresh-send on claude, `turn/started` on codex); a premature
  flush would only mark `sent` early, which is the message's correct terminal
  state anyway (an idle driver has no live turn left to abort it). The
  is_error/interrupt DROP path is unchanged — an aborted turn still drops its
  queue.
- Both drivers, symmetric intent: claude flushes `queued_sends` → `sent` (they
  were written to stdin already); codex RE-DRIVES a stranded `buffered_sends`
  entry as a fresh turn (a codex buffer was never sent, so it is DELIVERED, not
  declared sent unseen) — the defensive rescue for the one seam where a turn
  ends under a pending `turn/start`.

### CancelQueued: pull back a still-queued message

`AgentCommand::CancelQueued{id}` (client frame `{type:"cancel_queued", id}`,
deserialized straight to the driver — no server switch) + the resolution
`UserMessageState::Cancelled`. A cancelled message NEVER happened: the UI drops
the bubble from both the pending stack and the transcript, and replay agrees
(the journaled `UserMessage{queued}` + `UserMessageUpdate{cancelled}` fold to
nothing).

- **claude**: if `id` is still in `queued_sends`, remove it and send the CLI a
  `{"subtype":"cancel_async_message","message_uuid":id}` control request, then
  emit `Cancelled`. If it is no longer queued (already popped `sent`, or
  idle-flushed), emit a `Notice` ("too late to cancel") — it can't be un-said.
- **codex**: if `id` is still in the pre-steer `buffered_sends`, remove it and
  emit `Cancelled`. If it was already steered into the running turn (delivered),
  emit the same `Notice`. Symmetric intent, per-protocol capability.

### `cancel_async_message` reliability — LIVE

The subtype is defined in the bundled SDK but the official extension never calls
it, so its real un-queue effect was unverified. `tests/live.rs`
(`claude_cancel_async_message_behavior`, chat-smoke) queues a distinctively-
answered message mid-turn, sends the cancel for its uuid, and reports whether the
message still ran.

> **Observed (chat-smoke, claude 2.1.206): it UN-QUEUES.** The probe queued
> "Reply with exactly: CANCELME" behind a running turn, sent
> `cancel_async_message` for its uuid, and drained to idle — result: ONE turn
> completed, the CANCELME reply NEVER ran (`results=1, queued_message_ran=false`).
> So the real CLI honors the control request and drops the named message from its
> native queue, contrary to the "SDK-defined but never called" mining note. This
> makes claude's `CancelQueued` a true cancel, not just a local relabel.
>
> Best-effort by design regardless: the driver marks the bubble `Cancelled`
> locally and journals the resolution whether or not the CLI honors the request,
> so the UI stays honest and deterministic. We do NOT rewrite the queue model on
> the strength of it — a future CLI that stops honoring the control degrades to a
> local-only relabel (the message would still run, but the journaled `Cancelled`
> keeps replay consistent), and the live probe would catch the regression.

## Pass 13 (2026-07-11 — claude queued sends: hold-until-flush). ADOPTED.

The maintainer hit it live: several messages queued behind a running turn came
back with a MIDDLE one stranded "not delivered" while its neighbours delivered,
plus a phantom turn "responding to an empty message." Root cause was structural,
not a missed edge case. Passes 11–12 let the driver **dump every queued send to
stdin immediately** and then *reverse-engineer* what the CLI's opaque coalescer
did — pop one queued id `sent` per `result` (FIFO), with an idle-flush timer
mopping up the surplus. That reconciliation is a guess with no ground truth:

- The first `result` after queuing is the END of the pre-existing turn, yet the
  FIFO pop marked a queued id `sent` off it — off by one.
- The CLI coalesces rapid sends into FEWER results than messages, so the
  id↔result mapping drifts; timing decided which id stranded.
- The DROP paths (`is_error`/interrupt/kill) drain the WHOLE queue, so a send
  that arrived after a mid-sequence abort could strand while its neighbours went
  through — exactly the "middle one not delivered."

**Fix — hold, don't dump.** A send that arrives while a turn is active is now
HELD in `queued_sends` (`VecDeque<(uuid, stdin content)>`) and is **not written
to the CLI**. When the running turn's `result` lands, `on_result` flushes the
whole held batch: writes each message to stdin AND resolves each
`UserMessageUpdate{sent}` in that one step. Determinism replaces the guess:

- Delivery is tied to OUR write, not to counting the CLI's results — so the
  result count is irrelevant and no id can strand. If the CLI later coalesces the
  flushed batch, every message is already `sent`; we don't care how many results
  come back.
- No mid-turn stdin write means the delivered bubble lands AFTER the turn's
  `TurnCompleted` (the store appends at the current end), never spliced into the
  still-streaming turn — and the CLI never receives our sends mid-turn, so it
  can't coalesce them into a bare result or a phantom empty turn.
- This is also more faithful to the official client, whose queued messages wait
  for the current turn to finish rather than steering into it.

Deleted with the guess: the per-result FIFO pop, the **idle-flush** (+
`idle_flush_grace`, `IDLE_FLUSH_GRACE_TICKS` usage in claude — the const stays
for codex), and the `cancel_async_message` round-trip in `CancelQueued`.
`CancelQueued` on a held message is now a pure local removal (`Cancelled`, no CLI
frame — nothing reached the CLI to un-queue); once flushed+`sent` the bubble
loses its ✕, and a late cancel finds nothing held → `Notice`. The abort paths
(`is_error`/interrupt watchdog/`drain_pending`) still drop the held queue
`dropped` — now HONESTLY, since a held message was never sent. **[Superseded by
Pass 14: only `drain_pending`/an unshipped flush still drop — a live abort now
flushes the queue, and the late-cancel Notice became a tombstone `Cancelled`.]**
The `was_active` guard and the interrupt watchdog (Pass 11) are unchanged. The
wire SHAPE is untouched (`tests/wire_contract.rs` green); only the TIMING of the
same normalized events changed.

**Codex is deliberately NOT changed.** Its native model is `turn/steer` —
inject into the RUNNING turn — with a per-message RPC answer, so it already maps
each send deterministically and never coalesces/strands. Holding would DIVERGE
from codex-native (the maintainer's "keep native" applies per agent: claude
queues-then-runs, codex steers). A steered codex bubble genuinely joins the
running turn, so its `sent` mid-turn is honest. The two drivers stay symmetric in
*intent* (a queued send is never lost and never splices a claude turn), asymmetric
in *mechanism* because the two agents' protocols differ.

Tests: hermetic `queued_sends_flush_together_on_turn_end`,
`several_held_sends_all_resolve_sent_and_none_strand` (the multi-message case),
`queued_send_flushes_with_no_client_attached` (the flush is daemon-side, off the
CLI's result — a hidden/closed tab can't stall it), `cancel_queued_removes_a_held_send`,
`a_flush_whose_write_never_ships_is_dropped_on_teardown`, and the updated
`bare_result_*` guard; live `driver_rapid_queued_sends_settle_idle` now also
asserts every held id resolves `sent`. `just chat-smoke` re-run for the driver change.

**Flush all-at-once (maintainer's choice), and its accepted edges.** When the
turn ends the WHOLE held batch flushes in one step (all written, all `sent`),
rather than one-per-turn. The maintainer chose batched over one-at-a-time
(2026-07-11), accepting two edges:

- *Grouped ordering.* Several queued bubbles render together, then their
  responses — `[q2, q3, resp2, resp3]`, not interleaved `[q2, resp2, q3, resp3]`.
  A consequence of resolving the batch on the single turn-end result. Not a splice
  (all `sent` land after that turn's `TurnCompleted`, before any next-turn frame).
- *Stop-during-batch.* Only the first flushed message opens a turn; the rest sit
  in the CLI's native queue. Marking them `sent` on OUR write is honest (delivered
  to the CLI), but if the user interrupts the first, the CLI drops the siblings and
  they stay `sent` with no reply. One-at-a-time would instead keep them held and
  drop them "not delivered". Accepted for batched mode.

**Write-confirmation (`flushing`).** `on_result` empties `queued_sends` into the
flush step (writes + `sent`) BEFORE `deliver` performs the write. A `WriteFailed`
(child wedged/died right after its result) would drop the `sent` events with the
queue already empty — stranding the ids "queued" forever (`drain_pending` had
nothing to drop). Fix: the flushed uuids are staged in `flushing`, cleared on the
next frame (reaching `on_frame` again means `deliver` returned Ok), and dropped by
`drain_pending`. A drop for an already-`sent` id is a reducer no-op.

**Known narrow edge (not fixed).** Cancelling a HELD message that is another held
message's checkpoint `preceding`, then forking at that successor, resolves the
fork anchor to the cancelled (never-written) uuid — the held-cancel makes its
absence from the native transcript deterministic. Needs ≥2 held sends with no
intervening assistant/user frame, a cancel of the earlier, then a fork at the
later. Low severity; the preceding-chain is not repaired on cancel.

## Pass 14 (2026-07-11 — stop preserves the queue; ✕ dismisses). ADOPTED.

Live testing of Pass 13 surfaced the wrong default: pressing **Stop** with
messages queued dropped the whole held queue "not delivered" — a wall of
un-dismissible red bubbles, and the user's typed messages silently un-sent.
Maintainer decision: **a stop (or a failed turn) ends only the CURRENT turn —
the queued messages still deliver, in full.** Dropping-on-abort was inherited
from the pre-Pass-13 world, where the CLI owned the queue and genuinely
discarded it on interrupt; with the driver holding the messages it is an
unforced choice, and the wrong one. `dropped` now means *genuinely
undeliverable* — the agent process died (teardown) or the flush's write never
shipped — and nothing else.

**claude.** `on_result` flushes the held queue at EVERY turn end: the is_error
branch no longer drops — it emits `TurnAborted` (when a turn was open), then
falls through to the same flush as a completion, so the delivered bubbles land
after the abort marker. The interrupt watchdog does the same on firing
(best-effort write against a possibly wedged child — a failed/timed-out write
tears the driver down and the `flushing` stage drops the batch honestly).
`drain_pending` (process death) is the only remaining `dropped` producer.

**codex.** The live-abort paths (`turn/completed{interrupted}`, `turn/failed`)
no longer call the drop-drain: the first buffered send re-drives as a fresh
turn (the rest steer into it on its `turn/started`), and in-flight steer RPCs
stay tracked — codex is alive on these paths, so a steer that landed before
the abort resolves `sent` off its ack, and one that missed re-drives off its
error answer (the pre-existing `on_response` steer-error arms; the Pass 12
"never resurrect after a stop" special-case is deliberately REVERTED — the
resurrection is now the point). Called after `reset_turn_state`, which would
otherwise clobber the re-drive's `turn_pending` window. The interrupt watchdog
and teardown still drop: the watchdog firing means codex stopped answering, so
a re-driven `turn/start` would just strand "queued" against a wedged process.

**CancelQueued is now the universal pull-back/dismiss.** For a still-held
(claude) / still-buffered (codex) send it removes it and emits `Cancelled`.
For an id that already resolved it emits the same `Cancelled` as a TOMBSTONE:
the reducer removes a `dropped` bubble (the ✕ on a "not delivered" bubble is
this dismiss — replay-stable, since the tombstone is journaled) and no-ops for
an already-`sent` id (seq order guarantees `sent` folds first — the delivered
message can't be un-said). One codex exception: an id whose steer RPC is IN
FLIGHT is mid-delivery (its resolution is still coming), so a tombstone would
vanish a bubble the agent may consume — that one gets a Notice ("on its way").
The UI shows ✕ on every pending bubble, queued or dropped.

Tests: claude `interrupt_marks_abort_user_initiated_and_preserves_queue`,
`interrupt_watchdog_aborts_a_hung_turn_after_the_grace` (flush, not drop),
`bare_result_*` (error flush), `cancel_queued_after_delivery_is_a_reducer_noop_tombstone`;
codex `user_interrupt_preserves_queued_steer_via_redrive`,
`cancel_queued_mid_steer_is_a_notice`, `cancel_queued_after_resolution_is_a_tombstone`;
e2e `interrupt_classifies_user_stop_and_queue_still_delivers`,
`cancel_queued_after_delivery_is_a_reducer_noop_tombstone`; reducer vitest
covers abort→flush ordering and the tombstone dismiss/no-op pair. Wire SHAPE
untouched. `just chat-smoke` re-run for the driver change.

<<<<<<< HEAD
## Pass 16 (2026-07-16 — live probe codex 0.144.2): multi-agent / collab. ADOPTED.

> Pass 15 (claude Workflow-run wire facts) lives on the `rich workflow rows`
> branch (PR #69), still unmerged when this pass landed — the number is
> reserved for it, not skipped by accident.

Codex ships multi-agent ("collab") support; 0.144.2 uses it out of the box
when asked (raw app-server probes, three runs; no config needed). The model's
tools are SpawnAgent/SendInput/ResumeAgent/CloseAgent (binary-mined names) —
but the WIRE surfaces them as items on the parent thread plus whole extra
threads, as follows.

### The connection multiplexes every thread — the one load-bearing fact

A spawned subagent is a REAL THREAD (its own thread id, rollout file, turns),
and its ENTIRE transcript — `turn/started`, `item/*` (reasoning,
agentMessage + deltas, commands…), `thread/tokenUsage/updated`,
`thread/status/changed`, `mcpServer/startupStatus/updated`, `turn/completed`
— streams interleaved on the SAME app-server connection, distinguished only
by `params.threadId` (every `item/*` notification carries `threadId` +
`turnId`). Observed order at spawn: the parent's `subAgentActivity` marker,
THEN the subagent's `turn/started`; at answer: the subagent's final
`agentMessage` + `turn/completed` land BEFORE the parent's `wait` item
completes. A driver that doesn't scope by threadId renders the subagent's
answer as the parent's prose and closes the parent turn on the subagent's
`turn/completed`. The driver now gates every notification on
`params.threadId` (absent or matching = the parent's; anything else feeds
the subagent lane). `serverRequest/resolved` is exempt — JSON-RPC ids are
connection-scoped, and a subagent-thread ask must still withdraw its card.
Server→client REQUESTS (approvals, requestUserInput) also carry the
subagent's threadId but answer by rpc id, so the existing card path already
routes them correctly regardless of thread.

### Parent-thread items

- `subAgentActivity` — the spawn/input/close markers. Arrives as
  `item/completed` ONLY (no started). Shape: `{type, id (the collab CALL id,
  "call_…"), kind, agentThreadId, agentPath}`. `agentPath` is a namespace
  path whose last segment is the model's own name for the agent
  ("/root/agent_a"). Kinds seen live: `started` (spawn), `interacted`
  (send_input — the follow-up then runs as a NEW TURN on the agent's
  thread), `interrupted` (close/shutdown). Binary mining also names
  compaction and a catch-all variant (subAgentThreadSpawn/subAgentActivity/
  subAgentCompact/subAgentOther) — unseen kinds are folded onto the row
  verbatim rather than dropped.
- `collabAgentToolCall` — a collab tool call as an item. Live, only
  `tool:"wait"` surfaces this way (started + completed pair); spawn/input/
  close produce only the activity markers above. Shape: `{type, id
  ("call_…"), tool, status (inProgress|completed), senderThreadId (= the
  parent), receiverThreadIds[], prompt, model, reasoningEffort,
  agentsStates{}}` — the last four were null/empty in every probe (likely
  populated for targeted waits/spawns; shapes unprobed).

### Delegation config

The thread/settings surface calls it `multiAgentMode`; `thread/start`'s
result echoes the default: `explicitRequestOnly`. Full enum, pinned by the
server's own -32600 error text: `none | custom | explicitRequestOnly |
proactive` (camelCase on the wire — "disabled"/"explicit-request-only" are
rejected). `thread/settings/update {threadId, multiAgentMode}` accepts it
live (0.144.2); it also rides `turn/start` per-turn (pass-2 mining). Not
exposed in chimaera yet — the default already delegates when asked.

### Chimaera's mapping (claude-symmetric)

The subagent surface is claude's exactly: an `Agent: {name}` tool row
(`ToolKind::Agent`, row id `agent:{agentThreadId}`, name = last agentPath
segment) that the AgentsTray derives from. `started` opens the row;
foreign-thread frames fold into its progress line ("{last} · N tools · M
tokens" — claude's task_progress format; tokens from the agent thread's
`tokenUsage`, throttled to ≥256-token moves; tools counted from its
completed tool-ish items, label changes emitted on item/started); the agent
thread's `turn/completed` closes the row ("answered" — it sits idle
awaiting follow-ups; a deliberate stop closes quietly, like claude's
stopped verdicts); `interrupted` closes it ("closed"). **A follow-up/resume
to a CLOSED agent opens a NEW row** (`agent:{thread}#N`): the chat UI's
tool-status guard is monotonic by design (a finished row never walks back
to running — it exists to drop straggling subagent updates), so fresh work
gets a fresh card instead of an update the client would rightly drop.
Unseen `subAgentActivity` kinds open-or-note the row (an unmined spawn
variant must not leave a subagent invisible). A subagent's `fileChange`
items also record `item_locations`, so its approval card still names the
touched files; its `error` notifications fold into the progress line.
Subagent transcripts are hidden from the parent's (claude hides
parent_tool_use_id frames the same way). The set lives per parent turn like
claude's task map: an aborted/failed/watchdogged parent turn — and driver
teardown (`drain_pending`), where the subagents die with the process —
first fails still-open rows ("subagent stopped with the turn"); a normal
end just clears the set. Over the 32-row cap, closed rows are evicted
first; when every slot is live the newest agents are NOT tracked (a
synthetic close would lie) and a once-per-turn Notice says so. `wait`
renders as an ordinary tool row ("waiting for subagents"),
upserted-on-completed so an instant call without item/started still lands.
Nested delegation (a subagent spawning its own) shows as that agent's
"delegating" progress — no nested rows. No stop affordance: the collab
tools are the MODEL's, and no client→server RPC to stop a single subagent
is known (`turn/interrupt` on the subagent's thread/turn is unprobed) — the
tray's stop stays claude-only. SubagentStart/SubagentStop HOOK events
(binary-mined) are codex's hook system, not this wire — not consumed.

## Pass 17 (2026-07-16 — post_turn_summary → SessionStatus). ADOPTED.

`system/post_turn_summary {summarizes_uuid, status_category, status_detail,
needs_action}` — the CLI's own post-turn one-liner about where the session
stands. Mapped to the additive `SessionStatus {category, detail,
needs_action}` event, folded LATEST-WINS into `ChatInfo`
(`status_detail`/`status_category`/`status_needs_action`; the flag clears on
`TurnStarted`, the line stays as context) and onto the session rows — the
rail's second line, and `needs_action` lands the `idle_prompt` attention
state on top of TurnCompleted's `finished` (the frame follows the `result`).
`summarizes_uuid` is dropped (nothing keys transcript blocks by uuid).

Wire facts (live 2026-07-16):

- **`needs_action` is a STRING, not a bool** — captured frames carry
  `"needs_action": ""` (empty = nothing needed). The driver maps truthiness
  (non-empty string, or a literal bool `true` if the CLI ever switches);
  non-empty value semantics beyond truthiness are unprobed.
- **Emission is CONDITIONAL and version-unstable.** On 2.1.207 it fired
  after workflow-lifecycle turns (launch: "workflow 'probe' launched, 2
  agents spawning"; completion: "workflow completed: …", both
  `status_category: "review_ready"`). On 2.1.211 it was NOT emitted for
  ANY probed turn — bare echo, Write-tool, background-Bash, or a full
  Workflow launch+completion (driven live through the daemon). Treat the
  frame as opportunistic: the mapping is additive and dormant when absent;
  nothing may depend on it arriving.
- The vsix routes the subtype but never consumes it — no client-side
  capability gates it, so its 2.1.211 silence looks like a server/CLI-side
  gate, not a missing initialize flag.

Tests: hermetic `post_turn_summary_folds_latest_wins_session_status`
(fake-claude emits the summary after each successful result — empty then
non-empty `needs_action`, distinct details for the latest-wins assert; the
TurnStarted clear is asserted too). The live echo test listens 5s
post-result and pins the shape IF a frame shows up — its absence is the
expected outcome on 2.1.211 and is printed, not failed. Full
`just chat-smoke` re-run against 2.1.211 (16/16) and the pin bumped
2.1.207 → 2.1.211.
