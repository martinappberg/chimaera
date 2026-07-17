# Structured chat mode (Tier B)

The rich chat surface: instead of running an agent as a raw TUI, Chimaera drives the same
CLI over its **structured JSON protocol** (Claude Code over bidirectional `stream-json`,
Codex over `codex app-server` JSON-RPC) and renders a first-class chat UI — streamed prose
and thinking, tool cards, permission and question prompts, inline artifacts, model/effort
controls, and lossless reconnect. The same session identity can toggle between chat and the
TUI (see [view switch](#view-switch-and-rewind)).

**Where it lives (shared):** UI `web-ui/src/lib/chat/` (`ChatView.svelte`, `Composer.svelte`,
`ChatHeader.svelte`, `Markdown.svelte`, `ToolGroup`/`ToolCallCard`, `PermissionCard`,
`QuestionCard`, `RewindDialog`, `McpPanel`, `UsagePanel`, `store.svelte.ts`, `chatWs.ts`,
`paths.ts`). Engine `crates/chimaera-agent/src/` (`driver.rs`, `claude.rs`, `codex.rs`,
`model.rs`, `journal.rs`). Daemon glue `crates/chimaera-server/src/chat.rs`, WS
`ws.rs::chat_ws`. Wire: `GET /ws/chat/{id}` (events + the 17 `AgentCommand`s),
`POST /api/v1/sessions/{id}/view`, `POST /api/v1/sessions/{id}/rewind`. Deep protocol facts:
[PROTOCOL.md](../../crates/chimaera-agent/PROTOCOL.md); rules:
[rules/agent-protocol.md](../../.claude/rules/agent-protocol.md); working skill:
[chat-mode](../../.claude/skills/chat-mode/SKILL.md).

## Composing & sending

- **Send / type-through.** Type, Enter to send (Shift+Enter = newline). Sends work mid-turn —
  the placeholder becomes "type through — the agent hears you mid-run"; claude queues it
  natively, codex steers the running turn. `socket.send` returns `false` when the socket isn't
  OPEN, so the draft is only cleared on an accepted send — a message during a reconnect window is
  preserved, not lost (no client-side queue; reconnect replays the gap).
- **Delivery honesty + pending stack.** A mid-turn send doesn't drop into history — it waits in a
  **pending stack pinned just above the composer** (the send point), faded, with a small "queued"
  mark. When the agent actually consumes it (claude dequeues one per finished turn, codex on the
  steer ack) the block leaves the stack and enters the transcript proper as the newest user turn,
  solid. If the turn aborts first (claude drops its native queue on interrupt; a codex steer fails
  for good) the message stays in the stack marked **"not delivered"** (text kept readable/copyable,
  never auto-dumped back into a draft you may have started). Driven entirely off the single `blocks`
  reducer — a derived `store.pendingUserBlocks` splits queued/dropped out of the inline render — and
  journaled via `user_message` `id`/`queued` + `user_message_update`, so replay shows the same truth
  (see PROTOCOL.md pass 8).
- **Image paste.** Paste an image → a removable chip; sent as base64 blocks. Downscaled to
  1568px max dim, 2 MiB post-encode cap (oversized silently dropped); the journal stores a
  placeholder, never the bytes.
- **Autocomplete.** `/` → slash-command popover (native chimaera pickers first, then the CLI's
  own commands); `@name` → fuzzy file/dir quick-open; `@term:` → workspace-terminal grants (see
  [linked-terminals.md](linked-terminals.md)). `/rename <name>` pins a session name. `/compact`
  is native for codex (`thread/compact/start`; the compaction runs as its own turn and lands a
  "context compacted" notice) — claude's `/compact` rides its own CLI catalog as prompt text.
  The slash popover triggers for a **line-leading** `/command` anywhere in the draft (a follow-up
  begun on a fresh line), not only when the slash is the first character — a mid-draft pick
  completes the token in place; only a whole-draft slash takes the command path. Ordinary path
  text ("cd /usr") is never hijacked.
- **`/login` recovery (claude).** An expired-auth chat session dead-ends — the `-p stream-json`
  CLI answers "/login isn't available in this environment". `/login` (palette + intercepted on
  send) instead flips the session to its real TUI (the [view switch](#view-switch-and-rewind)),
  where claude's own `/login` runs the native auth flow (OAuth / setup-token / SSO); chimaera
  never touches the credentials. Sign in there, toggle back to chat.
- **Where.** `Composer.svelte`, `ChatView.svelte` (`sendNow`, `onSlash`, `composerCommands`),
  `composerBus.ts` (other surfaces drop references into the draft). Uses `fsValidate`/`fsQuickOpen`.

## Header controls

- **Model / effort / mode / thinking / ultracode pickers.** Click a header chip (or `/model`,
  `/effort`, `/mode`) to switch. The agent's *live* catalog (claude `initialize.models` / codex
  `model/list`) beats the daemon's curated list. Effort is per-model (codex falls back to the
  `minimal…xhigh` ladder; **`xhigh` is never relabeled** — canonical vocabulary is sacred).
  Thinking (claude) and ultracode (claude, gated to an xhigh-capable model) are client-held toggles
  reconciled from `effort_state` read-backs. **Thinking defaults ON** for claude sessions (the
  reasoning pass earns its keep in a coding workbench; the chip shows it explicitly and one click
  turns it off) — the preference lives in the pooled store (`null` = unchosen ⇒ default on; a bool is
  an explicit choice) and is pushed to the live driver once per driver process, re-synced on each
  respawn (a fresh CLI defaults thinking off) but never re-forced, so a tab remount can't reset it and
  a toggle-off always sticks.
- **Live telemetry chips.** A rate-limit chip appears at ≥80% / reached; a context chip shows "42%
  ctx". Subscription usage (`/usage`, `/cost`) shows **percentages, never dollars**.
- **Where.** `ChatHeader.svelte`, `EffortPopover.svelte`, `UsagePanel.svelte`; store fields fed by
  `model_switched`/`effort_state`/`mode_changed`/`rate_limit`/`context_usage` events. Commands
  `set_model`/`set_effort`/`set_mode`/`set_thinking`/`set_ultracode`/`get_usage`.

## The transcript

- **Content.** User bubbles (right), agent prose (left, markdown), collapsible "thinking · N chars"
  blocks, per-turn duration rulers, plan/todo panel, tool cards, permission/question cards, inline
  artifacts. A live activity row ("thinking · ~1.2k tokens / writing / <tool>") pulses while running.
- **Stopping.** Esc (or the header stop chip) interrupts the running turn. A deliberate stop is
  not a failure: the abort event carries a structural `interrupted` flag set by the driver that
  issued the interrupt (claude's result string is free text and never said so), the transcript
  shows a muted "stopped" notice, and the session rail reads finished — error-red `turn failed`
  and the Errored rail state are reserved for genuine failures.
- **Rendering.** Streamed prose reveals word-by-word on a ~75ms ticker (respects
  `prefers-reduced-motion`). Markdown goes through **marked → DOMPurify**: `<style>` tag and `style`
  attribute are forbidden (injected CSS can't restyle the workbench to spoof a permission prompt),
  every external anchor is forced to `target="_blank" rel="noopener noreferrer"`, and bad local
  anchors are neutralized on click. Client-side transcript cap of 2000 blocks (oldest dropped behind
  one "earlier history trimmed" notice; the live tail is never touched).
- **Where.** `ChatView.svelte` (`renderItems`), `store.svelte.ts` (the `blocks` reducer),
  `Markdown.svelte`, `UserText.svelte`.
- **Untrusted output.** Everything the model emits is attacker-influenced — see
  [rules/web-ui.md](../../.claude/rules/web-ui.md). Tool-card bodies/diffs render as plain `<pre>`
  (no `{@html}`).

## Tool cards, permissions & questions

- **Tool cards + grouping.** Each tool call is a collapsible card (title, glyph, status dot,
  output/diff, a `↗` to open the touched file). Consecutive calls condense into a group ("6 commands
  · 2 files"); groups auto-collapse once every tool finished cleanly, stay open while anything runs
  or failed. Tool calls upsert by id (a late enriching re-emit never walks a finished tool back to
  pending); `tool_output_delta` streams live output ahead of the authoritative result. **Dangling
  rows reconcile at turn end:** on `turn_completed`/`turn_aborted`/`exited` any tool still
  in_progress/pending in the just-ended turn is closed to `completed` (a pure reducer scan back to
  the previous turn boundary). This kills the phantom "running…" a dropped result frame would leave —
  most often a large image `Read` whose `tool_result` exceeds the transport's per-line byte cap and
  is skipped below the event layer, so its completion never arrives and the group never collapses.
- **Background / stop a running row (claude).** A running tool row offers a ⤓ "continue in the
  background" affordance (`background_tool` → the CLI's `background_tasks`, Ctrl-B parity; a
  refusal lands an honest notice), and a running **Agent** (subagent) row offers a ■ stop
  (`stop_task`; the driver resolves the row id to the CLI's opaque task key). Codex has no
  equivalents — the buttons are omitted there.
- **Background-work tray (claude).** Backgrounded Bash commands and workflows (`run_in_background`,
  Ctrl-B parity, `/workflows`) get their own pinned monitor strip, sibling to the subagents tray:
  collapsed to a one-line "N background tasks running" count, expandable to each task's description,
  live elapsed (anchored on a driver-journaled start stamp, so replay shows honest ages), status,
  and a ■ stop. Background work is **cross-turn** — the tray persists after the turn that started it
  ends, and dies with the CLI process (cleared on `init`/`exited`). When a task settles, its verdict
  folds into the transcript as a quiet notice ("background “…” completed — … (exit code 0)"; failed
  renders as an error). The driver ingests the CLI's task lanes — `task_started` (non-`local_agent`)
  + `task_updated` (patch) + `background_tasks_changed` (the authoritative REPLACE-the-set signal) +
  `task_notification` (the only frame carrying the verdict summary) — into one level-set
  `background_tasks` event; tasks the set-change removes before their verdict park in a bounded
  departed buffer so the notification arriving ~ms later still closes them (the live-verified settle
  order). The ■ stop sends `stop_task` with the native task key — the CLI's stop is generic over its
  task registry and acks a raced not-found as success. Codex has no background lane — the tray never
  renders there.
- **Live subagents tray + active plan step.** Subagents running *right now* are promoted out of the
  (collapsed) tool groups into a live monitor pinned just above the composer, so parallel work stays
  glanceable instead of scrolling away — collapsed by default to a one-line "N subagents working"
  summary, expandable to each agent's progress line (tools · tokens, from `task_progress`) + stop.
  They keep their in-place "Agent:" row in history; a finished/abandoned run drops from the tray
  (reconciled shut at turn end). The plan/todo panel likewise surfaces the current step in its
  summary ("plan · 1/3 · ◐ …"). Both are pure derivations over `blocks`/`plan` — no new events
  (`AgentsTray.svelte`).
- **Codex subagents (collab / multi-agent), same surface.** Codex 0.144.x delegation renders the
  identical way: each spawned agent is an "Agent: {name}" row (name = the model's own
  `agentPath` name) whose progress line folds the agent's live activity (thinking · a command ·
  N tools · M tokens), so the tray works unchanged. A subagent is a real thread multiplexed onto
  the same connection — the driver scopes every frame by `threadId`, hides the agent's transcript
  from the parent's (claude symmetry), and closes the row when the agent answers ("answered"), is
  shut down ("closed"), or dies with an aborted parent turn or the process itself. A follow-up to a
  closed agent opens a fresh row (a finished card never walks back to running). The
  model's `wait` renders as a "waiting for subagents" tool row. No per-agent stop for codex (no
  such client RPC on the wire) — the tray's ■ stays claude-only. Wire facts: PROTOCOL.md Pass 16.
- **Permission prompts.** A warning card ("<tool> wants to run") with a JSON-input preview and
  allow-once / always / reject options, plus a destination cycler for "always" rules (this project
  just-you / all projects / this project shared / this session, persisted in localStorage). The card
  captures focus on arrival; Enter = first allow-once, Esc = first reject (or closes the feedback
  row first when it's open).
- **Deny with feedback.** Every permission card has a "deny with feedback…" affordance: the typed
  reason rides the deny so the agent reacts to it instead of aborting. Claude: the reason is
  appended to the deny directive with `interrupt:false` — the tool errors but the turn runs on;
  codex: the decline answers the rpc, then the reason steers into the running turn (`turn/steer`).
  Either way the reason is journaled as a user message (it's transcript truth — the model received
  it).
- **Plan approval.** Claude's `ExitPlanMode` renders a dedicated card instead of the generic
  permission prompt: the plan markdown itself (sanitized, file references clickable) plus the
  official three answers — "Yes, and auto-accept edits" / "Yes, manually approve" / "No, keep
  planning" — and an optional comment that rides the decision (approvals:
  `updatedInput.userFeedback`/`userComments`; keep-planning: the feedback-denial). Auto-accept
  follows the allow with a `set_permission_mode acceptEdits`, so the mode chip flips with it.
  Enter (card focused) = auto-accept, Esc = keep planning; Enter inside the comment field is
  deliberately inert (a comment can accompany any of the three answers).
- **Structured questions.** The agent's multiple-choice/free-text questions (claude
  `AskUserQuestion` / codex `requestUserInput`) render as a card. Selections are keyed by
  question/option **index**, not by model-authored id/label (those are untrusted and can collide).
  A codex question carrying `autoResolutionMs` auto-skips at the deadline (empty answers — the
  official client's behavior) with a visible "question timed out" notice; claude's question
  timeouts run CLI-side.
- **Where.** `ToolGroup.svelte`, `ToolCallCard.svelte`, `PermissionCard.svelte`,
  `PlanApprovalCard.svelte`, `QuestionCard.svelte`, `AgentsTray.svelte`, `BackgroundTray.svelte`;
  commands `permission` (optional `destination`, `feedback`) / `answer` / `stop_task`; events
  `tool_call`(`_update`/`_output_delta`), `permission_request` (optional `plan` = the plan-approval
  marker + markdown) / `permission_resolved`, `question_request` / `question_resolved`,
  `background_tasks` (level-set + one-shot `closed` verdicts). Wire facts:
  `crates/chimaera-agent/PROTOCOL.md` pass 8 and "Claude: subagents + queueing truth".

## Inline artifacts

- **What & when.** The output *is* the point of many jobs — after a turn's closing prose, a gallery
  previews the previewable files that turn produced (image thumbnail, CSV/TSV first-rows peek,
  embedded PDF). Click a tile to open the full viewer in a pane.
- **Where.** `ArtifactGallery.svelte`, `InlinePreview.svelte`; `turn_end.artifacts` collected by
  scanning back to the turn boundary (only *written* previewable files + touched images; a merely
  *read* CSV isn't an artifact; capped at 8). Uses `POST /api/v1/fs/ticket` → `GET /raw/{ticket}` and
  `GET /api/v1/fs/table`.

## Reconnect & gap-replay

- **What & when.** Chat survives socket drops losslessly. On reconnect the transcript catches up;
  while disconnected the empty state reads "connecting…".
- **How.** `GET /ws/chat/{id}` auth frame carries `last_seq`; the server replays the journal gap
  from that point, then goes live. The client dedupes by `seq` (drops `seq <= lastSeq`); one bad
  event costs one event, not the batch. If the journal head is below `lastSeq` (journal recreated),
  the store hard-resets.
- **Where.** `chatWs.ts` (`ChatSocket`, `Reconnector`), `store.svelte.ts` (`apply` seq-dedupe).
  Engine: the seq-numbered journal in `crates/chimaera-agent/src/journal.rs`.

## The engine — journal, protocol, degrade

- **Normalized event/command model** (`model.rs`): one ACP-shaped vocabulary both drivers translate
  into, so the journal/WS/UI all speak it and a future generic ACP agent slots in. **Size caps live at
  event construction** (`TOOL_OUTPUT_HEAD 12k`/`TAIL 4k`, diff budgets) so a giant tool input never
  reaches the journal, ring, or a client.
- **Seq-numbered journal + gap-replay** (`journal.rs`): every event gets a monotonic, gap-free `seq`
  assigned *once* in `Journal::append` — the durable JSONL, the live broadcast, and every client agree.
  `seq` must stay the first serialized key of `SeqEvent` (the write-path scan depends on it). The
  journal repairs a crash-torn tail, compacts at a turn boundary past `FILE_CAP 4 MiB`, and is
  size-capped per dir (`100 MiB` / `200 files`). Resuming a finished conversation seed-copies the old
  journal so `attach` replays the whole history (via a native-id → chimaera-session index).
- **Pinned protocols** (`claude.rs`/`codex.rs`): the `stream-json` and `app-server` wire formats are
  **unversioned and pinned, not trusted** — each driver is verified against its `TESTED_*_VERSION`
  constant (the current pins live at the top of `claude.rs` / `codex.rs`). Touching a driver or
  bumping a CLI **requires `just chat-smoke`** (live, bills a few cents). The two drivers must stay
  **symmetric**.
- **Handshake watchdog → degrade-to-PTY** (`driver.rs`, `chat.rs`): a chat session that can't prove
  its protocol in 20s fails fast and respawns as the real TUI on the same session id (one attempt),
  so a pane never hangs.

## View switch and rewind

- **View switch.** `POST /api/v1/sessions/{id}/view {ui:"chat"|"term", force?}` flips a session
  between chat and TUI on the same id (same AgentRecord, resume target). Kill-then-respawn is **not
  atomic** — every respawn precondition is resolved before the kill; concurrent toggles serialize on
  `chat_switching` (double-click → 409). A busy `Running` agent needs `force` (409). **Billing note:**
  the TUI side bills like an interactive session; the chat side drives the structured protocol. This
  is also the **`/login` recovery** path (see [Composing & sending](#composing-sending)): an
  expired-auth session flips to its TUI so claude's native auth flow can run.
- **Rewind + fork (claude).** Hover a user message → "↺" → a dry-run report → a dialog listing the
  files that will revert → "restore files" or "restore + rewind conversation". File-restore rides the
  chat socket (`rewind`); the conversation fork is `POST /api/v1/sessions/{id}/rewind {resume_at}`,
  which respawns `--resume … --fork-session --resume-session-at …` and truncates the reused journal at
  the fork. Needs `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true`; tracks only file tools (a
  Bash-created file survives).
- **Rewind (codex, conversation-only).** Same "↺" on user messages that opened a turn (steered
  mid-turn sends aren't rewindable boundaries, and codex has no file restore, so the dialog is a
  plain confirmation). The same rewind endpoint truncates the journal at the checkpoint, counts the
  dropped turns, and respawns `thread/resume` + `thread/rollback {numTurns}` — the thread id
  survives (no fork). The count is journal-derived, so turns run outside chat (TUI-interleaved via
  the view toggle) are invisible to it — see PROTOCOL.md pass 8.

## Status: partial

- Chat sessions survive a *disconnect* **and a daemon restart** — the ledger resurrects them live
  (resuming the native conversation, carrying the pinned title), retiring the non-resumable to
  Recents (see [lifecycle-and-persistence.md](lifecycle-and-persistence.md)).
- Codex **create-time model** is dropped in chat mode (a `TODO(seam)` in `chat.rs`).
- Codex **guardian** auto-approval reviewer is parsed but not rendered.
- Codex rewind's rollback count only sees turns the chat journal saw (TUI-interleaved turns
  undercount it — the rollback then leaves those turns in place).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why chat mode exists
_Captured 2026-07-09 — drafted from DESIGN.md + code, confirmed live with the maintainer._

- **Problem it solves.** The rich Tier-B surface that replaces the Claude desktop app. It became the
  *default* agent view on 2026-07-07, deliberately accepting the billing exposure (Tier B is exactly
  the usage class Anthropic's paused change targets).
- **Core vs addition.** Chat mode is an **addition** — improvable — but two things in it are
  load-bearing and shouldn't be casually undone: `agents.defaultView` as the one-key flip back to
  the billing-safe TUI, and the **protocol-authoritative** state rule (hooks don't fire under
  `-p stream-json`, so state is derived from events, not hooks).
- **Do not change (without care):** the `agents.defaultView` lever and the protocol-authoritative
  rule.

### Plan approval + deny-with-feedback — why they exist
_Captured 2026-07-10 (from the maintainer)._

- **Problem it solves:** chat mode is meant to be "a fully functioning working version of the chat
  UI, so that people don't have to use the wonky TUI most of the time. But for that we can't leave
  features out." Plan approval and deny-with-feedback were the two highest-leverage permission-UX
  gaps vs the official vendor UIs; closing them is feature-parity work, not new invention —
  "Parity!" is the whole rationale for deny-with-feedback.
- **How settled it is:** parity with the official clients is the promise — option wording, wire
  shapes (`updatedInput.userFeedback`/`userComments`, `interrupt:false` denials), and two-driver
  symmetry follow the vendors' own semantics (PROTOCOL.md pass 9). The card layouts themselves are
  **additions** — improvable like the rest of the chat chrome.
- **Do not change (without care):** the parity direction — when the official UIs and chimaera's
  permission UX diverge, the gap is a bug to close, not a place to invent different behavior.

### Parity batch (codex rewind · /compact · question timeouts · background/stop) — why it exists
_Captured 2026-07-10 (from the maintainer, PR #43)._

- **Problem it solves:** chat mode must not be second-class. It is the default agent view, so
  anything the CLI/TUI or the official clients can do — rewind, compact, background a tool, stop a
  subagent — must work there too; using chimaera should carry no capability tax.
- **How settled it is (intended vs provisional):** the *capabilities* are the promise — that
  rewind/compact/background/stop exist in chat mode. The *mechanics* are free to improve: the
  journal-derived turn counting, the dialog shapes, the button styling are implementation details,
  not contracts.
- **Deliberately left out:** a visible countdown for `autoResolutionMs` questions, the codex
  create-time-model seam, and rendering the guardian auto-approval reviewer — all conscious
  deferrals, open for later.
- **Grade — addition, open to change:** improve freely, as long as two-driver symmetry and the
  additive-only wire rule hold. (The one technical sharp edge — codex `thread/rollback` silently
  clamping on overcount, hence the exact journal-derived count — is recorded as a wire fact in
  PROTOCOL.md pass 10 rather than frozen here.)

### Chat tab keep-alive (the chat pool) — why it exists
_Captured 2026-07-11 (from the maintainer)._

- **Problem it solves.** Switching chat tabs must not refetch the journal or drop the socket. A
  session-keyed chat pool holds a warm store + open socket per session, so transcript, scroll, and the
  live stream survive the remount (mirrors the terminal `termPool`; the turn timer's start lives in
  the pool so it survives a mid-turn switch).
- **The promise (the load-bearing bit).** **Chat tabs are as durable as terminal tabs** — a view
  switch is a view switch, never a reload. Keep that parity.
- **Grade — addition** otherwise: the pool mechanics are implementation, free to improve.

### Chat-UX batch (tool-state · subagents tray · thinking-default · slash-anywhere · /login) — why it exists
_Captured 2026-07-12 (from the maintainer, in-session)._

- **Problem it solves:** rough edges that made chat mode feel less than the workbench it should be —
  phantom "running…" tool rows that outlived their turn, subagents buried in scrolled-off tool groups,
  and no way to recover an expired login from chat. The framing the maintainer set: chimaera is a
  **workbench, not a chat window** (unlike the Claude desktop app) — so long-lived, *monitorable* work
  (subagents, the plan) should be promoted OUT of the transcript into calm pinned surfaces, while the
  transcript stays for the conversation. Don't copy vendor chrome; use chimaera's own idioms.
- **Deliberate defaults:** thinking defaults ON for claude ("the reasoning pass earns its keep in a
  coding workbench") — shown on the chip, one click off, and easy to reverse if it's the wrong call.
  The subagents tray is collapsed by default (a quiet one-line indicator, expand for detail).
- **Auth is native-only, by decision:** `/login` routes to the CLI's own auth flow (in the TUI);
  chimaera **never** builds credential entry or handles secrets — auth methods vary (OAuth,
  `setup-token`, SSO) and a custom in-app flow was judged too risky. The `/login`→TUI flip is the
  minimal unblock; the fuller auth UX (auth-fail card, `/logout`, `/auth status`, codex parity, and
  the stuck-`Running`-after-auth-fail state) is a conscious follow-up.
- **Grade — additions, open to change:** all improvable; the load-bearing bits are the
  **native-auth-only** rule and the **workbench-promotion** principle (pin long-lived work, keep the
  transcript for conversation).

### Background-work tray — why it exists
_Captured 2026-07-15 (from the maintainer)._

- **Problem it solves:** both the trigger and the frame. The trigger: backgrounded bash/workflows
  were invisible the moment the turn ended — nothing showed what was still running, nothing could
  kill it, and completions arrived silently. The frame: the same **workbench-promotion** principle
  as the subagents tray — long-lived, monitorable work belongs in a pinned surface, not scrolled-away
  transcript.
- **How settled it is:** **mostly provisional** — only the *why* is settled. The surface, placement,
  notices, and chrome are all free to change if improved.
- **Deliberately open / where it may go:** both known gaps are open improvements, not decisions —
  the close carries `output_file` but the tray doesn't yet open/preview the task's output, and codex
  has no background lane today (claude-only by wire reality, not by design choice).
- **Open to change:** fully. **Grade — addition**: no locked rules beyond the repo's standing
  invariants; don't treat any of the current mechanics as a contract.

### Codex subagents (collab / multi-agent) — why it exists
_Intent pending — not yet captured from the maintainer (shipped 2026-07-16, autonomous session)._

- Derived context, not intent: codex parity for the rich subagent surface was the named open
  improvement in the PR #63 intent capture, and the two-driver-symmetry rule made claude's Agent-row
  surface the natural mapping target. The thread-scoping gate is wire-correctness, not a choice —
  see PROTOCOL.md Pass 16. Run **capture-feature-intent** with the maintainer to replace this stub.
