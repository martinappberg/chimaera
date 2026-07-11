# web-ui/src/lib/chat — the structured chat surface

Orientation for coding agents. This directory is the **front half of chat mode**:
the rich UI that renders the daemon's structured agent stream (Claude & Codex),
the sibling of the xterm.js terminal surface. Parent map: repo-root
[CLAUDE.md](../../../../CLAUDE.md). The back half it talks to is
[`crates/chimaera-agent`](../../../../crates/chimaera-agent/CLAUDE.md).

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
| `store.svelte.ts` | `ChatStore` — the reducer + all reactive view state (`blocks`, `pending`, `questions`, model/mode, activity, exited/degraded/connected). **The single source of truth for the view.** |
| `chatWs.ts` | `ChatSocket` — connect/auth/reconnect(backoff)/gap-replay, decode frames, dispatch to handlers. Shares reconnect accounting with `../terminal/ws.ts`. |
| `ChatView.svelte` | The host: wires the socket + store, renders the transcript, and hangs the header/composer/overlays/panels off itself. Still the big one — keep new chrome in child components, not inline. |
| `ChatHeader.svelte` | The header row: model / mode / effort pickers, usage + `/mcp` entry, session identity (always names which agent — Claude or Codex). |
| `EffortPopover.svelte` | The reasoning-effort ladder picker (uses the agent-native vocabulary verbatim — never relabel `xhigh`). |
| `Composer.svelte` | Input: draft, images, `@`-mention + `/`-slash popovers, submit/interrupt. |
| `Markdown.svelte` | Renders agent prose. **Sanitizes untrusted model output** (marked → DOMPurify, `<style>` forbidden, external links `noopener`); stamps validated file paths as clickable. |
| `ToolCallCard` / `ToolGroup` | Tool-call rendering (title, status, diff/output, grouping). |
| `PermissionCard` / `QuestionCard` | The permission prompt and structured-question cards (their answers ride `socket.send`; `PermissionCard` also carries the deny-with-feedback field). |
| `PlanApprovalCard.svelte` | Claude `ExitPlanMode` plan-approval card — renders the sanitized plan markdown + the three official options (auto-accept / manual / keep-planning) with an optional comment that rides the permission reply. |
| `RewindDialog.svelte` | The rewind/fork-point confirmation overlay (claude fork + codex `thread/rollback`). |
| `McpPanel.svelte` / `UsagePanel.svelte` | The `/mcp` linked-server panel and the token-usage panel. |
| `InlinePreview` / `ArtifactGallery` | Inline file/image previews inside the transcript. |
| `UserText.svelte` | User-message bubble. |
| `paths.ts` | Path-candidate detection + validation types (shared with Markdown's stamping). |
| `composerBus.ts` | Cross-component channel to insert text/attachments into the active composer (e.g. `@term:` grants, references, dropped-file paths). |
| `drafts.ts` | Per-session composer draft persistence (survives the per-session ChatView remount + a page reload) — text layers into sessionStorage, images stay in-memory; both bounded. |
| `images.ts` | Pasted/dropped image → downscale + base64 encode into an `ImageAttachment` (the canonical home of that type); size-bounded. |

The transcript's copy affordances reuse `../shared/clipboard.ts` (the native-first
clipboard writer lifted out of the terminal pool) — see the shared/ area.

## Invariants / gotchas

- **Agent output is untrusted.** Anything the model emits (prose, tool output,
  file contents it echoes) is attacker-influenced. Render it through
  `Markdown.svelte`'s sanitizer; never `{@html}` raw agent text elsewhere; never
  build a live external link without `rel="noopener"`.
- **Never lose a user action to a closed socket.** `socket.send` returns `false`
  when not OPEN — respect it (the composer keeps the draft; `store.connected`
  tracks liveness). Reconnect replays the gap; don't invent a client-side queue.
- **The seq contract is the daemon's.** Trust `lastSeq`/`head` from the wire; do
  not renumber. A gap is healed by reconnect replay, not by client bookkeeping.
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
