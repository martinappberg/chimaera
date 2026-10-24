# web-ui/src/lib/chat — the structured chat surface

Orientation for coding agents. This area is the front half of chat mode: the UI for Claude, Codex, Antigravity, Grok and Cursor. Back half: [chimaera-agent](../../../../crates/chimaera-agent/AGENTS.md). Svelte 5 runes; Node 26. Parent map: [AGENTS.md](../../../../AGENTS.md). Rules: [web-ui.md](../../../../.claude/rules/web-ui.md). All detail, by topic: [REFERENCE.md](REFERENCE.md).

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

## File map

| Files ([in full](REFERENCE.md#file-map-in-full)) | What they own |
|---|---|
| `store.svelte.ts` | `ChatStore`: the reducer and all view state. |
| `chatWs.ts`, `cooperativeQueue.ts` | `ChatSocket`: auth, reconnect, gap replay, ordered dispatch. |
| `chatPool.ts`, `chatPoolRegistry.ts` | Warm store and socket per session; the registry App imports. |
| `ChatView.svelte` | The host: transcript window, composer, overlays. |
| `transcriptWindow.ts`, `readingAnchor.ts`, `heightModel.ts` | Window math, reading anchor, row heights. |
| `Markdown`, `streamSegments.ts`, `revealLedger.ts`, `tables.ts`, `markedExtensions.ts`, `MathText`, `math.ts` | Sanitized prose and math, streamed. |
| `ChatHeader`, `ModelPicker`, `EffortPopover`, `capabilities.ts` | Header controls from capabilities. |
| `Composer`, `composer.ts`, `composerHeight.ts`, `composerBus.ts`, `drafts.ts` | Input, slash completion, inserts, drafts; one context row for branch/worktree, linked terminals and notices, only when populated; terminal completion opens through `@term`, with no permanent launcher. |
| `stopConfirm.ts` + `StopNote.svelte` | Esc / Stop / Clear goal ask once ("Also stops 2 subagents · Esc again to stop") when that action also ends subagents or workflows, the line above the control pressed and worded by the real gesture; otherwise they act at once. |
| `uploadTokens.ts`, `uploadChips.ts`, `ComposerMentions`, `terminalMentions.ts` | Atomic mention tokens in the draft. |
| `voice*.ts`, `VoiceMeter` | Voice dictation. |
| `images.ts`, `AttachmentStrip`, `ImagePreview` | Image attachments and tiles. |
| `ToolCallCard`, `ToolGroup`, `toolLabels.ts` | Tool calls and their one-line labels. |
| `Activity*`, `activityFold.ts`, `FinishedFold`, `FinishedRow` | Folds of settled activity. |
| `ThoughtRow`, `thoughtText.ts`, `HookRow`, `hookNotice.ts`, `statusWord.ts` | Thought and hook lines; status word. |
| `AgentsTray`, `BackgroundTray`, `PlanCard`, `AheadCard`, `WorkStrip`, `workStrip.ts` | Pinned work cards; one shared inventory folds three kinds (including tasks) into a labelled strip; Running groups background agents and commands behind an activity icon. Idle tasks fold without adding a status label or refresh action; task tools own completion. |
| `subagent*.ts`, `SubagentBanner`, `SubagentChip`, `ModelChip` | A subagent as a read-only chat. |
| `AgentMessageCards`, `agentMessages.ts`, `WakeRequestBar` | Other agents' messages and wake requests. |
| `PermissionCard`, `QuestionCard`, `PlanApprovalCard`, `Elicitation*` | Answers to the agent. |
| `RewindDialog`, `ForkDialog`, `AgentMessageMeta` | Rewind, fork, message actions. |
| `McpPanel`, `UsagePanel`, `UsageMeter` | `/mcp`, `/usage` and the usage meter. |
| `ArtifactGallery`, `artifacts.ts`, `embeds.ts`, `FileChip`, `ProseChip`, `hoverTargets.ts` | Embeds, written files, hover previews. |
| `paths.ts`, `UserText` | Path links; the user-message bubble. |
| `ChatFind`, `chatFind.ts`, `quoteSelection.ts` | Find in chat; quote a selection. |
| `nativeUi.ts`, `mods.svelte.ts`, `Mod*.svelte`, `modClient.ts`, `nativeComposer.ts` | [Claude Mods](REFERENCE.md#claude-mods): never journaled or replayed. |

## State owners

| State | Owner | Who mutates |
|---|---|---|
| `blocks`, `pendingSends`, plan, subagents, flags | `ChatStore` | `apply` and store methods only. |
| Socket, resume `lastSeq` | `ChatSocket` | The socket; it feeds the store. |
| Warm entries, scroll cursor | `chatPool.ts` | Acquire; App disposes. |
| Drafts, dictation | `drafts.ts`, `Dictation` | `Composer`; `Dictation`. |

## Invariants / gotchas

- **Render agent output only through `Markdown.svelte`'s sanitizer** ([render](REFERENCE.md#render-and-sanitizer-invariants)).
- **Streaming is never more permissive than settle** ([pipeline](REFERENCE.md#the-streaming-render-pipeline)).
- **Trust the daemon's seq: a gap heals by replay, and fork points are event-backed** ([reducer](REFERENCE.md#reducer-invariants)).
- **A queued send becomes a block only once read, and agents' messages are cards** ([reducer](REFERENCE.md#reducer-invariants)).
- **The reducer's vocabulary is pinned by daemon-written fixtures.** `net/fixtures/wire/chat-events.json` (every `AgentEvent` variant, written by chimaera-agent's wire test) goes through `ChatStore.apply` in `net/wireFixtures.test.ts`, which expects `store.unhandledEventTypes` to stay empty — a type the reducer ignores on purpose is an explicit `case`, never the `default:`.
- **Never lose a user action to a closed socket; hiding a chat never stops its agent** ([transport](REFERENCE.md#transport-invariants)).
- **Rows key by stable uid, never array index** ([file map](REFERENCE.md#file-map-in-full)).
- **Scroll has one writer; never write `scrollTop` mid-gesture** ([scroll](REFERENCE.md#scroll-invariants)).
- **Keep floating chrome off `.column`** ([quote](REFERENCE.md#copy-and-quote-selection)).
- **`ForkDialog` is pane-local, not a window modal:** no focus trap, so App's chords stay live; Escape anywhere in its pane or a press outside it in its pane cancels, and that Escape never reaches the composer's interrupt ([ui-contracts §3](../../../../docs/agent-guides/ui-contracts.md#3-focus-escape-and-keyboard-navigation)).
- **Never infer controls from the agent's brand** ([capabilities](REFERENCE.md#session-capabilities)).
- **Mutate `$state` only in store methods, tear down every `$effect` timer, use theme tokens, share chrome instead of re-pasting it per card** ([conventions](REFERENCE.md#runes-and-ui-conventions)).

## Adding to the chat UI

Event: a `ChatStore.apply` case. Command: `socket.send` + `AgentCommand`. Chrome: a new component ([recipe](REFERENCE.md#adding-to-the-chat-ui)).

## Checks

```sh
npm --prefix web-ui run check
npm --prefix web-ui run test -- src/lib/chat
```

Verify live (**verify-app** skill); a fake claude (**chat-mode** skill) gives a hermetic UI check.

## Neighbours

[net](../net/AGENTS.md) · [layout](../layout/AGENTS.md) · [shared](../shared/AGENTS.md) · [settings](../settings/AGENTS.md) · [terminal](../terminal/AGENTS.md). Feature pages: [chat-mode](../../../../docs/features/chat-mode.md), [claude-mods](../../../../docs/features/claude-mods.md), [loops-and-goals](../../../../docs/features/loops-and-goals.md), [agent-communication](../../../../docs/features/agent-communication.md).
