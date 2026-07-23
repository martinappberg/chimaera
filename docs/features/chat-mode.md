# Structured chat mode (Tier B)

The rich chat surface: instead of running an agent as a raw TUI, Chimaera drives the same
CLI over its **structured JSON protocol** (Claude Code over bidirectional `stream-json`,
Codex over `codex app-server` JSON-RPC) and renders a first-class chat UI — streamed prose
and thinking, tool cards, permission and question prompts, inline artifacts, model/effort
controls, and lossless reconnect. The same session identity can toggle between chat and the
TUI (see [view switch, rewind, and branch](#view-switch-rewind-and-branch)).

**Where it lives (shared):** UI `web-ui/src/lib/chat/` (`ChatView.svelte`, `Composer.svelte`,
`ChatHeader.svelte`, `Markdown.svelte`, `ToolGroup`/`ToolCallCard`, `PermissionCard`,
`QuestionCard`, `RewindDialog`, `ForkDialog`, `McpPanel`, `UsagePanel`, `store.svelte.ts`, `chatWs.ts`,
`paths.ts`). Engine `crates/chimaera-agent/src/` (`driver.rs`, `claude.rs`, `codex.rs`,
`model.rs`, `journal.rs`). Daemon glue `crates/chimaera-server/src/chat.rs`, WS
`ws.rs::chat_ws`. Wire: `GET /ws/chat/{id}` (events + the 19 `AgentCommand`s),
`POST /api/v1/sessions/{id}/view`, `POST /api/v1/sessions/{id}/rewind`,
`POST /api/v1/sessions/{id}/fork`. Deep protocol facts:
[PROTOCOL.md](../../crates/chimaera-agent/PROTOCOL.md); rules:
[rules/agent-protocol.md](../../.claude/rules/agent-protocol.md); working skill:
[chat-mode](../../.claude/skills/chat-mode/SKILL.md).

## Composing & sending

- **Send / queue / steer.** Type, Enter to send (Shift+Enter = newline). Mid-turn sends queue for
  the next run, matching both native clients; the placeholder says so. Codex queued rows expose
  **↪ Steer**, which promotes only that follow-up into the current run via `turn/steer`. Claude has
  no separate steer action. `socket.send` returns `false` when the socket isn't OPEN, so the draft
  is only cleared on an accepted send — a message during a reconnect window is preserved, not lost;
  reconnect replays the daemon-owned queue state.
- **Delivery honesty + pending stack.** A mid-turn send doesn't become a delivered history block —
  it waits in a faded **pending stack at the scrollable transcript tail**, with a small "queued"
  mark. It therefore stays at the live end without covering the reading area or growing fixed
  composer chrome. After a turn ends, Claude flushes its held queue; Codex opens exactly the oldest
  queued item as the next turn and leaves later items queued. A Codex Steer instead resolves on the
  steer RPC acknowledgement. Once consumed, the block leaves the stack and enters the transcript proper
  as the newest user turn, solid. A genuinely undeliverable entry stays marked **"not delivered"**
  (text kept readable/copyable, never auto-dumped into a draft you may have started). The ✕ pulls
  back a queued item or dismisses a dropped one. This is driven by the single `pendingSends` reducer
  and journaled via `user_message` `id`/`queued` + `user_message_update`, so replay rebuilds the same
  order and delivery truth (see PROTOCOL.md passes 8 and 21).
- **Image paste.** Paste an image → a removable chip; sent as base64 blocks. Downscaled to
  1568px max dim, 2 MiB post-encode cap, at most four images (8 MiB total); oversized images
  are silently dropped. The daemon independently enforces those image budgets plus 256 KiB of
  text, a 10 MiB pre-deserialization WebSocket envelope, and a per-session 32 MiB / 64-message
  aggregate across the manager channel and driver-held send queue. A refused command raises a
  visible, nonfatal notice; the socket stays healthy. The journal stores a placeholder, never the
  bytes.
- **Long drafts.** The composer grows upward with wrapped text to a pane-conscious cap, then scrolls
  internally. Its subtle top-edge grip can expand or contract it manually (drag for a precise size;
  click to toggle expanded/content-fit; Up/Down resize from the keyboard and Home returns to
  content-fit). A manual size remains the baseline rather than locking the input: once new text uses
  the available room, the composer keeps growing to the cap. A successful send resets the next draft
  to its natural height.
- **Autocomplete.** `/` → slash-command popover (native chimaera pickers first, then the CLI's
  own commands and Codex's enabled skills); `@name` → fuzzy file/dir quick-open; `@term:` → workspace-terminal grants (see
  [linked-terminals.md](linked-terminals.md)). `/rename <name>` pins a session name. `/compact`
  is native for codex (`thread/compact/start`; the compaction runs as its own turn) — claude's
  `/compact` rides its own CLI catalog as prompt text. Manual and automatic compaction on either
  agent normalize to one journaled lifecycle: while history is being summarized the live status
  reads **"compacting context"** with an indeterminate progress track (no invented percentage),
  the dashboard carries the same now-line, and completion becomes a quiet transcript notice
  (including Claude's pre-summary token count when reported). Reconnect/replay restores an active
  compaction instead of falling back to a generic working spinner; turn abort, failure, or driver
  exit always settles it.
  The slash popover triggers after any whitespace boundary, so prose such as
  `please use /skill` remains fully editable while the matching command menu opens. A mid-draft
  pick completes the token in place; only a whole-draft slash takes the command path. Native
  command arguments are completable too (`/model`, `/mode`, `/effort`, `/ultracode`). Path
  fragments such as `src/skill` are not command boundaries, and an unmatched slash token remains
  ordinary text.
  Codex's cwd-scoped `skills/list` inventory becomes bounded `/skill-name` rows. Sending prose
  containing an exact skill token preserves the visible text and adds Codex's native
  `{type:"skill", name, path}` input block; Claude continues to execute its advertised commands as
  text. Skill invocations are de-duplicated and capped at eight per send.
- **`/login` recovery (claude).** An expired-auth chat session dead-ends — the `-p stream-json`
  CLI answers "/login isn't available in this environment". `/login` (palette + intercepted on
  send) instead flips the session to its real TUI (the [view switch](#view-switch-rewind-and-branch)),
  where claude's own `/login` runs the native auth flow (OAuth / setup-token / SSO); chimaera
  never touches the credentials. Sign in there, toggle back to chat.
- **Where.** `Composer.svelte`, `composer.ts`, `ChatView.svelte` (`sendNow`, `onSlash`, `composerCommands`),
  `composerBus.ts` (other surfaces drop references into the draft). Uses `fsValidate`/`fsQuickOpen`.

## Header controls

- **Model / effort / mode / thinking / ultracode pickers.** Click a header chip (or `/model`,
  `/effort`, `/mode`) to switch. The agent's *live* catalog (claude `initialize.models` / codex
  `model/list`) beats the daemon's curated list. Effort is per-model (codex falls back to the
  `minimal…xhigh` ladder; **`xhigh` is never relabeled** — canonical vocabulary is sacred).
  Codex exposes Read only / Auto / Auto review / Full access / Plan; every switch sends a complete
  settings tuple so a prior reviewer, sandbox, approval policy, or collaboration mode cannot stay
  invisibly active. Claude's launch-gated Bypass permissions mode is not offered: structured chat
  sessions do not start with `--dangerously-skip-permissions`, so Claude would reject the switch.
  New and resumed Codex chats default to **Auto review** at thread open — workspace writes remain
  sandboxed and escalation remains on-request, while `auto_review` assesses approval requests.
  The header reflects that state from the first `Init`; `/mode auto-review` switches it explicitly,
  and the common `/model auto-review` slip is recovered as a mode switch with a corrective notice.
  The model chosen when a Codex chat is created rides `thread/start` (and resume), rather than
  silently falling back to the CLI default until the first header change.
  Codex's effort chip is seeded from the effective thread setting and each selection is applied
  immediately through `thread/settings/update`, then reconciled from the agent's settings
  notification so it survives remount/reconnect. Older app-servers fall back to carrying the
  selected effort on the next `turn/start`; Plan mode keeps its nested reasoning effort aligned.
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
  shows a muted "stopped" notice, dangling subagents settle neutrally instead of turning their
  tool group red, and the session rail reads finished — error-red `turn failed`, failed tool rows,
  and the Errored rail state are reserved for genuine failures.
- **Rendering.** Streamed prose reveals word-by-word on a ~75ms ticker (respects
  `prefers-reduced-motion`). Markdown and Codex's common LaTeX delimiters (`$…$`, `$$…$$`,
  `\(…\)`, `\[…\]`) render through **marked + KaTeX MathML → DOMPurify**; math is excluded from
  the streaming word wrapper so equations are never split into animated fragments. `<style>` tag
  and `style` attribute are forbidden (injected CSS can't restyle the workbench to spoof a
  permission prompt), KaTeX trust is off, every external anchor is forced to
  `target="_blank" rel="noopener noreferrer"`, and bad local anchors are neutralized on click.
  Client-side transcript cap of 2000 blocks (oldest dropped behind one "earlier history trimmed"
  notice; the live tail is never touched). User bubbles remain verbatim plain text—not Markdown—but
  recognized LaTeX spans use the same sanitized MathML renderer, so typed equations do not show raw
  delimiters.
- **Assistant message metadata + actions.** Hover an assistant message (or focus one of its actions)
  to reveal a slim rail directly below the prose: the journal-backed send time, copy-full-message,
  and conversation-fork actions. The timestamp starts at `now`, advances through minute labels and
  `1h ago`, then switches to the user's local hour cycle and calendar (`today`, `yesterday`, nearby
  weekday, dated time, and a year-bearing date for older calendar years). One view-level timer wakes
  only at the next label boundary rather than leaving an interval on every transcript row. Touch
  devices keep the rail visible; a successful copy briefly swaps its icon to a checkmark.
- **Hydration + history window.** A fresh attach folds replay into the reducer behind one quiet
  "loading recent conversation" state until the advertised journal `head` arrives; it then mounts
  the newest 64 blocks bottom-anchored in one paint, rather than visibly growing from the oldest
  message. Explicit earlier/later controls page 64 blocks at a time while preserving the paragraph
  under the reader; the middle of a long conversation is never skipped just because the bounded page
  no longer contains the live edge. The DOM window stays capped at 192 blocks and a clear jump returns
  directly to the live tail. Historical artifact tickets, table queries, image decodes, and PDF embeds wait until their
  preview approaches the viewport. This is client-side rendering pagination, not lossy history: the
  reducer still holds the capped 2000-block transcript and the daemon journal remains authoritative.
  Replay/live/control frames are reduced through one order-preserving cooperative queue, yielding
  between bounded slices so a large remote journal cannot monopolize navigation clicks.
- **Scroll ownership.** Stream events, Markdown reveals, and late artifact sizing all request
  bottom-follow through one frame-coalesced scroll writer. Paging explicitly into older history
  keeps that historical page stable until the reader returns to newest; merely scrolling within the
  live tail does not freeze it. Visible tail rows point directly at the reducer's reactive blocks, so a
  streamed delta updates its own row instead of cloning/repainting the whole window. The tail keeps
  rendering while the reader scrolls or types. A non-empty composer draft or attachment pauses only
  auto-follow; incoming events extend the bounded tail behind a source-row anchor and surface as “new
  activity” rather than moving the text box or reading position while the user types. Submitting is an
  explicit return to the live edge, so the sent/queued bubble and its reply stay visible.
- **Tab lifecycle.** Recently viewed chat tabs retain only that bounded rendered page in the pane's
  live set. A visible page uses live reducer references; hiding it takes one plain-data transcript
  snapshot plus snapshots of plan/subagent/background/permission/question/queued-send chrome, and
  then scroll-following, Markdown rendering, elapsed clocks, question countdown painting, tray/tool
  animations, and streaming tool-body scrolling pause. Keyed cards stay mounted, so expanded trays,
  selected question answers, and typed permission/plan feedback survive. Hidden prompts never steal
  focus from the active tab; they become focusable again on return. Reactivation catches up behind the saved source-row anchor;
  a backlog too large for the bounded page remains explicit “new activity” rather than evicting the
  row the reader left on. The pooled
  store/socket continues folding authoritative events. Activation reconciles one page at the right
  end in a single paint. If the live set evicts or moves the view, `chatPool` preserves the reducer,
  socket, scroll intent, and rendered-range cursor for a bounded warm remount. Dashboard reporting
  remains independent on the daemon's session event bus, so none of this pauses the agent.
- **Where.** `ChatView.svelte` (`renderItems`), `store.svelte.ts` (the `blocks` reducer),
  `AgentMessageMeta.svelte`, `Markdown.svelte`, `UserText.svelte`, `chatPool.ts`, and
  `layout/Pane.svelte`; timestamp formatting and refresh boundaries live in `../shared/time.ts`, and
  clipboard writes use `../shared/clipboard.ts`.
- **Untrusted output.** Everything the model emits is attacker-influenced — see
  [rules/web-ui.md](../../.claude/rules/web-ui.md). Tool-card bodies/diffs render as plain `<pre>`
  (no `{@html}`).

## Tool cards, permissions & questions

- **Tool cards + grouping.** Each tool call is a collapsible card (title, glyph, status dot,
  output/diff, a `↗` to open the touched file). Consecutive calls condense into a group ("6 commands
  · 2 files"). Groups are collapsed by default, including while work is running, and remain
  expandable on demand; the summary badge (`running…` / `failed` / `recovered`) carries the verdict
  without turning live activity or history into a wall of command rows.
  Tool calls upsert by id (a late enriching re-emit never walks a finished tool back to
  pending); `tool_output_delta` streams live output ahead of the authoritative result, but a late
  delta may only enrich terminal text — it cannot revive the streaming cursor. **Dangling
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
  ends, and dies with the CLI process. Teardown clears its level-set, and every successful driver
  spawn journals the same empty set before its first event so a crash-tailed journal cannot revive
  dead work when the daemon resurrects a session. When a task settles, its verdict folds into the
  transcript as a quiet notice ("background “…” completed — … (exit code 0)"; failed renders as an
  error). The driver ingests the CLI's task lanes — `background_tasks_changed` (the authoritative
  REPLACE-the-set signal, and the **only** frame that admits a task to the tray) + `task_updated`
  (patch) + `task_started` (non-`local_agent`; enrichment only — it adds the workflow name / card
  binding the set change lacks) + `task_notification` (the only frame carrying the verdict summary)
  — into one level-set `background_tasks` event. Membership hangs on the set change alone because a
  *foreground* long-running Bash emits a `task_started` identical to a backgrounded one (PROTOCOL.md
  Pass 23) — trusting that frame parked every slow foreground command in the tray. Tasks the
  set-change removes before their verdict park in a bounded
  departed buffer so the notification arriving ~ms later still closes them (the live-verified settle
  order). The ■ stop sends `stop_task` with the native task key — the CLI's stop is generic over its
  task registry and acks a raced not-found as success. Codex has no Bash/workflow background-task
  lane — that tray never renders there; long-lived Codex subagents stay in the sibling Agents tray.
- **Rich workflow rows + card verdicts (claude).** A `local_workflow` lane renders richer than a
  bare bash row: the tray row leads with the workflow's `meta.name` (from `task_started
  .workflow_name`; lane + description stay in the tooltip) and carries a per-agent **dot row**
  (filled = done, red = error-ish states, hollow = anything else — at most 24 dots, newest win,
  per-dot tooltip = label — state: result preview) plus an honest "done/total agents" count. The
  driver folds `task_progress.workflow_progress` (per-agent `{index, label, state, resultPreview…}`,
  live-probed shape — PROTOCOL.md Pass 15) into the level-set — 24 stored agents per task (exactly
  the dot-row budget) plus a set-wide 96-entry budget that sheds the oldest tasks' dot rows so one
  `BackgroundTasks` event can never blow the journal's 256 KiB entry cap — with totals deduped and
  counted over the whole wire list, and re-emits **only on state transitions** (the stored fields
  exclude per-tick token churn, so the journal stays quiet). A trailing progress frame that races
  the settle removal still patches the parked counts, and teardown lands an honest
  `workflow "name" interrupted · N/M agents` line on any card-bound run that dies with the CLI. The launching `Workflow`
  tool card ticks "N/M agents done" while the run lives and lands a final
  `workflow "name" completed · 4/4 agents · 7m 15s` line at the close (a `failed` verdict flips the
  card red); the count/elapsed survive the departed buffer because they ride the task identity.
  Inner workflow agents' permission asks ride the normal `can_use_tool` path — they surface as
  ordinary permission cards, and a denied agent shows as an error dot.
- **Live subagents tray + active plan step.** Subagents running *right now* are promoted out of the
  (collapsed) tool groups into a live monitor pinned just above the composer, so parallel work stays
  glanceable instead of scrolling away — collapsed by default to a one-line "N subagents working"
  summary, expandable to each agent's progress line (tools · tokens, from `task_progress`) + stop.
  They keep their in-place "Agent:"/"Task:" row in history; a finished/abandoned run drops from the
  tray (reconciled shut at turn end; a deliberate Stop settles neutrally, a genuine error fails).
  The plan/todo panel likewise surfaces the current step in its
  summary ("plan · 1/3 · ◐ …"). Both are pure derivations over `blocks`/`plan` — no new events
  (`AgentsTray.svelte`).
- **The task list is the plan panel, whichever tool spells it.** Claude replaced `TodoWrite` with an
  incremental `TaskCreate`/`TaskUpdate`/`TaskList` family; both feed the same pinned panel and
  neither leaves bookkeeping rows in the transcript. Because the new family carries more than a
  checklist, a row can show **who owns it** (`@name`) and **what blocks it** ("blocked by #1", with
  its own ⊘ mark since a blocked task is still `todo`), plus a muted detail line when the task's
  description says more than its subject. Blockers are filtered to ones still open, so finishing a
  dependency visibly unblocks its dependents. The in-progress summary prefers the agent's own
  present-continuous `activeForm` ("Running tests"). Older CLIs and codex, whose plans carry none of
  this, render exactly as before — the extra fields are simply absent.
- **Three pinned strips, one shell, all minimizable.** Plan, subagents, and background work are three
  orthogonal readings of a session — what the agent *means* to do, *who* is working, what is
  *detached* — so they share the `WorkTray` chrome and stack above the composer, collapsed by
  default, each a one-line glance. Only non-empty strips render, and the plan's glyph stays still
  unless a step is actually in flight. Inside the plan, finished tasks fold into a "N done" line so a
  long list shows what's next rather than a wall of ✓ (they unfold on click; a fully-finished plan
  shows its rows, since nothing else remains). The workspace dashboard card follows the same rule —
  live tasks first, finished ones as a count — so it never fills with ✓ while hiding the work in
  flight. **A subagent's task list is its own**: its `Task*` calls never reach the parent stream, so
  they cannot appear in the parent's plan (verified live) — the subagent surfaces as its Agent row
  and tray entry instead. Where the two connect is `owner`: a task claimed by a teammate shows
  `@name`, which is the same identity `TaskStop` accepts.
- **Codex subagents (collab / multi-agent), same surface.** Codex 0.144.x delegation renders the
  identical way: each spawned agent is an "Agent: {name}" row (name = the model's own
  `agentPath` name) whose progress line folds the agent's live activity (thinking · a command ·
  N tools · M tokens), so the tray works unchanged. A subagent is a real thread multiplexed onto
  the same connection — the driver scopes every frame by `threadId`, hides the agent's transcript
  from the parent's (claude symmetry), and closes the row when the agent answers ("answered"), is
  shut down ("closed"), or the process itself dies. A parent answer/abort does not close the child:
  delegated threads may keep working after the parent turn, so their cross-turn rows remain in the
  pinned Agents tray and keep the rail/dashboard's off-screen-work cue live until the child's own
  turn ends. A follow-up to a
  closed agent opens a fresh row (a finished card never walks back to running). The
  model's `wait` renders as a "waiting for subagents" tool row. No per-agent stop for codex (no
  such client RPC on the wire) — the tray's ■ stays claude-only. Wire facts: PROTOCOL.md Passes 16
  and 27.
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
- **Codex auto review.** Auto review is the Codex chat default. It keeps the workspace sandbox and on-request policy but
  assigns Codex's `auto_review` reviewer. Its unstable `item/autoApprovalReview/started|completed`
  lifecycle is normalized into an ordinary bounded tool card (reviewed action/files, verdict,
  risk, rationale); a denied action is a successful safety verdict, while timeout/abort renders
  failed. Codex also repeats routine successful reviews through `guardianWarning`; those duplicates
  stay inside the compact tool history, while genuine or unrecognized guardian warnings remain
  visible as transcript notices.
- **Structured questions.** The agent's multiple-choice/free-text questions (claude
  `AskUserQuestion` / codex `requestUserInput`) render as a card. Selections are keyed by
  question/option **index**, not by model-authored id/label (those are untrusted and can collide).
  A codex question carrying `autoResolutionMs` shows a live "skips in Ns" countdown, then auto-skips
  at the driver deadline (empty answers — the official client's behavior) with a visible "question
  timed out" notice. The countdown is presentation-only and never disables an answer: browser and
  remote-daemon wall clocks can differ, so only the driver's monotonic deadline and resulting
  `QuestionResolved` close the card. The journal stores the absolute deadline so reconnect and replay
  do not restart the clock; claude's question timeouts run CLI-side.
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
- **Board `show` cards.** A completed command whose result carries `chimaera board show`'s
  `shown … → <path>.board` line (a legacy `.board.json` path still matches) grows a `ShownCard` under the producing tool row: the board's
  server-rendered PNG (`POST /board/render`, content-addressed) in a bordered card; click opens the
  board pane. Detection is client-side in `ToolCallCard.svelte` (relative paths resolve against the
  session cwd) — the planned daemon `shown` journal event (board-plan §10.1) can replace it without
  changing `ShownCard.svelte`.

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

## View switch, rewind, and branch

- **View switch.** `POST /api/v1/sessions/{id}/view {ui:"chat"|"term", force?}` flips a session
  between chat and TUI on the same id (same AgentRecord, resume target). Kill-then-respawn is **not
  atomic** — every respawn precondition is resolved before the kill; concurrent toggles serialize on
  `chat_switching` (double-click → 409). On term→chat, native or earlier Chimaera history is imported
  before the transition marker, so the chat reopens with its transcript rather than only “continued
  in chat”; a terminal resurrected after daemon restart uses its durable resume handle even before a
  fresh transcript hook arrives. A busy `Running` agent needs `force` (409). **Billing note:**
  the TUI side bills like an interactive session; the chat side drives the structured protocol. This
  is also the **`/login` recovery** path (see [Composing & sending](#composing-sending)): an
  expired-auth session flips to its TUI so claude's native auth flow can run.
- **Branch at any message, without stopping the source.** Hover an assistant response and choose its
  fork action to create a new idle chat immediately after that response. The composer is empty and no
  synthetic prompt or agent turn is sent. Forking from your own message instead branches immediately
  **before** that message and restores its text into the new composer as an unsent draft, ready to
  edit. The picker can target any installed chat-capable agent. An exact same-agent boundary uses the
  vendor's native history: Claude can use the selected user checkpoint's preceding message; Codex
  uses `thread/fork` through a completed turn. Every other combination — cross-agent, a same-agent
  boundary the native API cannot represent exactly, or a source with no reusable native id — copies
  the normalized visible Chimaera prefix and installs a bounded vendor-neutral transcript as quiet
  system/developer context. The new journal retains the full copied visible prefix; only the
  model-facing handoff is head/tail capped. Neither path rolls files back, kills the source process,
  truncates its journal, or asks the destination agent to speak before the user sends something.
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
  (resuming the native conversation, carrying the pinned title). A normally finished Codex chat
  preserves its native thread id in Recents, whose click starts `thread/resume` under a new Chimaera
  session id (see [lifecycle-and-persistence.md](lifecycle-and-persistence.md)).
- Codex rewind's rollback count only sees turns the chat journal saw (TUI-interleaved turns
  undercount it — the rollback then leaves those turns in place).
- Native same-agent branches are available only at boundaries the vendor exposes: a Claude user
  checkpoint can authenticate the message immediately before that prompt, while Codex exposes
  completed turns. The branch action remains available at every message, but uses the portable
  handoff elsewhere.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Conversation branching — why it exists
_Captured 2026-07-19 (from the maintainer, in-session)._

- **Problem it solves:** the maintainer wants to "fork a conversation (and keep the current one
  running) at any message in the conversation" and send that branch to Codex, Claude, or another
  agent. A failed rewind after moving conversations exposed that the old destructive-respawn path
  was not a general branching primitive.
- **Settled behavior:** the source conversation keeps running; the branch may start at any rendered
  user or assistant message; and when source and target are the same agent, Chimaera should use that
  agent's native fork rather than translating through the standardized format whenever the native
  protocol can represent the selected boundary.
- **Deliberately open:** the visual chrome, icon, labels, and the normalized portable transcript
  presentation are implementation details and may improve. Native APIs do not expose every rendered
  boundary, so the portable fallback at those points is an explicit compatibility choice, not a claim
  of native identity.
- **Grade — addition:** the branching surface is improvable. Do not casually turn it into an in-place
  rewind, stop the source, or prefer a portable handoff over an available exact native fork.

### Rich workflow rows — why they exist
_Captured 2026-07-16 (from the maintainer, PR #69)._

- **Problem it solves:** "parity with the claude app" — the official chat UI renders Workflow runs
  as a rich card (name, agent count, elapsed, per-agent dots); Chimaera's chat mode should show the
  same run, not an anonymous tool card.
- **How settled it is:** nothing here is a promise — it's "subject to change if how claude works
  changed." The feature tracks the upstream wire (PROTOCOL.md Pass 15) and should follow it when it
  moves.
- **Deliberately open / non-obvious decisions:** none named by the maintainer. (The tray-as-live-
  surface / card-as-transcript-artifact split is the implementer's fit to the existing workbench
  design, not a maintainer constraint.)
- **Do not change:** nothing — the **whole area is open**. Grade: an addition, no core bets.

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
  session-keyed chat pool holds a warm store + open socket per session, so transcript state, the
  bounded render cursor, scroll, and the live stream survive a remount (mirrors the terminal
  `termPool`; the turn timer's start lives in the pool so it survives a mid-turn switch). Hidden
  views freeze only their bounded rendered snapshot: the pooled reducer/socket and daemon-owned agent
  continue working, and returning to the chat can send the next message normally. Even an LRU client
  eviction closes only that browser socket; it never stops the agent, and the next open gap-replays
  the journal.
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

### Codex parity (review · launch model · math · timed questions) — why it exists
_Intent pending — not yet captured from the maintainer (shipped 2026-07-17, autonomous session)._

- Derived context, not intent: the maintainer requested parity with the Claude chat experience while
  preserving Codex-native behavior, including its current modes, queue/steer semantics, and protocol
  quirks. Run **capture-feature-intent** with the maintainer to replace this stub.

### The `Task*` task list (owners, blockers, folded completions) — why it exists
_Intent pending — not yet captured from the maintainer (shipped 2026-07-18)._

- Derived context, not intent: the maintainer noticed task bookkeeping rendering as anonymous
  transcript rows and asked whether the plan panel had already been fixed — it had (#25, #48), but
  only for `TodoWrite`, which Claude replaced. So the *restore* is a regression fix; what is a real
  choice is how far the panel goes beyond a checklist. The maintainer chose "full richness now"
  (owner + blockers + description) over parity-first, and separately asked that a failed tool group
  stop auto-expanding, that finished tasks minimize, and that the panel always be minimizable —
  stated as a standing UI bar: "make sure this is very usable and not too cluttered".
- Known-open, not decided: the `@owner` chip is a label, not a control, though `TaskStop` accepts the
  same agent identity — wiring it would cross into the Mastermind story and was left deliberately
  untouched. Run **capture-feature-intent** with the maintainer to replace this stub.
