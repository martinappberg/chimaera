# web-ui/src/lib/chat — the structured chat surface

Orientation for coding agents. This directory is the **front half of chat mode**:
the rich UI that renders Claude, Codex, Antigravity and Grok agent streams,
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
| `chatWs.ts` / `cooperativeQueue.ts` | `ChatSocket` — connect/auth/reconnect(backoff)/gap-replay, then dispatch replay/live/control frames through one order-preserving cooperative queue so a cold history cannot starve browser input. Per-command refusals (`command_failed` / `invalid_command` / `read_only`) are visible but nonfatal and carry the refused command's `client_id`; a `ready` reports `send_ids` and whether it is a reattach (`ReadyAttach`), and `send_cancelled` answers the store's `cancel_send`. Shares reconnect accounting with `../terminal/ws.ts`. |
| `chatPool.ts` | Session-keyed warm reducer/socket + scroll/render-window/followed-revision cursor. The agent keeps folding while a tab's bounded DOM snapshot is hidden or its view is evicted; client-pool eviction never stops the daemon-owned process. |
| `ChatView.svelte` | The host: renders a bottom-anchored transcript window (64 blocks initially, 192 maximum) that pages automatically in both directions — scroll-driven prefetch mounts the next page about two viewports ahead in the reader's direction of travel (one page per frame), sentinels cover windows that end inside the viewport, and fallback buttons appear only without IntersectionObserver — plus a direct jump to newest. A **history spacer** ahead of the column stands in for the unmounted earlier history (sized by `heightModel.ts`, calibrated by what the pages mounted above the reader really measured) and absorbs every above-viewport height change for a scrolled-up reader (see the scroll invariant below); its twin, the **later spacer** after the column, stands in for the unmounted later rows of a history page and takes every scroll-height change a range write makes below the reader, so the scrollbar thumb moves only when the reader scrolls (zero at the live edge, forced; never below the model of the rows still unmounted, or an undershooting estimate ends the scroll range early and the last page moves the end away). A scrollbar drag deep into either mounts the page the model puts there (`pageAround`; a drag to the very end lands on the live edge). A page write never discards rows within one viewport past the prefetch reach (`keepInReach`): the cap counts blocks, and a folded tool run is one short line. Once the window holds the first row, the history spacer's leftover (blank above the first message) shrinks as fast as the reader scrolls into it (`holdTopEdge`). It hangs the header/composer/overlays/panels off itself. Non-tool rows are keyed `b-${block.uid}` and tool groups `g-${firstTool.id}` — **stable identities, never array indices**, so an at-cap trim's front-splice cannot remount the whole window (one caveat: a group whose FIRST tool is trimmed away changes key and remounts). "New rows vs in-place chunk" detection keys on the store's `structuralVersion` (never net lengths), a reducer trim shifts the view's absolute range AND its rendered slice by the trim delta (`trimShift`) so range, rows, and index labels keep agreeing — falling back to the tail when the whole window was trimmed — and cursors/ranges are discarded, never shifted, across a store `epoch` change. Re-activating a hidden tab whose window+content are unchanged since its freeze skips the range rebuild entirely (the frozen rows rebind to live proxies on the next event or bottom-reach). Visible tail rows are reducer proxies; hidden/history rows are one inert snapshot. A fresh replay stays gated until `head`, so it never paints oldest-to-newest. Still the big one — keep new chrome in child components, not inline. |
| `ChatFind.svelte` / `chatFind.ts` | Pane-scoped Find in retained conversation messages (user/assistant/agent messages; not tools/thoughts). Bounded to 500 matching messages, navigated by stable uid through ChatView’s existing transcript window; no full-history DOM mount. Shared controls and non-mutating range highlights live in `../shared/`. |
| `transcriptWindow.ts` | Pure range math for the 64-block/192-block sliding transcript DOM window — array coordinates throughout; saved cursors alone persist in trim-stable virtual coordinates, converted back at the boundary by `restoreVirtualWindow` (the one stale-cursor policy, with a one-page floor) while `trimShift` keeps a mounted range aligned across a trim. Also the scroll policy: `prefetchPage` (direction-gated, so a short window cannot ping-pong), `keepInReach` (a page write keeps the rows near the reader — the 192-block cap alone unmounted the row being read on tool-heavy history, or pulled the discarded edge within reach so its sentinel paged it straight back, every frame), `pageAround` (a far jump's page), and the spacer's `spacerTarget` / `spacerNeedsRebalance` with the WebKit rationale. Tests cover both paging directions, stale cursor repair, both trim conversions, prefetch, and the spacer policy. |
| `readingAnchor.ts` | The reading anchor: the top-level row at the viewport's top edge AND the element inside it at that edge (a figure resolving above the paragraph being read, within the same multi-thousand-px reply, moves the text while the row's top stays put) — never a card straddling the edge, which resizes after mounting, but the first element after it that starts in view (else the next row) — with transform-free (`offsetTop`) positions in the column; `measureShift` measures the edge element while the row still holds it, else re-finds the row by node, uid, then source-index range. `rowsInReach` is the source range of the rows near the viewport (`keepInReach`). DOM-only, no state. |
| `heightModel.ts` | Content-based height model for unmounted blocks (kind + rendered text length — link targets and markup never show — a settled run of thoughts and tool calls on the one line its fold renders, the live tail's unfolded run a line each, and inline embeds as the cards they render), in relative units the view calibrates against the pages it has measured; `HistoryWeights` keeps incremental prefix sums (rebuilt per epoch/trim/measure) and maps a spacer position back to a block. Own vitest suite. |
| `ChatHeader.svelte` | The responsive header: model / mode / effort selectors, session/status row below 480px, and Chat options for thinking / ultracode / Remote Control + its popover (state dot, open-on-claude.ai / copy link / on-off; reads `store.remoteControl` + `remoteControlAvailable`, sends `set_remote_control` through the host), session identity (always names which agent — Claude or Codex). |
| `EffortPopover.svelte` | The reasoning-effort scale (uses the agent-native vocabulary verbatim — never relabel `xhigh`). Connected dots retain full-size labeled click targets; `../shared/toolbarPopover.ts` supplies top-layer placement, arrow-key navigation and focus return. |
| (the line above the input) | `ChatView` renders one quiet `.branch-line` just above the composer only when there is something to say: on the left the session's branch (`../shared/BranchChip.svelte`, only in a repository; opens "Changes on this branch"), on the right the same-file notice (`../workspace/SameFileNotice.svelte` — another live session wrote a file this one wrote; click opens it). It shares the composer and pinned trays' centered column and side insets; long labels truncate within the pane. The rail shows a branch only for an agent working in a separate worktree (`App.svelte`, `.wt-mark`); agents in the main checkout show nothing there (maintainer, 2026-09-29). |
| `Composer.svelte` / `composer.ts` | Input chrome plus the pure slash-context, argument-completion, and Codex skill-block helpers (covered by `composer.test.ts`). Autofocus yields to keyboard navigation in the pane tabs (`../shared/tabNavigation.ts`). Slash discovery is whitespace-boundary aware; path fragments must stay ordinary text. Enter sends (mid-turn: read at the agent's next step); the after-turn chord calls `onSubmit(…, afterTurn=true)` — see the queued-send invariant below. |
| `uploadTokens.ts` / `uploadChips.ts` / `ComposerMentions.svelte` | A dropped file's mention reads as its name in the draft, where it was referenced (`@/…/uploads/s-…/plot.png` → `@plot.png`): every text entering the composer (inserts, the loaded draft, a paste) is collapsed, every text leaving it (the send, a copy or cut, the saved draft) expanded, so the agent gets exactly the text a drop always typed; typed and picked mentions are never rewritten. `uploadTokens.ts` is pure (own vitest suite): recognition by text (so undo/redo/paste bring one back), `snapRange`, `keepsTokens` (would an edit glue text onto one?), `settleEdit` (the after-the-fact fix). `uploadChips.ts` is the textarea attachment that makes a short form one unit — caret snapping, arrow/Backspace/Delete over it, a space before text typed or pasted against its end, the gluing space-delete refused — every change through `execCommand` so native undo stays whole (never `execCommand("undo")`: WebKit merges typing runs into one undo group). `ComposerMentions` paints every `@` mention as a pill UNDER the textarea (a mirror that copies the field's computed padding/type and follows its scroll — the textarea keeps glyphs, caret, IME, undo) and shows an upload pill's landed path on hover (hidden while a completion popover is open). |
| `Markdown.svelte` / `MathText.svelte` / `math.ts` | Render agent prose and plain user-message LaTeX (`$`/`$$` and Codex's `\(`/`\[` forms) as KaTeX MathML under one bounded policy — the policy itself (`mathOptions`/`renderMath`/`safeMathHtml`: KaTeX trust off → DOMPurify, memoized) lives in `../shared/math.ts` (a leaf the markdown file previews load on demand; `math.ts` re-exports it), while the CHAT delimiter dialect (`$` at word boundaries, Codex's `\(`/`\[`) stays here and is deliberately distinct from the previews' comrak-mirroring `previews/mdMath.ts`. **Sanitize untrusted/replayed content** (marked/KaTeX → DOMPurify, KaTeX trust off, `<style>` forbidden, external links `noopener`); Markdown also stamps validated file paths as clickable. **Local images are embeds**: inside this component's sanitize calls only (a flag around `DOMPurify.sanitize`; the hook is global), a schemeless `<img>` src moves to `data-md-embed` before the HTML reaches the DOM (never a request against the app's origin); `upgradeEmbeds` then mounts an EmbedCard (`mountEmbed`) — or, for a document (`artifacts.ts` `embedsAsChip`: markdown unless `#slide=`, docx, pptx), a `ProseChip` in an inline slot — in a slot it builds BESIDE the hidden placeholder — never replacing it, since a top-level node of the settled `{@html}` is what its teardown walks — on the settled render and on each closed segment after its word wrap (the open tail keeps a quiet placeholder box, so a card never churns per chunk). A slot is retired (card destroyed, slot removed) once its placeholder leaves the DOM; stamping, anchors and reveal spans skip `.md-embed`. **Streaming renders incrementally** (see the pipeline section below): closed segments parse once, only the open tail re-renders per chunk, and settle swaps in one canonical full parse. Post-render it also marks overflowing `.md-table` hosts and fence code boxes keyboard-reachable (`shared/scrollRegion.ts` — attribute writes only, never overwriting or stripping what the sanitized content brought; at the idle stamp pass, reveal completion and settle, never the open tail). |
| `tables.ts` / `markedExtensions.ts` | `tables.ts` hosts every GFM table in a `.md-table` scroll container at render time (why in the HTML string rather than a post-render DOM wrap is in the file); the CSS side is the "Markdown tables" recipe in `web-ui/src/app.css` (shared with the file preview's reading view and the live editor's table widget: scroller, rhythm, borders, alignment, numerals, and hosted cells keeping whole tokens — chat overrides three `--md-table-*` spacing tokens on its root), and chat's one delta — headers stay one line; a raw-HTML `<table>` has no host, so it keeps the root's squeeze-to-fit wrapping — lives in `Markdown.svelte`. The host emits no `tabindex`: whether it overflows is only known after layout, so `Markdown.svelte` marks it post-render. `markedExtensions.ts` is the ONE list of chat marked extensions, consumed by the component and by the parity pins in `streamSegments.test.ts`. Own pin: `tables.test.ts`. |
| `streamSegments.ts` | Pure INCREMENTAL segmentation of streaming markdown source at SAFE top-level blank-line boundaries — and the owner of the streaming security invariant (never more permissive than settle): fences and block math never split (mirrored against math.ts exactly), an HTML-block-opening line bails the message, lists (incl. lazy continuations) and indentation refuse, reference definitions make boundaries sticky until a ~16 KiB tail cap. Lossless partition (segments concatenate back to the source, byte for byte) with prefix-cache invalidation when a rewrite doesn't extend the prior text. Own vitest suite (`streamSegments.test.ts`) incl. hostile-HTML and marked-equivalence pins. |
| `revealLedger.ts` | Pure reveal-cursor arithmetic for the streaming pipeline (close-segment carry, tail rebuilds that never re-hide, prefix-first ticker takes) — the DOM queues in `Markdown.svelte` mirror it entry-for-entry. Own vitest suite (`revealLedger.test.ts`), incl. the duplicate-tail order contract. |
| `ToolCallCard` / `ToolGroup` / `toolLabels.ts` | Tool-call rendering (title, status, diff/output, grouping). A collapsed group is one quiet activity line titled by `toolLabels.ts` (pure, own vitest suite): the agent's batch labels (`tool_summary`, set on each row as `summary`), else readable past/present-tense counts, a lone agent named. The chimaera MCP server's agent-communication calls read in words on the card and in the count — "Message to fix CI", "Checked messages", "Listed agents", "Read fix CI's work" (`commsCall` recognizes claude's `message_agent (chimaera)`, codex's `chimaera.message_agent` / `chimaera · message_agent`, the raw `mcp__chimaera__…`; a target shows once the title carries one, `→ fix CI`), the driver's title on hover. Terminal rows may accept late output text but must never revive their streaming cursor. |
| `ActivityFold.svelte` / `activityFold.ts` / `ActivitySummary.svelte` / `ActivityRows.svelte` | Settled activity folds: `foldSpans` (pure, own vitest suite) marks each run of ≥2 thought/tool-group rows that a reply or a finished-work line directly follows; ChatView renders it as one `ActivityFold` line titled by `foldTitle` ("Thought, ran 6 commands, read 2 files"), keyed by the row that settled it, whose rows mount only while open and render through the same `activityRow` snippet as the live column. Finished lines never fold (results, and a woken turn's only stated cause). `ActivitySummary` is the one-line disclosure (label, live dot, failed/recovered badge, chevron) and `ActivityRows` the expanded body that the fold and `ToolGroup` share; `toolRunHealth` / `isLive` (`toolLabels.ts`) compute the badge and the dot — a failure's recovery looks past its own group into the rest of the turn (`TurnTail`: ChatView hands each group and fold one shared per-turn call array plus its offset, so a retry after a thought row still counts; the tail ends at the mounted window's edge). |
| `AgentMessageCards.svelte` / `agentMessages.ts` | Messages from other agents in the workspace (agent communication, [plan §12](../../../../docs/agent-communication-plan.md)): the `agent_message` block — a card per message with the sender's name beside its vendor mark (`SessionGlyph`; a vendor word with no mark — the daemon's "agent" — shows none rather than a wrong one), `#id`, "to you"/"to everyone", "re #N", a quiet accent edge for the Mastermind's direction, the body through `Markdown` (the plain card, when no header parsed, through `UserText`), and a send's leading why-line as a caption. Never a fork/rewind point. The pending tail reuses it for a Codex steer that hasn't been read ("next step") or missed its turn ("not delivered — it's in their inbox", ✕ dismisses). `agentMessages.ts` (pure, own vitest suite) parses the daemon's header format: a header is a line that starts `[message #` unquoted, so a peer's `> `-quoted body can't forge a second card; peer bodies lose their `> `, the Mastermind's stay verbatim (the daemon escapes a header-shaped line in it with `\`, which the parser removes). |
| `ThoughtRow.svelte` / `thoughtText.ts` | A reasoning line: "Thought"/"Thinking" + `thoughtPreview` (pure, own vitest suite) — the newest Codex `**section title**` (live or settled, so the row never jumps), else the first line, as plain text with code spans intact; the body renders through `Markdown.svelte` with the chat's prose wiring and mounts only while the row is open. |
| `TransferNote.svelte` / `transfer.ts` / `KeptNote.svelte` | The quiet one-line dividers where the conversation changed machines (see the transfer paragraph below); `KeptNote` is the "Back on this Mac … both changed N files" line with its **Review** action. |
| `FinishedRow.svelte` | The `finished` block: a subagent's end (`subagent_finished`, report on click), a background command's or monitor's close (the CLI's own sentence + output link). |
| `AgentsTray.svelte` / `BackgroundTray.svelte` / `backgroundKinds.ts` | Two of the three pinned strips above the composer: live subagents (derived from in-flight Agent tool rows; a lone agent is named with its step) and live background tasks (the `background_tasks` level-set; the header names a Monitor watch and counts the rest per kind via `backgroundKinds.ts`, so it doubles as the between-turns "still waiting" signal), each with a stop affordance. Chrome lives in the shared `../shared/WorkTray.svelte` + `WorkTrayRow.svelte` shell; elapsed/duration text uses `../shared/time.ts`. The **plan strip** is the third, rendered inline in `ChatView` on the same `WorkTray` shell (`pulse` off unless a step is in flight) — three orthogonal readings of the same session: what the agent *means* to do (plan), *who* is working (subagents), what is *detached* (background). |
| `PermissionCard` / `QuestionCard` | The permission prompt and structured-question cards (their answers ride `socket.send`; `PermissionCard` also carries the deny-with-feedback field; `QuestionCard` presents Codex auto-resolution deadlines without owning the authoritative timeout). |
| `PlanApprovalCard.svelte` | Claude `ExitPlanMode` plan-approval card — renders the sanitized plan markdown + the three official options (auto-accept / manual / keep-planning) with an optional comment that rides the permission reply. |
| `RewindDialog.svelte` | The destructive in-place rewind/fork-point confirmation overlay (claude rewind + codex `thread/rollback`). |
| `ForkDialog.svelte` | The non-destructive conversation-branch picker: target agent plus native-vs-portable boundary disclosure. |
| `AgentMessageMeta.svelte` | The hover/focus rail below assistant prose: localized journal-backed timestamp, full-message copy, and the conversation-fork affordance. Its pure time ladder lives in `../shared/time.ts`. |
| `McpPanel.svelte` / `UsagePanel.svelte` | The `/mcp` linked-server panel and the token-usage panel. |
| `ArtifactGallery` / `artifacts.ts` / `embeds.ts` | Prose embeds in the transcript are `../shared/embed/EmbedCard` cards (one design with the documents; loads near the viewport, box reserved up front, fresh on overwrite, missing/error states). **The "written this turn" gallery** (a `turn_end` block, also pushed on an aborted turn that wrote something) shows the files the turn's edit tools wrote (`artifacts`, the tools' absolute locations, artifact kinds only — figures, reports, documents, tables, notebooks, slides, media; never source code) plus shell-written ones, **minus what the prose already showed** (`proseEmbedTargets` / `proseCovered`: an embed or a prose name covers any shape — a name's path link previews on a rest as a chip would — the shallowest match only; `covered` lists them: the heading becomes "Also written" and `chipLabels` widens names against them too), as **one chip line** after the header: every file — figures, HTML, PDF, media and documents alike — is a `FileChip` in written order (never tiles: a turn that saved twenty plots must still cost a line or two); click opens, a rest previews that one file (`hoverTargets.ts`; there is no preview fold — unfolding every tile at once was the clutter); past seven, six show, one of each view kind before a second of any (`artifacts.ts` `foldedChips`), behind a "+n more"/"fewer" toggle; `chipLabels` widens colliding names with their folders; `fileStateAfter` + the disk monitor mark a chip gone or changed-since while on screen, the latter shown in its preview. Replay and live agree because all of it runs in the reducer. Shell-written ones: the reducer lists the artifact-shaped paths the turn's commands (the execute row's additive `command` — the whole text, since the title truncates at ~120 chars — falling back to the title) and outputs mention (`mentioned`, `artifacts.ts` `artifactMentions`, head+tail scanned, capped), and the gallery keeps those `fs/resolve_targets` confirms exist AND were modified between the turn's journal-stamped `startedAtMs`/`endedAtMs` (`writtenDuring`, 3 s slack — daemon clock on both sides). `EmbedResolver` (one per ChatView) resolves chat targets against the same base ladder as path links, but strictly (an embed names one file), batched and briefly cached; a card or gallery remounted by transcript paging takes a cached answer synchronously (`peek`, and `shared/embed/embed.ts` `peekFile` for absolute paths), so its box is final before its first paint instead of growing a tunnel round trip later — and a card mounted from `peekFile` re-asks once near (`resolveFile(…, {fresh: true})`, which never joins a request sent before it), since the file may have been rewritten since. Own vitest suite (`artifacts.test.ts`; the reducer side in `store.svelte.test.ts`). |
| `FileChip.svelte` / `ProseChip.svelte` | The file chip (icon + name, gone/not-found states), shared by the turn-end line (every written file) and prose document embeds; it registers its preview in the chat's `HoverTargets` while its file is known. `ProseChip` is a prose embed's chip: resolves through the `EmbedResolver`, asks again on a pointer arrival or click while not found. |
| `hoverTargets.ts` | What a chat element previews on hover (`HoverTargets`, one per ChatView, a `WeakMap` — sanitized agent HTML can forge classes and `data-*`, so only elements the chat built or stamped preview), answered to the shared hover controller (`../previews/doc/hoverController.svelte.ts`) as its `targetOf`. `ChatView` hosts that controller over the transcript (`root` = the scroller, `layer` = the chat, `anchors: false`, `standalone: true`); `Markdown.svelte` registers resolved FILE path links (never dirs or ambiguous names), `ArtifactGallery` its chips. Its `refs` (`ReferenceChips`, `../shared/references.ts`) hold the **id chips** `Markdown.svelte` makes on settled content (`linkReferences` after `stampPaths`; again when a reference source arrives): an id a source answers for (a knowledge entry's `F-228`) previews its lines and opens on click/Enter through the target's `open`. Own vitest suite. |
| `UserText.svelte` | User-message bubble: plain text (never Markdown); every `@` file mention reads as a chip in place (icon + name — `paths.ts::mentionChipLabel`, an upload by its landed name, other names widened by `artifacts.ts::chipLabels` only where two collide — clickable once resolved, muted on a confirmed miss, the whole mention in its tooltip and in a copy via `data-full`); a path written without `@` stays as written (a dotted link once validated); recognized LaTeX spans delegated to `MathText`; `>`-led quoted lines muted with their markers kept (`shared/reference.ts::quoteRuns`). |
| `AttachmentStrip.svelte` / `ImagePreview.svelte` | A message's images as picture tiles (one row height, width from the picture's aspect via `images.ts::tileBox`, no hover effects): `drafts` in the composer (in-memory pixels, ✕ to remove, click → `ImagePreview`, a fixed overlay like the plan card's) and `paths` on sent/queued bubbles (the daemon's saved copies from `user_message.attachment_paths`, resolved near the viewport through `resolveFile`, click → open in a pane, a gone copy a dashed tile). A picture-only message puts the strip where the bubble would be. |
| `paths.ts` | The chat half of path links: which candidates a code span / link target offers (parsing is `../shared/fileRef.ts`, shared with the terminal), and `PathResolver` — one per ChatView, batching every renderer's candidates into `fsValidate` calls grouped by base ladder (live cwd, spawn cwd, workspace root from App's `setChatLinkContext`), caching hits/ambiguous/misses keyed by candidate + base ladder + workspace (`resolveScope`; misses expire after 15 s and at every turn end, hits after 60 s; failures are never cached). A click re-checks before opening (`resolveNow` / `reopenResolution`), so a stale hit never opens a moved or deleted file. Opening goes through `../shared/openPath.ts` (reveal at the line, Cmd/Ctrl split); ambiguous names open a context-menu pick list. Own vitest suite (`paths.test.ts`). |
| `voice.svelte.ts` / `voiceCapture.ts` / `VoiceMeter.svelte` / `voiceLanguages.ts` | Voice dictation — the composer's mic button (on by default; `/voice on` or `off`; the same in Claude and Codex chats). `Dictation` (one per composer) is a chain of `Phrase`s — one `/ws/voice` socket each, opened with the mic (audio captured before a socket is up is queued, never dropped). `PauseDetector` (pure, tested) ends a phrase at a pause: the service revises nothing until a stream is finalized, so the recording finalizes each phrase there and speaks on into a fresh one — opened only when speech resumes, with ~300 ms of pre-roll, so a recording that ends in silence opens none — and each phrase's corrected text arrives a moment later. The host check (`hostCanDictate`) re-asks on the window's next focus/visibility whenever the answer was no. `finals` is the leading run of finished phrases, `interim` everything after (a finishing phrase keeps its guess until its correction lands); keeps the last five levels for `VoiceMeter`'s waveform (beside the stop button), and owns `error` (the composer only reads it) — including the silence message naming the device when the loudest chunk stayed near zero; a generation counter fences late events from an ended recording. `hostCanDictate` / `recheckHost` cache the daemon's `GET /api/v1/voice` per window (re-asked after a login error), gating the mic; `voiceProblem` is `/voice on`'s check (that, then one mic permission request). `voiceCapture.ts`: `listMicrophones` (names are withheld until the page has the mic once), `startCapture(onChunk, microphone)` resolving a remembered NAME to this origin's device id, and an AudioWorklet (inlined as a Blob URL) box-filtering the device rate to 16 kHz mono PCM16 in 100 ms chunks with an RMS level; the mic is released after each recording. Keys stay the composer's: the Dictate chord (`keys.dictate`, matched only while the composer has focus — App's handler has no case for it, so it falls through), and while recording Esc restores the draft and Enter stops and sends; Space is never taken over. The words stream INTO the draft (`dictationParts` / `joinParts`: the text around the caret, settled words, forming words, spaced like typing), so the box grows like typing; the textarea goes read-only with transparent text and the composer's `.ghost` mirror (exact box, font, wrapping, scroll and scrollbar-width padding) draws it with the spoken part dimmed — keep their box and font properties identical. Pure helpers (`dictationParts`, `insertDictation`, `joinSpoken`) have a vitest suite (`voice.test.ts`). |
| `composerBus.ts` | Cross-component channel to insert text/attachments into the active composer (e.g. `@term:` grants, references, dropped-file paths, a quoted transcript passage). An insert is `inline` (joins the draft after a space) or `block` (its own paragraph, so a quote's `>` starts a line); `composer.ts::draftWithInsert` is the pure join (its `above` is for the return channel). A message that did not arrive comes back through `registerComposerReturn` / `returnableCount` / `returnToComposer`: all or nothing (its pictures must fit), above the draft, and never taking focus; keyed by the mounting view's token like inserts, so a chat mounted twice keeps a target when one view unmounts. An insert may name the mounting view's token (`view`): one chat can be mounted twice (the Mastermind dock and a pane), and a quote belongs in the composer under its selection. Own vitest suite (`composerBus.test.ts`). |
| `composerHeight.ts` | Pure height policy for content-fit growth plus manual resize baselines; covered by `composerHeight.test.ts`. |
| `drafts.ts` | Per-session composer draft persistence (survives the per-session ChatView remount + a page reload) — text layers into sessionStorage, images stay in-memory; both bounded. It also publishes which drafts remain memory-only so an interface-build transition cannot silently reload over them. |
| `images.ts` | Pasted/dropped image → downscale + base64 encode into an `ImageAttachment` (the canonical home of that type, with its encoded size); size-bounded. Also the tile geometry (`tileBox`) and draft `<img>` source (`attachmentSrc`); own vitest suite. |

The transcript's copy affordances — fenced code blocks, blockquotes, and whole
assistant messages — reuse `../shared/clipboard.ts` (the native-first clipboard
writer lifted out of the terminal pool) — see the shared/ area. Selection-copy
depends on settled prose being plain text nodes: the settle swap to the
canonical parse (below) renders span-free, because span-fragmented text copies
with a hard newline at every visual wrap point.

A selection in the transcript joins the workbench's context bridge
(`../shared/reference.ts`): `ChatView` publishes it as a `chat` selection and
floats the shared `ReferenceChip` ("quote in reply") on the chat root
(`quoteSelection.ts` holds the DOM helpers); App resolves its target to this
same chat only and inserts `composeChatQuote`'s blockquote as a `block` into
the publishing view's composer. A chat whose composer is disabled offers none. Keep that chip (and any other floating chrome) OFF the
`.column`: `readingAnchor.ts` binary-searches the column's children as a
vertical stack of rows, which an absolutely positioned child would break.

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
- **A queued send is NOT a transcript block until the agent reads it.**
  A mid-turn send (plain Enter) is read at the agent's NEXT STEP — between tool
  calls, both agents; until then it lives in `store.pendingSends` (a faded
  bubble at the scrollable transcript tail), never in `blocks`, so it can't
  splice output already rendered or crowd the fixed composer. The reducer
  appends it to `blocks` at the current end only when `user_message_update`
  resolves it `sent` — possibly mid-turn, which is where the agent read it
  (several waiting messages are read together; such a block is `midTurn`, and
  the turn-end artifact scan looks past it to the turn's real opener); `cancelled` removes it;
  `dropped` marks it "not delivered" until dismissed. `send_after_turn` (the
  composer's ⌥↩ / Alt+Enter, yielding to any app action the user binds to the
  same keys — ⇧⌘↩ is Zoom Pane) holds a message until the turn ends: its echo carries `after_turn`, kept as `afterTurn` (caption "after
  this turn" vs "next step"). Every waiting bubble offers **Send now**
  (`{type:"send_now", id}`: the daemon interrupts the turn and every waiting
  message is read at once) and ✕ (`{type:"cancel_queued", id}`: pulls back a
  waiting send, dismisses a dropped one — the driver's tombstone `Cancelled`
  survives replay — and no-ops, or answers a Notice, once read). A **Stop
  never drops the queue** — the driver aborts only the turn and the waiting
  messages resolve `sent` right after, so `dropped` means genuinely
  undeliverable (agent died). `steer_queued` is wire-only for old clients;
  the UI never sends it. All pure reducer, so replay rebuilds the identical
  order — see `store.svelte.test.ts`.
- **Other agents' messages are cards, never the user's words.** They reach a
  chat two ways, and both fold into one `agent_message` block: a Claude chat's
  hook delivery is the daemon-journaled `agent_message` event (context at the
  next step — no user message exists), and a real send (a wake, the user's
  hand-over, a Codex steer) is a `user_message` with `origin: "agent"` /
  `"mastermind"` whose text the reducer parses into messages. A queued one
  keeps its origin in `pendingSends` and promotes to the card, not a user
  bubble. A turn-opening send is a turn boundary (the artifact scan, the wake
  marker) like a user message; its checkpoint is swallowed, never passed to an
  earlier user message. The legacy `origin: "worker"` stays a tagged user
  bubble.
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
  rebalance, which re-sizes the spacers to the model with one compensating
  write read from the offset BEFORE the spacer changes (shrinking it first
  clamps the offset). Idle means no scroll event for 160 ms AND no wheel
  input (a decelerating fling keeps sending momentum wheel events after its
  move rounds to nothing, while WebKit still owns the position), then one
  frame (a long task can run the timer ahead of queued scroll events): a
  write inside a fling is snapped back and leaves WebKit's scrolling thread
  and the page disagreeing about the offset. A range write that drops rows
  above the reader holds the column's height until the anchor hold has run
  (WebKit clamps the offset at the layout in between). The transcript sets
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

Remote chats (Pro): attaching and reconnecting are passive; there is no wake
button. A keeper may keep a sleeping cloud machine's sockets open (it marks them
`X-Chimaera-Sockets: kept`; VIEWING.md, "A sleeping cloud machine's sockets";
none is deployed as this is written), and a native window's daemon then passes
straight through to it. `ChatSocket.send` on an open socket is simply sent
(never a wake redial). `QUIET_OPEN_MS` after authentication a socket that
heard nothing is `store.held` (not live, so a send shows pending; not
reconnecting, so no "Reconnecting…" row, no rail pulse and no "· reconnecting"
in the header; before the first replay a viewed cloud conversation shows the
wake hint, `waitsForCloud`). `{"type":"waking"}` shows unconfirmed sends as
pending even when the socket still looked live, and a second `ready` on the
same socket is a reattach: `onReady` resets nothing, `apply`'s seq guard drops
what the store has, pending bubbles wait for their echoes (and go out again by
id, below). `worker_asleep`
ends `held`; so does `remote_unavailable`, which also ends `connected` and
`waking`: with `reason:"reconnecting"` (a relay retrying) that lasts until the
next frame, without it (a hand-back) the socket is still kept and `held`
returns after the quiet window. A kept socket that drops is dialed again
before the placement's "suspended" parks it.

Sends (any keeper, relay or daemon): each composer send goes out under a
`client_id` (`mintSendId`) and is kept in `store.unconfirmed` with its frame
(`noteSent(id, frame, text, images)`); `store.sending` is the ones shown as
"sending…" bubbles (several at once, also under the loading line). Only the id
settles a send. The echo (`user_message`) that carries it confirms it. A
refusal of a `send`/`send_after_turn` that carries it (`onCommandFailed`'s
`clientId`) hands back exactly that text through `restoredDrafts` /
`takeRestoredDrafts` (a queue; ChatView hands the oldest
that fit to the composer through `composerBus`'s return channel,
`returnableCount` + `returnToComposer`: texts above the draft in progress,
pictures attached, no focus taken and a focused caret kept where it was; a
send whose pictures do not fit waits in the queue, whole, until the composer
reports room through `onReturnRoom`); a refusal naming an id the store no
longer holds says nothing (unless it is one `noteSentOutside` recorded,
below). At every `ready` whose daemon says
`send_ids` (`ReadyAttach.sendIds`), once `lastSeq` reaches its `head`
(`resendUnconfirmed`), each send still without an echo goes out again under
the same id through the path `chatPool` bound with `bindSender`
(`ChatSocket.sendQuietly`: never a redial, never a wake), while it is
younger than `RESEND_FOR_MS` (two minutes): the daemon runs an id once, so
that is right whether the first copy was lost, is queued in the daemon or is
about to be delivered by a keeper. Copies of one send are paced (`resendDue`:
at least `RESEND_GAP_MS` since it last went out, doubling to 30 s); one not
due at the `ready` goes out from the store's own timer when it is, if the
conversation is still live and its echo has not come (`onDisconnected` and
`dispose` clear the timer). An older one is withdrawn with
`cancel_send`; `send_cancelled` (`onSendCancelled`) with `cancelled:true`
returns its text with the notice "not delivered", `false` leaves the bubble
for the echo that is coming. Those two frames are the only thing this client
ever sends by itself; there is no queue of commands. A refused `cancel_send`
is not shown and is asked again at the next `ready`.

A daemon without `send_ids` is never sent anything twice and no `ready`
decides anything there. Its echo has no id and confirms the oldest send with
exactly that text (a send made against such a daemon keeps that rule after the
daemon is replaced, `UnconfirmedSend.plain`); its refusal names no send and
returns the newest. A refusal without an id behind a daemon that has them (a
holder in between that predates ids) returns nothing, however many sends are
unconfirmed: it may answer a second copy of a send that holder still
delivers. They show as pending and the next `ready` sends or withdraws each. `waking`/`bringing` only change
presentation (`showUnconfirmed`). A send on a live connection shows no bubble
until it has waited `SHOW_UNCONFIRMED_AFTER_MS` for its echo (ChatView's timer
calls `showOverdue`; ids daemons only). `onDisconnected` (the pool calls it
too when it heals a dead socket) shows what is unconfirmed as pending; an exit
or a fall back to the terminal hands back everything; a move forgets nothing.
The Mastermind panel's one-click prompts go out under their own id and tell
the store (`noteSentOutside`): nothing to confirm or return, but a refusal
that names one is still said, and is never taken for the composer's send.

Commands that are not the user acting (`set_thinking`, `get_usage`, `get_mcp`,
`cancel_send`, a dry-run `rewind`) are dropped by a keeper or relay while
nothing is attached, so nothing may wait on one forever: `/mcp` keeps the
inventory it has and closes after 10 s without a first answer, a rewind's dry
run closes after 30 s (both only for a viewed conversation). `thinkingPushed`
is cleared by a new `init` and by a second `ready` on the same socket
(`ReadyAttach.reattach`: its keeper dropped a push made while nothing was
attached), never by a plain reconnect, where the process still has it and a
second window's default would override another window's choice at every blip.
A toggle made while not `connected` is marked pending (`toggleThinking`), so
the next `ready` pushes it.
The seven settings commands (`set_model`, `set_mode`, `set_effort`,
`set_ultracode`, `set_remote_control`, `set_mcp_enabled`, `reconnect_mcp`) are
held by a keeper and by this computer's relay while the owner is not attached.
The relay delivers one only in front of this viewer's next acting command
(never by itself at a `ready`) and refuses it by name after ten minutes, so
the refusal's notice shows; they have no optimistic state, so the old value
shows until the owner confirms.

The rest of this paragraph is what happens against a keeper that refuses or
closes those sockets (today's), and for another computer as owner. In a native window the daemon holds the first command while a paused
owner wakes (the additive `{"type":"waking"}` → `store.waking`, "Waking the
cloud machine…"), refuses further acting commands until it answers, and answers
anything it cannot deliver with `command_failed`. Only acting commands (the
daemon's `activity::is_interaction`) wake the owner or bring work here; the
seven settings commands are held with them (above), everything else is
dropped while the owner is away. Every refusal carries the additive `command`
it answers and the `client_id` it was sent under; the store keeps each send
with its pictures (`noteSent`) until its echo and hands it back
(`restoredDrafts` → `composerBus`'s return channel) only for
`command:"send"` / `"send_after_turn"` — never for a refused interrupt/permission
answer, which could resurrect a delivered message. A send made while not
live shows at once in `store.sending` ("sending…") until its echo. Acting on a conversation another
of the user's computers runs brings its work here: the daemon holds the send
and says `{"type":"bringing","to":"here"}` (a browser view's gateway says
`to:"computer"` when a phone's send on a sleeping cloud goes to a computer) →
`store.bringing` ("Bringing the work here…" / "Bringing the work to your
computer…", the send shown pending); it ends with the next `ready`, a wake, a
move, or a `command_failed` with `reason:"still_working"` (the other computer
kept it; the send it names comes back to the composer). A move to another computer is
`moved` with `other:true` → `store.moving = "other"` ("Continuing on your
other computer…"). In a browser view, `send()` into a
dropped socket reconnects once with `?wake=interaction` and returns false (the
composer keeps the draft). Socket states that are not errors: `worker_asleep` →
`store.asleep` ("Asleep in the cloud. Send a message to wake it.", and the
header's "In the cloud · asleep" instead of "· reconnecting"; it survives a
dropped socket and ends with an accepted send, `waking`, a move or `ready`; a
socket that drops meanwhile waits with no retry timer until a send dials it with
wake intent or `retrySoon` / `ownerAwake` dials it passively — `reachKey`
includes the row's `placement_available`);
the additive `{"type":"moved","to"}` → `store.moving` (only
for a real transfer); and the additive `{"type":"paused","reason","provider"?}`
→ `store.pausedFor` (restarting / needs_provider / importing): the chat is NOT
ended, stays mounted (the pane keeps a paused chat row's ChatView), hides the
replayed "agent exited", disables the composer with `net/placement.ts`
`pauseLabel` ("Continuing in the cloud…" — or, signed out (`net/plan.ts`
`accountSignedOut`), "This conversation is in the cloud. Sign in to Chimaera
Pro to bring it back." — "Picking up where you left off…",
"Waiting for Claude Code in the cloud", "Opening…"; a row naming the
agent in `blocked_provider` adds **Connect <agent> to continue**, `pro/providers.ts`
`pausedConnect`), and retries at once
(`ChatSocket.retrySoon`) when its row stops being paused or changes owner. The connection row appears only for a viewed project (routed row or
browser view) after a 2 s grace; while it says "Reconnecting…" the header does
not repeat it. Browser sockets use `net/base` to preserve gateway prefixes. The
daemon's transfer pick-up (a user message with origin `moved`, `home` or `recovered`) renders
as `TransferNote.svelte`: one line ("Continued in the cloud · 5m ago", "Back on
your computer", or "… after this computer stopped responding" when it
is tagged `recovered`, its direction read from the daemon's sentence — `transfer.ts`) with the agent-facing
text behind "Show what the agent was told". A surface that holds only the prompt's text, not the
origin tag (the Timeline's "Since you left" rows), recognises the pick-up from its opening words
with `pickupNote` and shows the same short line instead of quoting the agent-facing text.
When the chat's project came back from the cloud with files both sides changed while apart
(Pro's kept-both report, `pro/keptReviews.svelte.ts`, read once per project and again on a
`kept_both` notice or a choice), `KeptNote.svelte` draws one more such line — "Back on this
Mac. The cloud and this Mac both changed 3 files while apart." with **Review**, which opens the
review of both versions (`pro/KeptReviewView.svelte`). It is not a block: ChatView places it by
the report's `returned_at` before the first user/assistant row sent after the return (after the
last row at the live edge when nothing followed), never in a chat that began after the return or
whose return point is outside the mounted window; a `home` pick-up at that point carries the
line itself (`TransferNote`'s `kept` prop) instead of a second divider. The line goes once
nothing waits for a choice.
`store.awaitingWake` (`hydrating && asleep`) replaces the loading line with one quiet sentence while
the owner sleeps before the first replay; a wake (`waking`) hands back to the ordinary loading line.
ChatView's `waitsForCloud` shows the same sentence for a kept, quiet socket (`held`) of a viewed
conversation that runs on a cloud machine (`net/placement.ts` `ownerIsCloud`), never for a local chat.

## Session capabilities

`capabilities.ts` consumes the driver’s additive `capabilities` event; unknown agents have no
implicit controls. `catalog` refreshes model/mode/command choices without replaying Init or
resetting a turn. Header/composer controls follow those facts. `ForkDialog` includes every
ready chat adapter; native history and a conversation copy have different transfer semantics.
See [integration design](../../../../docs/agent-harness-design.md).
