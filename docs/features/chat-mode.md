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
- **Image paste.** Paste an image → a removable chip; sent as base64 blocks. Downscaled to
  1568px max dim, 2 MiB post-encode cap (oversized silently dropped); the journal stores a
  placeholder, never the bytes.
- **Autocomplete.** `/` → slash-command popover (native chimaera pickers first, then the CLI's
  own commands); `@name` → fuzzy file/dir quick-open; `@term:` → workspace-terminal grants (see
  [linked-terminals.md](linked-terminals.md)). `/rename <name>` pins a session name. `/compact`
  is native for codex (`thread/compact/start`; the compaction runs as its own turn and lands a
  "context compacted" notice) — claude's `/compact` rides its own CLI catalog as prompt text.
- **Where.** `Composer.svelte`, `ChatView.svelte` (`sendNow`, `onSlash`, `composerCommands`),
  `composerBus.ts` (other surfaces drop references into the draft). Uses `fsValidate`/`fsQuickOpen`.

## Header controls

- **Model / effort / mode / thinking / ultracode pickers.** Click a header chip (or `/model`,
  `/effort`, `/mode`) to switch. The agent's *live* catalog (claude `initialize.models` / codex
  `model/list`) beats the daemon's curated list. Effort is per-model (codex falls back to the
  `minimal…xhigh` ladder; **`xhigh` is never relabeled** — canonical vocabulary is sacred).
  Thinking (claude) and ultracode (claude, gated to an xhigh-capable model) are client-held toggles
  reconciled from `effort_state` read-backs.
- **Live telemetry chips.** A rate-limit chip appears at ≥80% / reached; a context chip shows "42%
  ctx". Subscription usage (`/usage`, `/cost`) shows **percentages, never dollars**.
- **Where.** `ChatHeader.svelte`, `EffortPopover.svelte`, `UsagePanel.svelte`; store fields fed by
  `model_switched`/`effort_state`/`mode_changed`/`rate_limit`/`context_usage` events. Commands
  `set_model`/`set_effort`/`set_mode`/`set_thinking`/`set_ultracode`/`get_usage`.

## The transcript

- **Content.** User bubbles (right), agent prose (left, markdown), collapsible "thinking · N chars"
  blocks, per-turn duration rulers, plan/todo panel, tool cards, permission/question cards, inline
  artifacts. A live activity row ("thinking · ~1.2k tokens / writing / <tool>") pulses while running.
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
  pending); `tool_output_delta` streams live output ahead of the authoritative result.
- **Background / stop a running row (claude).** A running tool row offers a ⤓ "continue in the
  background" affordance (`background_tool` → the CLI's `background_tasks`, Ctrl-B parity; a
  refusal lands an honest notice), and a running **Agent** (subagent) row offers a ■ stop
  (`stop_task`; the driver resolves the row id to the CLI's opaque task key). Codex has no
  equivalents — the buttons are omitted there.
- **Permission prompts.** A warning card ("<tool> wants to run") with a JSON-input preview and
  allow-once / always / reject options, plus a destination cycler for "always" rules (this project
  just-you / all projects / this project shared / this session, persisted in localStorage). The card
  captures focus on arrival; Enter = first allow-once, Esc = first reject.
- **Structured questions.** The agent's multiple-choice/free-text questions (claude
  `AskUserQuestion` / codex `requestUserInput`) render as a card. Selections are keyed by
  question/option **index**, not by model-authored id/label (those are untrusted and can collide).
  A codex question carrying `autoResolutionMs` auto-skips at the deadline (empty answers — the
  official client's behavior) with a visible "question timed out" notice; claude's question
  timeouts run CLI-side.
- **Where.** `ToolGroup.svelte`, `ToolCallCard.svelte`, `PermissionCard.svelte`, `QuestionCard.svelte`;
  commands `permission` / `answer`; events `tool_call`(`_update`/`_output_delta`), `permission_request`
  / `permission_resolved`, `question_request` / `question_resolved`.

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
  **unversioned and pinned, not trusted** — each driver is verified against `TESTED_CLAUDE_VERSION`
  (`2.1.204`) / `TESTED_CODEX_VERSION` (`0.142.5`). Touching a driver or bumping a CLI **requires
  `just chat-smoke`** (live, bills a few cents). The two drivers must stay **symmetric**.
- **Handshake watchdog → degrade-to-PTY** (`driver.rs`, `chat.rs`): a chat session that can't prove
  its protocol in 20s fails fast and respawns as the real TUI on the same session id (one attempt),
  so a pane never hangs.

## View switch and rewind

- **View switch.** `POST /api/v1/sessions/{id}/view {ui:"chat"|"term", force?}` flips a session
  between chat and TUI on the same id (same AgentRecord, resume target). Kill-then-respawn is **not
  atomic** — every respawn precondition is resolved before the kill; concurrent toggles serialize on
  `chat_switching` (double-click → 409). A busy `Running` agent needs `force` (409). **Billing note:**
  the TUI side bills like an interactive session; the chat side drives the structured protocol.
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

- Chat sessions survive a *disconnect* but **die with the daemon and are not yet resurrected across a
  restart** — they retire to Recents for manual resume (see
  [lifecycle-and-persistence.md](lifecycle-and-persistence.md); `sv-11` follow-up).
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
  PROTOCOL.md pass 8 rather than frozen here.)
