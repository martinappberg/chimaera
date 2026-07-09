# Agent protocol facts (live-verified + extension-mined)

The wire formats this crate speaks are unversioned. This file records what we
KNOW, how we know it, and what we have not adopted yet. Re-verify with
`just chat-smoke` whenever a CLI updates; pins live in `claude.rs` / `codex.rs`
(`TESTED_*_VERSION`).

Sources:
- **live**: probed against the real CLIs (claude 2.1.204, codex 0.142.5).
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
  `needs_action`, `status_detail`, `summarizes_uuid`) — unmapped; the
  extension ignores it too.
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
> now mirrors this. UNVERIFIED: `on_result` zeroing `queued_sends` on any
> `is_error` result assumes the CLI drops its native stdin queue with the
> aborted turn; the driver now also defensively opens an implicit turn if a
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
`system/task_notification` (remove). The official client HIDES
parent_tool_use_id-tagged transcript frames — the visible surface is the
Task tool row ("Agent: {description}"). No client-side message queue exists:
mid-turn user frames go straight to stdin (the CLI queues);
`{"subtype":"cancel_async_message","message_uuid"}` un-queues. Slash sends
bypass queueing.

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
  `cancel_async_message {message_uuid}` exists in the SDK but the extension
  never calls it; ours doesn't either.
- Subagents: `task_started {task_id, task_type ("local_agent" only),
  description, prompt}`, `task_progress {task_id, description?,
  last_tool_name?, summary?, usage:{total_tokens, tool_uses, duration_ms}}`,
  `task_notification {task_id}` = removal. task_id is an OPAQUE key with no
  relation to the Task tool_use_id — chimaera correlates by description and
  synthesizes an "Agent:" row when no Task card matches. Maps wipe on
  `result`.
- Permission destinations: rule/suggestion field is `destination`
  (localSettings|userSettings|projectSettings|session|cliArg); cycler order
  is that list minus cliArg; the chosen destination re-stamps every
  suggestion EXCEPT setMode (which keeps its own). Labels: "this project
  (just you)" / "all projects" / "this project (shared)" / "this session".
- Thinking: extension-persisted state, not read back from the CLI; spawn
  flags are `--max-thinking-tokens 31999 [--thinking-display summarized]` or
  `--thinking disabled`; mid-session = `set_max_thinking_tokens`.
- `post_turn_summary` is UNUSED by the extension (routed, never consumed) —
  not mapped here either.

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
