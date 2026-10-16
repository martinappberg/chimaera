# ACP v1: Grok Build, Google Antigravity and Cursor CLI

The current contract of the shared ACP adapter (`src/acp.rs`). The pins are `TESTED_GROK_VERSION`, `TESTED_AGY_VERSION` and `TESTED_CURSOR_VERSION`; the live gate is `just chat-smoke-acp`. Each fact names the pass or date and the agent version where it was recorded, for example "(Pass 45, grok 1.0.46)". A fact tagged "(2026-10-01)" alone was recorded at Grok Build 1.0.46 and agy-acp 1.2.1, the 2026-10-01 live probe. The dated record is [passes.md](passes.md); the shared rules are in [cross-agent.md](cross-agent.md).

## Runtimes and launch

- Grok Build **1.0.46 (2765805b9442)** runs as `grok agent --no-leader stdio` (2026-10-01).
- Google's ACP server is **agy-acp-server 1.2.1** (`agy_acp_server.par` beside `localharness_external`). The standalone `agy` CLI is a different executable with its own version (1.1.1 probed first; the managed installer later installed 1.2.14; 1.3.4 by Pass 46). Its headless stream-json interface rejects interactive control requests; chat uses Google's official ACP package (2026-10-01).
- The server must resolve managed symlinks before launch so sibling tools stay visible. Launched through the managed `agy-acp` symlink alone, `session/new` fails `-32603 "Could not find default localharness binary"`; the daemon's launcher resolves it (2026-10-01; Pass 45).
- Cursor CLI **2026.10.14-941492e** (`cursor-agent`, also linked as `agent` by the official installer) has a hidden `acp` subcommand, absent from the top-level `--help` and described by `cursor-agent acp --help` as "Start the Cursor Agent as an ACP (Agent Client Protocol) server". Chimaera uses it rather than the `-p --output-format stream-json` print mode, which runs one prompt per process with no permission round trip (`-f` / `--yolo` or an allow-list decide), no mid-turn cancellation and no session controls (2026-10-14, signed-out probe).
- The daemon sets `NO_OPEN_BROWSER=1` on every Cursor chat spawn (2026-10-14).
- Cold ACP startup can exceed 20 s while the harness and configured MCP servers start; server spawns allow 60 s (2026-10-01). See [cross-agent.md#startup-lifecycle](cross-agent.md#startup-lifecycle).
- Gemini CLI is out of the catalog (0.64.0 installed; its session records stay readable only) (Pass 43).

## Handshake and authentication

- All three speak JSON-RPC over newline-delimited stdio and negotiate `protocolVersion: 1` (2026-10-01; 2026-10-14).
- Grok and Antigravity advertise `loadSession`, `sessionCapabilities.resume`, HTTP MCP servers, and stream `session/update` (2026-10-01).
- Cursor's `initialize` → `agentCapabilities` = `loadSession: true`, `mcpCapabilities {http, sse}`, `promptCapabilities {image: true, audio: false, embeddedContext: false}`, `sessionCapabilities {list: {}}`. There is no `resume`, so a reopened chat uses `session/load`; its history replay arrives during the handshake and is discarded there (Chimaera's journal owns the transcript). One auth method: `cursor_login` ("Authenticate using existing Cursor login credentials. Run 'agent login' first if not logged in.") (2026-10-14).
- Grok advertises `grok.com` auth but accepts `cached_token` with `_meta.headless: true` for an existing login. Google's `oauth-personal` reuses the existing personal login; other Google methods are advertised but not selected by the adapter. Auth is never copied into Chimaera state (2026-10-01).
- Cursor's `authenticate {methodId: "cursor_login"}` while signed out does not fail fast: it starts Cursor's browser login. With `NO_OPEN_BROWSER=1` it answers `-32602 Invalid params` with `data.message` "Failed to open browser for login. Please visit: https://cursor.com/loginDeepControl?challenge=…", a one-time challenge owned by that process. The adapter's `sign_in` hint replaces the error: "Cursor CLI is not signed in on this host. Run `cursor-agent login` in a terminal, then start the chat again." `session/new` without authentication → `-32000 Authentication required`. A daemon must never open that login itself (2026-10-14).
- The adapter reports `startup_progress` for authentication and session opening before `Init` (2026-10-01).
- A long-lived `grok agent stdio` process keeps the plan tier it authenticated with: after an account upgrade its turns kept failing `subscription:free-usage-exhausted` until the chat's process restarted (2026-10-05, grok 1.0.46).

## Sessions, catalogs and controls

- `session/new {cwd, mcpServers}` returns the native id and the account model catalog. Grok's catalog after authentication included `grok-4.7`; Google's included Gemini 3.8 Flash effort-specific model ids. Do not hardcode these lists (2026-10-01).
- `configOptions` supplies select controls by category (`model`, `mode`, `thought_level`); `session/set_config_option` returns the authoritative current values. `config_option_update` can replace controls; absence removes an effort control (2026-10-01).
- Image input follows the negotiated capability: Grok advertised false, Google and Cursor true. Buttons and attachment admission follow it (2026-10-01; 2026-10-14).
- ACP stays catalog-only: no custom model input. `chat.newSessionModel=agent` omits remembered model and effort through the shared server path; explicit and saved conversation settings still take precedence (2026-10-04, agy-acp 1.2.1 and grok 1.0.46).
- The modes and models a Cursor session offers come only from its own `session/new` result and `configOptions`; nothing is manufactured (`cursor_offers_only_what_its_session_advertises`). Cursor's documentation names modes `agent`, `plan`, `ask` (pending a signed-in run) (2026-10-14).
- `session/resume` restores provider history without replaying it; Chimaera's journal restores the visible transcript. `session/load` is the compatibility fallback (2026-10-01).

## Grok modes

Recorded at grok 1.0.46 (probed 2026-10-04/05, with model turns on the second day).

- Grok advertises no modes: `session/new` has no `modes` and no `mode` config option (its `configOptions` are `model` and `reasoning_effort`).
- Always-approve: a `session/prompt` whose text is `/always-approve on` (or `off`) runs inside Grok and returns `end_turn` with `_meta.totalTokens: 0` and no message chunks. Measured with one shell command: default asked (`session/request_permission`), after `on` it ran unasked, after `off` it asked again. Nothing reads the state back mid-session (`_x.ai/sessions/changed` kept `yolo: false`), so the zero-token answer is the only confirmation. A slash Grok does not know (`/auto on` on an account without Auto-review) goes to the model and is billed. The adapter offers Normal / Always-approve only while `available_commands_update` lists `always-approve`, sends the switch between turns (it occupies the session like a turn: sends queue behind it, and it waits for a running turn), and treats any answer that billed tokens as "not switched". A handshake restore is not a pick (`restoring`). `_meta.yoloMode: true` on `session/new` starts a session in it (`yolo: true` is reported then).
- Plan: `session/set_mode` answers `{}` for any `modeId`; only `plan` and `default` do anything, each confirmed by a `current_mode_update`. In plan mode Grok writes `plan.md` in its session directory, calls `exit_plan_mode`, and sends the client a JSON-RPC request `_x.ai/exit_plan_mode {sessionId, toolCallId, planContent}`, then often `_x.ai/ask_user_question`. The reply shape for the approval is unknown: an error cancels the turn, and `{}` or every guessed field (`approved`, `approve`, `accepted`, `outcome`, `decision`, `action`, …) reads as "The user wants to revise the plan". The adapter does not offer Plan until that shape is captured from the TUI.
- Auto-review is gated per account (`_x.ai/settings/update` `auto_permission_mode_enabled`, null on the probed account; `/auto` is then absent from the command list).

## Turns, queue and cancellation

- A `session/prompt` response ends the turn (`end_turn` or `cancelled`) (2026-10-01).
- A queued prompt starts after the preceding prompt response, not after the cancel notification is written (2026-10-01).
- Cancellation resolves open permission requests as cancelled before sending `session/cancel` (2026-10-01).
- ACP v1 has no mid-turn input: `session/prompt` is one at a time. A message to a busy ACP chat waits for the turn to end and then meets the wake policy (Pass 44).
- No native fork is advertised. A conversation copy is held in the mapper until the first real user message and prepended to that prompt; opening a fork must not synthesize a turn. The handoff stays pending until the provider emits response content or tool activity, or completes the first prompt; then the driver journals `ForkContextConsumed`. A JSON-RPC rejection before acceptance keeps the copy for retry. An undelivered copy is recovered from its journal marker after a restart or reopen (2026-10-01).

## Permissions

- Permission requests are agent-initiated JSON-RPC requests (`session/request_permission`). Replies preserve the original numeric or string id and choose only an offered `optionId` (2026-10-01).
- ACP permission replies contain only the selected option id; there is no feedback field. Grok's TUI-derived reject label mentions feedback, but chimaera labels that action "Deny" and shows no feedback or rule-destination controls (2026-10-01).
- Permission requests stay native; no consent is inferred from a tool's display title (2026-10-01).
- Option sets seen (Pass 45): Antigravity `allow_always` / `allow` (`allow_once`) / `deny` (`reject_once`); Grok `always-allow` / `allow-once` / `reject-once`; Cursor `allow-once` / `allow-always` / `reject-once` (Cursor's docs list the same three).

## MCP tool calls and pre-approval

All three honor the stdin `mcpServers` entry the daemon sends: `{type: "http", name: "chimaera", url, headers: [{name: "Authorization", value: "Bearer …"}]}`. MCP credentials go over stdin in HTTP headers, never in process arguments. Cursor sent `Authorization: Bearer …` with `clientInfo {name: "Cursor", version: "1.0.0"}` and MCP `protocolVersion: "2025-11-25"`; its docs name `.cursor/mcp.json` as the source (team dashboard servers are not loaded over ACP), and the session's list works too (Pass 45; 2026-10-14).

Every agent names the server and the tool, each in its own place; the title alone is never enough, because the model's tool name is interpolated into it verbatim (Pass 45).

- Antigravity (agy-acp 1.2.1): structured `_meta`; the title is `<server>_<tool>`; the request's `toolCall` equals the preceding `tool_call` update.

```json
{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"sessionId":"ee4a…","toolCall":{"toolCallId":"cab0087fdc5748f3b428d0cc480cd5a7","kind":"other","status":"pending","title":"chimaera_notify","content":[],"rawInput":{"arguments":{"text":"hello from probe"},"text":"hello from probe"},"_meta":{"mcp":{"tool":"notify","server":"chimaera"},"is_mcp_tool_call":true}},"options":[{"optionId":"allow_always","name":"Allow Always","kind":"allow_always"},{"optionId":"allow","name":"Allow","kind":"allow_once"},{"optionId":"deny","name":"Deny","kind":"reject_once"}]}}
```

- Grok Build 1.0.46: an MCP call is its `use_tool` tool: `rawInput {variant: "UseTool", tool_name: "<server>__<tool>", tool_input}` under `_meta["x.ai/tool"].kind == "use_tool"`, the title repeating `tool_name`. The preceding `tool_call` update has `title: "use_tool"` and the unprefixed `rawInput`; a `tool_call_update` renames it; the completed update's `rawOutput` carries `{type: "MCP", tool_name, server_name}`.

```json
{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"sessionId":"01a1…","toolCall":{"toolCallId":"call-a49db86c-3bb4-445f-887b-27c3370ef139-1","kind":"other","title":"chimaera__notify","rawInput":{"variant":"UseTool","tool_name":"chimaera__notify","tool_input":{"text":"hello from probe"}},"_meta":{"x.ai/tool":{"version":1,"name":"use_tool","kind":"use_tool","namespace":"grok_build","label":"Use Tool","read_only":false}}},"options":[{"optionId":"always-allow","name":"always allow","kind":"allow_always"},{"optionId":"allow-once","name":"allow once","kind":"allow_once"},{"optionId":"reject-once","name":"reject once","kind":"reject_once"}]}}
```

- Cursor CLI 2026.10.14: the request's `toolCall` has no `rawInput` (only a `content` text block with the arguments as a JSON fence), and its title is `<server>-<tool>: <tool>`. The server and tool are named by the preceding `tool_call_update` with the same `toolCallId` (which contains a newline). The row opens as `tool_call {title: "MCP: tool", rawInput: {}}`, then:

```json
{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"e787…","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-dff99f2e-b536-4ea5-82b2-409a16e88c4c-1\nfc_77bc5181-0275-9386-833a-574b551b67aa_0","title":"chimaera: notify","rawInput":{"providerIdentifier":"chimaera","toolName":"notify","args":{"text":"hello from probe"}}}}}
{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"sessionId":"e787…","toolCall":{"toolCallId":"call-dff99f2e-b536-4ea5-82b2-409a16e88c4c-1\nfc_77bc5181-0275-9386-833a-574b551b67aa_0","title":"chimaera-notify: notify","kind":"other","status":"pending","content":[{"type":"content","content":{"type":"text","text":"```json\n{\n  \"text\": \"hello from probe\"\n}\n```"}}]},"options":[{"optionId":"allow-once","name":"Allow once","kind":"allow_once"},{"optionId":"allow-always","name":"Allow always","kind":"allow_always"},{"optionId":"reject-once","name":"Reject","kind":"reject_once"}]}}
```

- Completed updates: Cursor `{"status":"completed","rawOutput":{"success":true}}`; Grok `{"status":"completed","rawOutput":{"type":"MCP","tool_name":"notify","server_name":"chimaera","output":{"OkayOutput":"ok"}}}`; Antigravity carries the tool text as `content` and `rawOutput`.
- Pre-approval: `SpawnSpec.mcp_auto_approve` reaches the adapter. The server sets the worker consent for every ACP chat (`mcp::ALWAYS_ALLOWED_TOOLS` + the workspace's `plugins::spawn_allow`, never a Mastermind tier). The adapter answers a matching `session/request_permission` with the offered `allow_once` option (never `allow_always`, a standing grant the agent would keep) and emits no `PermissionRequest`. Scope: one server; the tool only in the pinned shape (agy's `_meta.mcp` + title `<server>_<tool>`; Grok's `UseTool` + `use_tool` meta + title = `tool_name` split at `__`; Cursor's remembered `tool_call_update {providerIdentifier, toolName}` + title `<server>-<tool>: <tool>`), with a `[A-Za-z0-9_-]` tool charset. Whole-server consent (`tools: None`, no ACP caller today) needs the structured server alone. A consent with a server match but no pinned tool tracing-warns and surfaces the ask. A shell command, a file edit, another server, a title that disagrees with the structured fields, or a request with no `allow_once` option all surface as before. Cursor's remembered names are forgotten when the call ends and at turn end (Pass 45).
- A renaming `tool_call_update` retitles the row: Cursor opens every MCP call as `tool_call {title: "MCP: tool", rawInput: {}}` and Grok as `title: "use_tool"` / `"search_tool"`; each names the call in a later update. The adapter keeps each open row's title, kind and locations (≤ 64, dropped when the call ends and at turn end) and, when an update carries a different one, re-emits the `ToolCall` under the same id, in progress. Clients upsert a re-emitted call by id. Titles stay `small()`-capped (Pass 45).
- Terminal MCP injection is Claude/Codex-only. Grok 1.0.46 rejected the `--plugin-dir` option its docs advertise, and no verified session-only override was found for the Antigravity terminal CLI. Chat uses ACP's `mcpServers` parameter without rewriting global settings; do not infer terminal parity from chat tests (2026-10-01).

## Plans and to-do lists

- Cursor's `TodoWrite` reaches the client as `cursor/update_todos`, a JSON-RPC request (it has an `id`) on 2026.10.14, while Cursor's ACP page calls it a notification and still documents a response type. Captured, three updates in one turn (Pass 45):

```json
{"jsonrpc":"2.0","id":0,"method":"cursor/update_todos","params":{"toolCallId":"call-e7813dbd-f14c-4d36-9bc1-581803d671d3-1\nfc_20676f0e-24ad-9ed8-8c29-cde7d2c8d719_0","todos":[{"id":"inspect","content":"inspect","status":"pending"},{"id":"draft","content":"draft","status":"pending"},{"id":"verify","content":"verify","status":"pending"}],"merge":false}}
{"jsonrpc":"2.0","id":1,"method":"cursor/update_todos","params":{"toolCallId":"call-0904…","todos":[{"id":"inspect","content":"inspect","status":"in_progress"},{"id":"draft","content":"draft","status":"pending"},{"id":"verify","content":"verify","status":"pending"}],"merge":true}}
```

- `status` is `pending | in_progress | completed | cancelled`; `merge: false` on the first write, `true` after, each time with the whole list (Cursor merges by `id`). Each is preceded by the agent's tool row `tool_call {title: "Update TODOs", rawInput: {_toolName: "updateTodos"}}` and `tool_call_update`s whose `rawInput.todos` carry Cursor's internal statuses (`TODO_STATUS_IN_PROGRESS`, `createdAt`, `dependencies`); the request is the documented carrier.
- Reply: the documented `{"outcome":{"outcome":"accepted","todos":[{id, content, status}…]}}` was answered live and the turn went on; so did a `-32601` (Cursor keeps its list either way). The adapter answers the accepted outcome with the list as it now holds it, only when the frame has an id. The list becomes a `Plan` level-set with `id`s (merged by id; a cancelled item leaves the card; ≤ 64), journaled like every other plan.
- Grok's `todo_write` tool (`_meta["x.ai/tool"] {name: "todo_write", kind: "plan"}`, `rawInput {todos, merge}`) is followed by the ACP `plan` update (`entries: [{content, priority, status}]`), which the adapter maps (Pass 45).
- Antigravity has no plan tool ("I do not have a built-in plan, task, or todo tool."); nothing reaches the card (Pass 45).
- The agent's own plan rows (Cursor's `updateTodos`, Grok's `kind: "plan"` tool) draw no transcript row, like claude's TodoWrite. The MCP plan paragraph (`mcp.rs` `task_list_instructions`) says what each ACP agent has (Pass 45).

## Cursor extension methods

- `cursor/task` and `cursor/generate_image` are notifications and are ignored (2026-10-14, from Cursor's docs).
- `cursor/ask_question` and `cursor/create_plan` are blocking requests. The adapter refuses them with `-32601` like any unsupported client method and journals a Notice pointing at the terminal view, because their reply shapes are documented but unverified (2026-10-14).
- `cursor/update_todos`: see [Plans and to-do lists](#plans-and-to-do-lists).

## Usage and context

Recorded in the Pass 46 addendum (2026-10-15).

- ACP `usage_update` (`{used, size, cost?}`, stable since schema 1.25.0; `PromptResponse.usage` is still unstable) maps generically to `ContextUsage`. None of the three agents sends it yet.
- Grok (ACP extensions, not in the spec): `_x.ai/session/info {sessionId}` → `result.result.context {used, total, usagePct, …}` (nested once more; answers before the first turn too). `_x.ai/billing {}` → `{config: {creditUsagePercent 0-100, currentPeriod: {type: "USAGE_PERIOD_TYPE_WEEKLY", start, end}, onDemandCap, onDemandUsed, prepaidBalance, …}, subscription_tier}`, answered 0.5 s after `initialize` with no session needed. After each completed Grok turn the adapter asks both: context → `ContextUsage`; billing → a deduped `RateLimit` with one window (`acp::grok_billing_usage`). A refusal is silent. The daemon's Agent-settings read runs `grok agent --no-leader stdio` → `initialize` → `_x.ai/billing`.
- Antigravity: the ACP stream reports neither context nor usage (the prompt result is `{stopReason}` only). `agy -p "/usage" --output-format json` (changelog 1.1.11: answers "without starting an agent turn, spending quota"; live `num_turns: 0`, 4 s) → `{status: "SUCCESS", command: {name: "usage", data: {groups: [{name: "Gemini Models" | "Claude and GPT models", buckets: [{id, name, window: "weekly" | "5h", remaining_fraction 0-1, reset_time ISO}]}]}}}`. An untouched bucket's `reset_time` is "now + window". Used only for the Agent-settings read ("Gemini session limit"; no reset time while `remaining_fraction` is 1).
- Cursor: nothing a host can read (no `usage_update`, no `_meta` usage, no usage subcommand; its TUI's usage panel calls Cursor's own servers). Nothing is shown.

## What ACP lacks

- No subagent surface: no ACP agent reports subagents on its wire, so the subagent model chip and the subagent view are claude- and codex-only (Pass 42).
- No mid-turn input (Pass 44).
- No system-context field: a portable branch's copy rides the first real prompt (2026-10-01).
- No feedback on deny (2026-10-01).

## Native extension discovery

Separate from ACP; the daemon probes these (2026-10-01).

- `grok inspect --json` includes Claude-compatible inventory that `plugin list` / `mcp list` omit. Skill plugin identity is `source.plugin_name`; bundled skills have `source.type: "bundled"`. Grok's `/plugins` and `/mcps` open its native extensions menu. Grok local-source installation requires explicit `--trust`; Chimaera asks in the visible install terminal before setting that flag.
- `agy -p /skills --output-format json --print-timeout 15s` returns `command.name: "skills"`, `num_turns: 0`, `status: "SUCCESS"` and `command.data.skills`. Probe only known supported versions. AGY `plugin list` imports have no enablement state and omit project-only packages. A loaded skill is positive evidence for its own scope, not for another copy with the same name. In agy 1.2.14 `/plugins` is a model prompt, not a management menu; use the native plugin CLI commands.
- Live (2026-10-01): Antigravity CLI 1.2.14 and Grok 1.0.46 ran through Chat → ACP → the chimaera MCP server: both listed workspace peers, sent a message and read the reply, held a message while busy, reached the Ask wake policy on turn completion, and consumed the message once after the user-approved wake. Antigravity's `/chimaera-fixture:probe` and Grok's loaded `probe` skill returned the disposable plugin's verification word.

## Live verification

- `just chat-smoke-acp` covers, for each registration: copied-context recall, ordered streaming, replay, same-id resume, model read-back, allow/deny, stop while awaiting permission, queue cancellation, and delivery after the active turn. Set `CHIMAERA_TEST_AGY_ACP` (the resolved `.par`), `CHIMAERA_TEST_GROK` and `CHIMAERA_TEST_CURSOR` to official executables; the tests reuse existing logins, create files only in temporary workspaces, and bill tiny turns (`cargo test -p chimaera-agent --test acp_live -- --ignored --test-threads=1`) (2026-10-01; Pass 45).
- Antigravity ACP 1.2.1 and Grok 1.0.46 passed stream/replay/resume, model selection, approval allow/deny/stop and queued-message controls (2026-10-04). The four-adapter daemon fixture verifies both new-session model policies without replacing existing chats' settings.
- Cursor: `TESTED_CURSOR_VERSION` first pinned the build whose handshake was probed signed out, not a verified turn wire (2026-10-14). The Pass 45 frames above are from signed-in live turns on that build; the turn-level gate is `CHIMAERA_TEST_CURSOR=$(command -v cursor-agent) cargo test -p chimaera-agent --test acp_live cursor -- --ignored --nocapture`.
- Sources: [Google ACP distribution](https://antigravity.google/docs/ide/extensions), [Google headless CLI](https://www.antigravity.google/docs/cli/headless/), [Grok headless interfaces](https://docs.x.ai/build/cli/headless-scripting), [Cursor ACP](https://cursor.com/docs/cli/acp).
