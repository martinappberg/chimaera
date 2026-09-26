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
| `store.svelte.ts` | `ChatStore` — the reducer + all reactive view state (`blocks`, `pending`, `pendingSends`, `questions`, model/mode, activity, exited/degraded/connected/fatalError — the last cleared by a fresh `init`, a `forked` marker, or a journal reset; a SOCKET-origin fatal (a handshake failure via `onFatalError`) also clears on the next successful `ready`, while a journal `error{fatal}` outlives reconnects until the driver is genuinely relaunched), including initial replay hydration through the ready-frame `head`. **The single source of truth for the view.** Every block carries a monotonic per-store `uid` (the transcript's keyed-render key — never an array index). `blocks` is capped at ~2000 with hysteresis: it runs one 64-block slack past the cap, then one batch splice trims back to the cap behind a single "earlier history trimmed" notice (so the O(n) index rebuild runs once per batch, not per event at cap); `trimmedCount` counts the NET front shift (dropped − the replacing notice), making a block's virtual index (`trimmedCount + i`) invariant and `virtualTotal` (`blocks.length + trimmedCount`) monotonic at cap. `structuralVersion` counts insertions/removals (net lengths are a false proxy — a retracted-then-reappended tail cancels out) and `epoch` stamps the transcript generation (a journal reset restarts the trim numbering). `activeAgents` is the reducer-maintained live-subagents set (same proxies as `blocks`, so tray rows update in place — no per-event full-blocks filter), and `tool_output_delta` accumulation is capped client-side (12 KiB head + rolling 4 KiB tail behind the server's own "[N bytes omitted]" marker; the authoritative result replaces it). Its reducer has a vitest test (`store.svelte.test.ts`) — the one place the UI is unit-tested. |
| `chatWs.ts` / `cooperativeQueue.ts` | `ChatSocket` — connect/auth/reconnect(backoff)/gap-replay, then dispatch replay/live/control frames through one order-preserving cooperative queue so a cold history cannot starve browser input. Per-command refusals (`command_failed` / `invalid_command`) are visible but nonfatal. Shares reconnect accounting with `../terminal/ws.ts`. |
| `chatPool.ts` | Session-keyed warm reducer/socket + scroll/render-window/followed-revision cursor. The agent keeps folding while a tab's bounded DOM snapshot is hidden or its view is evicted; client-pool eviction never stops the daemon-owned process. |
| `ChatView.svelte` | The host: renders a bottom-anchored transcript window (64 blocks initially, 192 maximum) that pages automatically in both directions — scroll-driven prefetch mounts the next page about two viewports ahead in the reader's direction of travel (one page per frame), sentinels cover windows that end inside the viewport, and fallback buttons appear only without IntersectionObserver — plus a direct jump to newest. A **history spacer** ahead of the column stands in for the unmounted earlier history (sized by `heightModel.ts`) and absorbs every above-viewport height change for a scrolled-up reader (see the scroll invariant below); a scrollbar drag deep into it mounts the page the model puts there (`pageAround`). It hangs the header/composer/overlays/panels off itself. Non-tool rows are keyed `b-${block.uid}` and tool groups `g-${firstTool.id}` — **stable identities, never array indices**, so an at-cap trim's front-splice cannot remount the whole window (one caveat: a group whose FIRST tool is trimmed away changes key and remounts). "New rows vs in-place chunk" detection keys on the store's `structuralVersion` (never net lengths), a reducer trim shifts the view's absolute range AND its rendered slice by the trim delta (`trimShift`) so range, rows, and index labels keep agreeing — falling back to the tail when the whole window was trimmed — and cursors/ranges are discarded, never shifted, across a store `epoch` change. Re-activating a hidden tab whose window+content are unchanged since its freeze skips the range rebuild entirely (the frozen rows rebind to live proxies on the next event or bottom-reach). Visible tail rows are reducer proxies; hidden/history rows are one inert snapshot. A fresh replay stays gated until `head`, so it never paints oldest-to-newest. Still the big one — keep new chrome in child components, not inline. |
| `transcriptWindow.ts` | Pure range math for the 64-block/192-block sliding transcript DOM window — array coordinates throughout; saved cursors alone persist in trim-stable virtual coordinates, converted back at the boundary by `restoreVirtualWindow` (the one stale-cursor policy, with a one-page floor) while `trimShift` keeps a mounted range aligned across a trim. Also the scroll policy: `prefetchPage` (direction-gated, so a short window cannot ping-pong), `pageAround` (a far jump's page), and the spacer's `spacerTarget` / `spacerNeedsRebalance` with the WebKit rationale. Tests cover both paging directions, stale cursor repair, both trim conversions, prefetch, and the spacer policy. |
| `readingAnchor.ts` | The reading anchor: the top-level row at the viewport's top edge and its transform-free (`offsetTop`) position in the column; `measureShift` re-finds it by node, uid, then source-index range after a re-render. DOM-only, no state. |
| `heightModel.ts` | Content-based height model for unmounted blocks (kind + wrapped text length, tool runs on one line), in relative units the view calibrates against the mounted window; `HistoryWeights` keeps incremental prefix sums (rebuilt per epoch/trim/measure) and maps a spacer position back to a block. Own vitest suite. |
| `ChatHeader.svelte` | The header row: model / mode / effort pickers, usage + `/mcp` entry, the Remote Control chip + popover (state dot, open-on-claude.ai / copy link / on-off; reads `store.remoteControl` + `remoteControlAvailable`, sends `set_remote_control` through the host), session identity (always names which agent — Claude or Codex). |
| `EffortPopover.svelte` | The reasoning-effort ladder picker (uses the agent-native vocabulary verbatim — never relabel `xhigh`). |
| `Composer.svelte` / `composer.ts` | Input chrome plus the pure slash-context, argument-completion, and Codex skill-block helpers (covered by `composer.test.ts`). Slash discovery is whitespace-boundary aware; path fragments must stay ordinary text. |
| `Markdown.svelte` / `MathText.svelte` / `math.ts` | Render agent prose and plain user-message LaTeX (`$`/`$$` and Codex's `\(`/`\[` forms) as KaTeX MathML under one bounded policy — the policy itself (`mathOptions`/`renderMath`/`safeMathHtml`: KaTeX trust off → DOMPurify, memoized) lives in `../shared/math.ts` (a leaf the markdown file previews load on demand; `math.ts` re-exports it), while the CHAT delimiter dialect (`$` at word boundaries, Codex's `\(`/`\[`) stays here and is deliberately distinct from the previews' comrak-mirroring `previews/mdMath.ts`. **Sanitize untrusted/replayed content** (marked/KaTeX → DOMPurify, KaTeX trust off, `<style>` forbidden, external links `noopener`); Markdown also stamps validated file paths as clickable. **Local images are embeds**: inside this component's sanitize calls only (a flag around `DOMPurify.sanitize`; the hook is global), a schemeless `<img>` src moves to `data-md-embed` before the HTML reaches the DOM (never a request against the app's origin); `upgradeEmbeds` then mounts an EmbedCard (`mountEmbed`) in a slot it builds BESIDE the hidden placeholder — never replacing it, since a top-level node of the settled `{@html}` is what its teardown walks — on the settled render and on each closed segment after its word wrap (the open tail keeps a quiet placeholder box, so a card never churns per chunk). A slot is retired (card destroyed, slot removed) once its placeholder leaves the DOM; stamping, anchors and reveal spans skip `.md-embed`. **Streaming renders incrementally** (see the pipeline section below): closed segments parse once, only the open tail re-renders per chunk, and settle swaps in one canonical full parse. Post-render it also marks overflowing `.md-table` hosts and fence code boxes keyboard-reachable (`shared/scrollRegion.ts` — attribute writes only, never overwriting or stripping what the sanitized content brought; at the idle stamp pass, reveal completion and settle, never the open tail). |
| `tables.ts` / `markedExtensions.ts` | `tables.ts` hosts every GFM table in a `.md-table` scroll container at render time (why in the HTML string rather than a post-render DOM wrap is in the file); the CSS side is the "Markdown tables" recipe in `web-ui/src/app.css` (shared with the file preview's reading view and the live editor's table widget: scroller, rhythm, borders, alignment, numerals, and hosted cells keeping whole tokens — chat overrides three `--md-table-*` spacing tokens on its root), and chat's one delta — headers stay one line; a raw-HTML `<table>` has no host, so it keeps the root's squeeze-to-fit wrapping — lives in `Markdown.svelte`. The host emits no `tabindex`: whether it overflows is only known after layout, so `Markdown.svelte` marks it post-render. `markedExtensions.ts` is the ONE list of chat marked extensions, consumed by the component and by the parity pins in `streamSegments.test.ts`. Own pin: `tables.test.ts`. |
| `streamSegments.ts` | Pure INCREMENTAL segmentation of streaming markdown source at SAFE top-level blank-line boundaries — and the owner of the streaming security invariant (never more permissive than settle): fences and block math never split (mirrored against math.ts exactly), an HTML-block-opening line bails the message, lists (incl. lazy continuations) and indentation refuse, reference definitions make boundaries sticky until a ~16 KiB tail cap. Lossless partition (segments concatenate back to the source, byte for byte) with prefix-cache invalidation when a rewrite doesn't extend the prior text. Own vitest suite (`streamSegments.test.ts`) incl. hostile-HTML and marked-equivalence pins. |
| `revealLedger.ts` | Pure reveal-cursor arithmetic for the streaming pipeline (close-segment carry, tail rebuilds that never re-hide, prefix-first ticker takes) — the DOM queues in `Markdown.svelte` mirror it entry-for-entry. Own vitest suite (`revealLedger.test.ts`), incl. the duplicate-tail order contract. |
| `ToolCallCard` / `ToolGroup` / `toolLabels.ts` | Tool-call rendering (title, status, diff/output, grouping). A collapsed group is one quiet activity line titled by `toolLabels.ts` (pure, own vitest suite): the agent's batch labels (`tool_summary`, set on each row as `summary`), else readable past/present-tense counts, a lone agent named. Terminal rows may accept late output text but must never revive their streaming cursor. |
| `ActivityFold.svelte` / `activityFold.ts` / `ActivitySummary.svelte` / `ActivityRows.svelte` | Settled activity folds: `foldSpans` (pure, own vitest suite) marks each run of ≥2 thought/tool-group rows that a reply or a finished-work line directly follows; ChatView renders it as one `ActivityFold` line titled by `foldTitle` ("Thought, ran 6 commands, read 2 files"), keyed by the row that settled it, whose rows mount only while open and render through the same `activityRow` snippet as the live column. Finished lines never fold (results, and a woken turn's only stated cause). `ActivitySummary` is the one-line disclosure (label, live dot, failed/recovered badge, chevron) and `ActivityRows` the expanded body that the fold and `ToolGroup` share; `toolRunHealth` / `isLive` (`toolLabels.ts`) compute the badge and the dot. |
| `FinishedRow.svelte` | The `finished` block: a subagent's end (`subagent_finished`, report on click), a background command's or monitor's close (the CLI's own sentence + output link). |
| `AgentsTray.svelte` / `BackgroundTray.svelte` / `backgroundKinds.ts` | Two of the three pinned strips above the composer: live subagents (derived from in-flight Agent tool rows; a lone agent is named with its step) and live background tasks (the `background_tasks` level-set; the header names a Monitor watch and counts the rest per kind via `backgroundKinds.ts`, so it doubles as the between-turns "still waiting" signal), each with a stop affordance. Chrome lives in the shared `../shared/WorkTray.svelte` + `WorkTrayRow.svelte` shell; elapsed/duration text uses `../shared/time.ts`. The **plan strip** is the third, rendered inline in `ChatView` on the same `WorkTray` shell (`pulse` off unless a step is in flight) — three orthogonal readings of the same session: what the agent *means* to do (plan), *who* is working (subagents), what is *detached* (background). |
| `PermissionCard` / `QuestionCard` | The permission prompt and structured-question cards (their answers ride `socket.send`; `PermissionCard` also carries the deny-with-feedback field; `QuestionCard` presents Codex auto-resolution deadlines without owning the authoritative timeout). |
| `PlanApprovalCard.svelte` | Claude `ExitPlanMode` plan-approval card — renders the sanitized plan markdown + the three official options (auto-accept / manual / keep-planning) with an optional comment that rides the permission reply. |
| `RewindDialog.svelte` | The destructive in-place rewind/fork-point confirmation overlay (claude rewind + codex `thread/rollback`). |
| `ForkDialog.svelte` | The non-destructive conversation-branch picker: target agent plus native-vs-portable boundary disclosure. |
| `AgentMessageMeta.svelte` | The hover/focus rail below assistant prose: localized journal-backed timestamp, full-message copy, and the conversation-fork affordance. Its pure time ladder lives in `../shared/time.ts`. |
| `McpPanel.svelte` / `UsagePanel.svelte` | The `/mcp` linked-server panel and the token-usage panel. |
| `ArtifactGallery` / `artifacts.ts` / `embeds.ts` | Files in the transcript are `../shared/embed/EmbedCard` cards (one design with the documents; loads near the viewport, box reserved up front, fresh on overwrite, missing/error states). **The "written this turn" gallery** (a `turn_end` block, also pushed on an aborted turn that wrote something) shows the files the turn's edit tools wrote (`artifacts`, the tools' absolute locations, artifact kinds only — figures, reports, documents, tables, notebooks, slides, media; never source code) plus shell-written ones, **minus what the prose already showed** (`proseEmbedTargets` / `proseCovered`: an embed covers any shape, a prose name covers a document — the shallowest match only — and a named-not-embedded figure still tiles; `covered` lists them: the heading becomes "Also written" and `chipLabels` widens names against them too), under one header, split by `artifactShape`: **visuals** (figures, HTML, PDF, media) are tiles; **documents** (markdown, docx/pptx, tables, notebooks) are chips on one line (the header moves inline when there are no tiles; click opens, "+n more" past six, a "preview" fold shows their tiles; `chipLabels` widens colliding names with their folders; `fileStateAfter` + the disk monitor mark a chip gone or changed-since while on screen). Replay and live agree because all of it runs in the reducer. Shell-written ones: the reducer lists the artifact-shaped paths the turn's commands (the execute row's additive `command` — the whole text, since the title truncates at ~120 chars — falling back to the title) and outputs mention (`mentioned`, `artifacts.ts` `artifactMentions`, head+tail scanned, capped), and the gallery keeps those `fs/resolve_targets` confirms exist AND were modified between the turn's journal-stamped `startedAtMs`/`endedAtMs` (`writtenDuring`, 3 s slack — daemon clock on both sides). `EmbedResolver` (one per ChatView) resolves chat targets against the same base ladder as path links, but strictly (an embed names one file), batched and briefly cached. Own vitest suite (`artifacts.test.ts`; the reducer side in `store.svelte.test.ts`). |
| `UserText.svelte` | User-message bubble: plain text (never Markdown), validated path/mention affordances, recognized LaTeX spans delegated to `MathText`. |
| `paths.ts` | The chat half of path links: which candidates a code span / link target offers (parsing is `../shared/fileRef.ts`, shared with the terminal), and `PathResolver` — one per ChatView, batching every renderer's candidates into `fsValidate` calls grouped by base ladder (live cwd, spawn cwd, workspace root from App's `setChatLinkContext`), caching hits/ambiguous/misses keyed by candidate + base ladder + workspace (`resolveScope`; misses expire after 15 s and at every turn end, hits after 60 s; failures are never cached). A click re-checks before opening (`resolveNow` / `reopenResolution`), so a stale hit never opens a moved or deleted file. Opening goes through `../shared/openPath.ts` (reveal at the line, Cmd/Ctrl split); ambiguous names open a context-menu pick list. Own vitest suite (`paths.test.ts`). |
| `composerBus.ts` | Cross-component channel to insert text/attachments into the active composer (e.g. `@term:` grants, references, dropped-file paths). |
| `composerHeight.ts` | Pure height policy for content-fit growth plus manual resize baselines; covered by `composerHeight.test.ts`. |
| `drafts.ts` | Per-session composer draft persistence (survives the per-session ChatView remount + a page reload) — text layers into sessionStorage, images stay in-memory; both bounded. It also publishes which drafts remain memory-only so an interface-build transition cannot silently reload over them. |
| `images.ts` | Pasted/dropped image → downscale + base64 encode into an `ImageAttachment` (the canonical home of that type); size-bounded. |

The transcript's copy affordances — fenced code blocks, blockquotes, and whole
assistant messages — reuse `../shared/clipboard.ts` (the native-first clipboard
writer lifted out of the terminal pool) — see the shared/ area. Selection-copy
depends on settled prose being plain text nodes: the settle swap to the
canonical parse (below) renders span-free, because span-fragmented text copies
with a hard newline at every visual wrap point.

## The streaming render pipeline (Markdown.svelte + streamSegments.ts)

Re-parsing + re-sanitizing + re-word-wrapping the WHOLE accumulated message on
every coalesced wire chunk (2 KiB / 100 ms) is O(n²) — a multi-thousand-word
reply burned tens of ms per chunk near its end. The live pipeline instead makes
per-chunk work proportional to the TRAILING OPEN SEGMENT, not the message:

- **The security invariant** (owned by `streamSegments.ts`): the streaming
  render must NEVER be more permissive than the settled render. Splitting
  inside a construct whose interior is inert when parsed whole (fenced code,
  block math, raw HTML) would hand that interior to marked as ordinary
  markdown — turning inert text into live DOM (`<img>` beacons, clickable
  links) that DOMPurify allows. So fences and block math are tracked and never
  split, and any line that can OPEN a CommonMark HTML block bails segmentation
  for the whole message. Nothing ever force-closes a fence/math/HTML tail.
- **Segmentation** (`streamSegments.ts`, pure, incremental): the source splits
  at safe top-level blank-line boundaries; the scanner persists its cursor +
  construct state, so each advance walks only the new lines. "Safe" is
  conservative — open fences and `$$`/`\[` block math never split (open/close
  lines mirror math.ts EXACTLY, pinned by tests, and `\r\n` is normalized the
  way marked's lexer does); a list refuses to close until a flush non-list
  block starts (loose continuations AND lazy-continuation lines); indented
  tails refuse; a reference-link definition makes boundaries sticky for the
  message (document-global targets), relaxed only past a ~16 KiB tail cap and
  even then only at the ordinary safe boundaries. The partition is lossless —
  closed segments + the open tail concatenate back to the source
  byte-for-byte, so the reducer's materialized `\n\n` block separators
  (PR #122) can't be eaten or doubled. Residual worst case, accepted: one
  giant never-closing fence (or an HTML bail) keeps the whole tail open, so
  its parse cost stays O(open) per chunk by construction.
- **Per chunk**: closed segments were parsed + DOMPurify-sanitized + copy-
  decorated + local-anchor-classified + word-wrapped ONCE (each in its own
  `display: contents` wrapper, so layout matches the wrapper-free settled
  render); their DOM is never touched again. Only the open tail's wrapper
  re-renders. A parser throw falls back to inert plain text (never a dropped
  segment); a text update that does NOT extend the previous source (a
  retraction/reroute rewrite) invalidates the whole cached prefix and rebuilds
  from the fresh split.
- **Reveal** (`revealLedger.ts` owns the cursor arithmetic, pure + tested):
  word spans exist only for not-yet-revealed words. The 75 ms ticker drains a
  prefix-then-tail queue in document order; the cursor carries across tail
  rebuilds and into closing segments so shown words never re-hide or re-fade —
  and every advance that closed segments MUST rebuild the tail, even when its
  source is string-equal (the duplicate-paragraph trap; the tail memo is
  invalidated on closes). A drained closed segment dissolves its spans shortly
  after the fade (span-fragmented text copies with hard newlines at wrap
  points, and closed-segment DOM now survives long enough to be selected).
  Blocks whose first word is unrevealed hide whole (probe spans). Reduced
  motion skips spans and the ticker entirely — content lands instantly, still
  incrementally.
- **Hidden ≠ settled.** `streaming` is TURN state (this row is the streaming
  tail, compared by block uid), `visible` rides separately: hiding a tab
  mid-stream FREEZES the live segment DOM in place — no canonical-parse swap
  at tab-switch-away, no ticker — and thaw resumes with the reveal cursor
  intact (catch-up segments append; the already-read prefix never
  re-animates). The swap to the canonical parse happens only when the row
  genuinely stops streaming.
- **Deferred decorations**: `stampPaths` (TreeWalker + `shared/fileRef.ts`)
  runs at idle on closed segments — never on the per-chunk hot path — and its
  async resolve callback re-stamps only the affected root, again at idle (or
  the settled root, when the stream settled while the batch was in flight —
  the canonical swap disconnected the segment it walked). What a stamped
  element opens lives in a component `WeakMap`, never in DOM attributes
  (sanitized agent HTML can forge classes and `data-*`). On
  WKWebView (the native app) `requestIdleCallback` is absent and the fallback
  is a short fixed delay. The open tail is stamped when its segment closes or
  at settle — but its schemeless anchors get the `md-local` class
  SYNCHRONOUSLY at every render (a streamed relative link must be swallowed by
  the click handler from first paint, or it navigates the workbench away).
- **Settle = ONE canonical full re-parse.** When `streaming` flips false the
  template swaps to the memoized `{@html html}` whole-message parse (computed
  lazily — a live block never pays it per chunk) and full decorations run. The
  settled transcript is therefore identical to a never-streamed render BY
  CONSTRUCTION — any conservative-segmentation artifact is transient — and the
  settled DOM is span-free, which keeps selection-copy clean. (Tradeoff, also
  by construction: the settle swap replaces the subtree, so a selection held
  ACROSS the settle moment is dropped — strictly better than the old
  per-chunk whole-subtree rebuild, which dropped selections every 100 ms
  mid-stream.)
- **Safety rail**: EVERY fragment that reaches `innerHTML`/`{@html}` — each
  closed segment, each tail render, the plain-text fallback, the canonical
  parse — passes through the same DOMPurify config first. Unsanitized
  fragments are never concatenated. The non-streaming path (history rows,
  reading mode) is the same single memoized parse as before.

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
- **Scroll restoration is window-aware and has one writer.** Save the
  scroll offset **relative to the rendered rows** (spacer excluded — it is
  re-derived on remount), the bounded block range **in virtual coordinates**
  (array index + the store's `trimmedCount`, so a cap trim while parked can't
  strand the cursor), whether it still tracks the tail, and the transcript
  revision the reader followed. Stream/Markdown/content and transcript-viewport
  resize follow requests coalesce into one frame. Pinned-tray/composer height
  changes are inputs to that same writer, never independent scroll owners. A
  live tail continues rendering while the reader scrolls or types, but a
  non-empty draft pauses auto-follow. Hidden tabs snapshot once and must not
  retain reactive block proxies. Replay never remounts the entire transcript.
  A visible top sentinel while a short live tail fills the viewport is layout,
  not reader intent: it may prepend only while retaining the tail, and stops at
  the DOM cap instead of silently paging the reader away from live activity.
- **A pinned follower leaves the live edge only by its own hand.** In WebKit a
  scroll event lands a frame late (rows appended in between make a reader who
  never moved look scrolled up), and scrollTop is clamped wherever a re-render
  momentarily shrinks the content (an up-move nobody made). So `onScroll`
  keeps `atBottom` for a non-move, and — while a turn runs — for an up-move
  with no wheel/pointer/touch/key input behind it (WebKit dispatches those
  ahead of the scroll they cause; the wheel listener must stay passive). The
  live-tail chrome gates on `atLiveEdge`, which counts a tail window whose
  appended row is one flush from rendering as live: tearing the status row out
  and back in per append was one of those shrinks. Repro: the real-WebKit
  harness in `scripts/perf/transcript-scroll/` with a page script that sends
  and samples the gap.
- **Never write `scrollTop` while a gesture may be in flight.** WebKit (the
  native app) has no scroll anchoring, and its scrolling thread owns the
  position during a fling: a mid-gesture `scrollTop` write — any correction for
  rows mounted above the reader — snaps back for a frame or two (measured with
  real momentum wheel events; see field notes 2026-09-25). A scrolled-up
  reader's anchor row is held by resizing the **history spacer** instead: page
  writes, trims, fold regrouping, and previews decoding above them (the column
  ResizeObserver runs before paint) all shift the spacer by exactly the
  anchor's movement. The spacer may go negative when the model underestimates
  (rows pulled past the scroll origin, like the old rendered edge). Scroll
  writes are reserved for the bottom-follow writer, a restore, and the idle
  rebalance (no scroll event for 160 ms), which re-sizes the spacer to the
  model with one compensating write. The transcript sets
  `overflow-anchor: none` so Chromium doesn't correct the same shift twice.
  Verify changes here with the real-wheel harness in
  `scripts/perf/transcript-scroll/` — Chromium, jsdom and the Browser pane
  hide the WebKit behavior.
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
- **Prose leads; activity lines follow.** Thought, tool-group, finished and wake rows share one
  quieter voice (12px, the column's `--activity-fg`, an `activity` class that ChatView clusters
  tight) so agent messages stay the page's voice. A settled message's hover rail flows inline after
  its last word (the `.md` shell is `display: contents`, a closing `<p>` goes inline — nothing
  measures `.md`, keep it that way). New transcript chrome joins that family rather than adding a new visual
  weight. A run of thought/tool lines that a reply (or a finished line) has followed folds into one
  line (`activityFold.ts`); the trailing run stays unfolded, so live work is always visible. Wake markers are reducer-derived (`markWake`, pure over `blocks` + the live background
  set), so replay rebuilds them; the live status line reads `turnTokens` / `activityLine`.
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
