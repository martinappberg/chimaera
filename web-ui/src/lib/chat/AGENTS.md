# web-ui/src/lib/chat — the structured chat surface

Orientation for coding agents. This directory is the **front half of chat mode**:
the rich UI that renders the daemon's structured agent stream (Claude & Codex),
the sibling of the xterm.js terminal surface. Parent map: repo-root
[AGENTS.md](../../../../AGENTS.md). The back half it talks to is
[`crates/chimaera-agent`](../../../../crates/chimaera-agent/AGENTS.md).

Svelte 5 (runes: `$state`/`$derived`/`$effect`/`$props`). Build/check needs
Node 22 (`nvm use 22`); the nvm default (16) errors.

## The one flow to hold in your head

```
  daemon  ──WS /ws/chat/{id}──▶  chatWs.ts (ChatSocket)
                                     │  auth(last_seq) → ready(head) → batch replay → live ev
                                     ▼
                                store.svelte.ts (ChatStore.apply)   ← the reducer
                                     │  seq-dedupe, folds events into `blocks`
                                     ▼
                                ChatView.svelte  ← renders blocks + composer + overlays
                                     │  user types ─▶ socket.send(AgentCommand)
                                     └──────────────────────────────────────────▶ daemon
```

**`ChatStore.apply(entry)` is the heart.** It is a reducer: one `SeqEvent` in,
store mutation out. Events below or at `lastSeq` are dropped (dedupe). A throwing
event is caught in `chatWs` so it can't strand the batch. On `ready`, if the
journal `head` is below our `lastSeq` the journal was reset — the store
hard-resets and rebuilds.

## File map

| File | What it owns |
|---|---|
| `store.svelte.ts` | `ChatStore` — the reducer + all reactive view state (`blocks`, `pending`, `pendingSends`, `questions`, model/mode, activity, exited/degraded/connected), including initial replay hydration through the ready-frame `head`. **The single source of truth for the view.** Its reducer has a vitest test (`store.svelte.test.ts`) — the one place the UI is unit-tested. |
| `chatWs.ts` / `cooperativeQueue.ts` | `ChatSocket` — connect/auth/reconnect(backoff)/gap-replay, then dispatch replay/live/control frames through one order-preserving cooperative queue so a cold history cannot starve browser input. Per-command refusals (`command_failed` / `invalid_command`) are visible but nonfatal. Shares reconnect accounting with `../terminal/ws.ts`. |
| `chatPool.ts` | Session-keyed warm reducer/socket + scroll/render-window/followed-revision cursor. The agent keeps folding while a tab's bounded DOM snapshot is hidden or its view is evicted; client-pool eviction never stops the daemon-owned process. |
| `ChatView.svelte` | The host: renders a bottom-anchored transcript window (64 blocks initially, 192 maximum; explicit earlier/later pages + a direct jump to newest), and hangs the header/composer/overlays/panels off itself. Visible tail rows are reducer proxies; hidden/history rows are one inert snapshot. A fresh replay stays gated until `head`, so it never paints oldest-to-newest. Still the big one — keep new chrome in child components, not inline. |
| `transcriptWindow.ts` | Pure range math for the 64-block/192-block sliding transcript DOM window; tests cover both paging directions and stale cursor repair after reducer compaction. |
| `ChatHeader.svelte` | The header row: model / mode / effort pickers, usage + `/mcp` entry, session identity (always names which agent — Claude or Codex). |
| `EffortPopover.svelte` | The reasoning-effort ladder picker (uses the agent-native vocabulary verbatim — never relabel `xhigh`). |
| `Composer.svelte` / `composer.ts` | Input chrome plus the pure slash-context, argument-completion, and Codex skill-block helpers (covered by `composer.test.ts`). Slash discovery is whitespace-boundary aware; path fragments must stay ordinary text. |
| `Markdown.svelte` / `MathText.svelte` / `math.ts` | Render agent prose and plain user-message LaTeX (`$`/`$$` and Codex's `\(`/`\[` forms) as KaTeX MathML under one bounded policy. **Sanitize untrusted/replayed content** (marked/KaTeX → DOMPurify, KaTeX trust off, `<style>` forbidden, external links `noopener`); Markdown also stamps validated file paths as clickable. |
| `ToolCallCard` / `ToolGroup` | Tool-call rendering (title, status, diff/output, grouping). Terminal rows may accept late output text but must never revive their streaming cursor. |
| `AgentsTray.svelte` / `BackgroundTray.svelte` | Two of the three pinned strips above the composer: live subagents (derived from in-flight Agent tool rows) and live background tasks (the `background_tasks` level-set), each with a stop affordance. Chrome lives in the shared `../shared/WorkTray.svelte` + `WorkTrayRow.svelte` shell; elapsed/duration text uses `../shared/time.ts`. The **plan strip** is the third, rendered inline in `ChatView` on the same `WorkTray` shell (`pulse` off unless a step is in flight) — three orthogonal readings of the same session: what the agent *means* to do (plan), *who* is working (subagents), what is *detached* (background). |
| `PermissionCard` / `QuestionCard` | The permission prompt and structured-question cards (their answers ride `socket.send`; `PermissionCard` also carries the deny-with-feedback field; `QuestionCard` presents Codex auto-resolution deadlines without owning the authoritative timeout). |
| `PlanApprovalCard.svelte` | Claude `ExitPlanMode` plan-approval card — renders the sanitized plan markdown + the three official options (auto-accept / manual / keep-planning) with an optional comment that rides the permission reply. |
| `RewindDialog.svelte` | The destructive in-place rewind/fork-point confirmation overlay (claude rewind + codex `thread/rollback`). |
| `ForkDialog.svelte` | The non-destructive conversation-branch picker: target agent plus native-vs-portable boundary disclosure. |
| `AgentMessageMeta.svelte` | The hover/focus rail below assistant prose: localized journal-backed timestamp, full-message copy, and the conversation-fork affordance. Its pure time ladder lives in `../shared/time.ts`. |
| `McpPanel.svelte` / `UsagePanel.svelte` | The `/mcp` linked-server panel and the token-usage panel. |
| `InlinePreview` / `ArtifactGallery` | Inline file/image previews inside the transcript; expensive tickets/table reads/image/PDF loads are intersection-gated near the viewport. |
| `UserText.svelte` | User-message bubble: plain text (never Markdown), validated path/mention affordances, recognized LaTeX spans delegated to `MathText`. |
| `paths.ts` | Path-candidate detection + validation types (shared with Markdown's stamping). |
| `composerBus.ts` | Cross-component channel to insert text/attachments into the active composer (e.g. `@term:` grants, references, dropped-file paths). |
| `composerHeight.ts` | Pure height policy for content-fit growth plus manual resize baselines; covered by `composerHeight.test.ts`. |
| `drafts.ts` | Per-session composer draft persistence (survives the per-session ChatView remount + a page reload) — text layers into sessionStorage, images stay in-memory; both bounded. It also publishes which drafts remain memory-only so an interface-build transition cannot silently reload over them. |
| `images.ts` | Pasted/dropped image → downscale + base64 encode into an `ImageAttachment` (the canonical home of that type); size-bounded. |

The transcript's copy affordances — fenced code blocks and whole assistant
messages — reuse `../shared/clipboard.ts` (the native-first clipboard writer
lifted out of the terminal pool) — see the shared/ area.

## Invariants / gotchas

- **Agent output is untrusted.** Anything the model emits (prose, tool output,
  file contents it echoes) is attacker-influenced. Render it through
  `Markdown.svelte`'s sanitizer; never `{@html}` raw agent text elsewhere; never
  build a live external link without `rel="noopener"`.
- **Math stays inside the same trust boundary.** KaTeX emits MathML with
  `trust:false`; DOMPurify still sanitizes the combined result. Exclude `.katex`
  descendants from path stamping and streaming word spans — mutating generated
  math markup corrupts equations.
- **Never lose a user action to a closed socket.** `socket.send` returns `false`
  when not OPEN — respect it (the composer keeps the draft; `store.connected`
  tracks liveness). Reconnect replays the gap; don't invent a client-side queue.
- **A queued send is NOT a transcript block.** Queued/undelivered user messages
  live in `store.pendingSends` (rendered at the scrollable transcript tail), never
  in `blocks` — so a mid-turn send can't splice into a running turn's output or
  crowd the fixed composer. The reducer moves
  an entry into `blocks` (appended at the end) only when `user_message_update`
  resolves it `sent`; `cancelled` removes it; `dropped` marks it "not delivered"
  and it stays in the stack until dismissed. A **Stop never drops the queue** —
  the driver aborts only the current turn and the held messages resolve `sent`
  right after, so `dropped` means genuinely undeliverable (agent died). The ✕ on
  any pending bubble rides `socket.send({type:"cancel_queued", id})`: it pulls
  back a queued send, dismisses a dropped one (the driver's tombstone
  `Cancelled` makes that survive replay), and no-ops for one already delivered.
  Codex rows additionally expose `socket.send({type:"steer_queued", id})`:
  that removes only the selected FIFO entry and maps it to `turn/steer`; plain
  Enter remains queue-for-next-turn.
  All pure reducer, so replay rebuilds the identical order — see
  `store.svelte.test.ts`.
- **The seq contract is the daemon's.** Trust `lastSeq`/`head` from the wire; do
  not renumber. A gap is healed by reconnect replay, not by client bookkeeping.
- **Inactive UI is not an inactive agent.** A hidden retained chat freezes its
  bounded transcript plus auxiliary plan/subagent/background/ask/send snapshots,
  while `chatPool` keeps its reducer and socket warm and the daemon-owned process
  continues working. Invisible timers/animations stop, but keyed cards stay
  mounted so expansion, comments, and question choices survive. A hidden
  permission/plan card must never call `focus()`; it may focus when its view
  becomes visible. Live-set or client-pool eviction may unmount a view or close
  a parked *client socket*, never the agent; the next acquire gap-replays the
  journal.
- **Scroll restoration is window-aware and has one writer.** Save `scrollTop`,
  the bounded absolute block range, whether it still tracks the tail, and the
  transcript revision the reader followed. Page/activation changes anchor by
  source-row index; stream/Markdown/content and transcript-viewport resize
  follow requests coalesce into one frame. Pinned-tray/composer height changes
  are inputs to that same writer, never independent scroll owners. A live tail
  continues rendering while the reader scrolls or types, but
  a non-empty draft pauses auto-follow. Hidden tabs snapshot once and must not
  retain reactive block proxies. Replay never remounts the entire transcript.
- **Fork boundaries are event-backed.** A rendered block's `forkSeq` is the
  latest sequence that makes that message true on replay (a queued user message
  advances on its `sent` update; a final Codex assistant message advances on
  `turn_completed`). An assistant action includes that block and opens with an
  empty composer; a user action passes its own id/seq so the daemon can derive
  the exact cut before delivery, then restores the selected text through
  `composerBus` as an unsent destination draft. Only pass `nativeAt` for the
  exact vendor boundary the reducer proved; the server independently validates
  it against the journal.
  `forked {native:false}` clears copied source-native ids and stale live work:
  those rows are display history in the fresh destination, not actionable
  rewind points or running prompts/tasks.
- **Runes discipline.** Mutate `$state` only inside the store's methods; give
  every timer/listener an `$effect` teardown (a stray debounce firing after
  unmount is a bug); an `$effect` that both reads and writes the same `$state`
  loops.
- **UI quality is an acceptance criterion.** Use the theme tokens (`--fg`,
  `--accent`, `--edge`, `--overlay-bg`, …) — no hard-coded colors — so light and
  dark both hold. Shared chrome (buttons, popovers, card headers, entrance
  animations) should be shared, not re-pasted per card.

## Adding to the chat UI

- New event kind from a driver? Add its case to `ChatStore.apply` and render it
  from `blocks`; keep the wire type in sync with `chimaera-agent/model.rs`.
- New agent command (a button that tells the agent something)? `socket.send({
  type: "…", … })` and add the matching `AgentCommand` variant server-side.
- Keep `ChatView.svelte` from growing without bound. The overlays/panels (header,
  rewind dialog, `/mcp`, usage, effort) are already their own components — add new
  chrome the same way rather than inlining it into the host.
