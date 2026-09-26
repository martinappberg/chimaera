# web-ui/src/lib/dashboard — the workspace dashboard

Orientation for coding agents. The workspace-first landing surface (the
feature page: [docs/features/dashboard.md](../../../../docs/features/dashboard.md);
the design spine: [docs/agent-dashboard-plan.md](../../../../docs/agent-dashboard-plan.md)).
Parent map: repo-root [AGENTS.md](../../../../AGENTS.md).

## File map

| File | What it owns |
|---|---|
| `DashboardView.svelte` | The surface, re-centred on questions (design §3): the vital-signs strip (name · the branch chip with ahead/behind + uncommitted count, opening source control · the sentence · the Slurm compute chip), **Needs you** (the attention lane, inline permission answering over warm chat sockets), **Since you left**, **Where things stand**, **Now** (one line by default; `dashboard.roster: "cards"` shows today's roster as `AgentCard`s), the blank state with recents, and the quiet "Ask the Mastermind ⌘J" pill (the Mastermind itself lives in the window panel). It captures the viewer's "last look" baseline (localStorage, per workspace) each time it becomes visible + focused. |
| `SinceYouLeft.svelte` | The Timeline past the viewer's last look: grouped per session, bad news first, ≤8 rows of `../workspace/TimelineRow.svelte`, "open timeline →"; empty is one line ("Nothing new since 14:02"). |
| `WhereThingsStand.svelte` | The top of Knowledge: contradicted first, then the strongest findings with the ladder, next steps, a warn-toned blocker; without a structured provider one quiet line + "Use mycelium →" (the attach sheet). |
| `NowLine.svelte` | Who is running as one line (name in mono + an honest phrase from the rail's vocabulary, terminals folded into a count, "show cards"). |
| `AgentCard.svelte` | One roster card (cards mode): provenance tier (worn as words by the degraded tiers only), state dot, unread mark, now-line (incl. the post-turn `status_detail`), ctx meter/cost, the work drop-down (subagents ∪ background tasks — workflow rows carry name + agent tally), evidence rows. |
| `MastermindDock.svelte` | The Mastermind's content, hosted by `MastermindPanel`: setup card (agent + ask/auto), embedded `ChatView` on the chat pool, the clickable `acts:` gate badge, **Brief me** + the "Ask about" context chip (the focused tab as a composer reference) + the empty-transcript suggestion chips + the Agent-notes inbox chip (each one user click = one canned prompt over the session socket), the quiet "Use mycelium" line, the native-permission-mode caveat line, close (expand only when a host passes `onToggleExpand`), mode-switch/retire, the honest gone/degraded states. The header reflows against its own width (container query) so no control is pushed off the edge. |
| `MastermindPanel.svelte` | The window's ONE right-hand Mastermind panel (mounted beside the stage in `App.svelte`, lazy-loaded on first open so `ChatView` stays out of the entry bundle): a sibling card of the panes, left-edge resize, docked or — on a narrow window — floating over the stage. |
| `mastermindPanelState.svelte.ts` | Its runes state: open/closed per window (sessionStorage), width per profile (localStorage), and what the corner icon in `PaneTabs` needs (`available`, `cornerPaneId` = `layout.topRightPane`, `attention`). Mutated only through its functions. |
| `AttentionCard.svelte` | One "needs you" lane entry (unchanged by the rework). |
| `dash.ts` | Shared derivations: `provenanceOf`, `rosterWeight`, `relPath`, the `DashCtx` type (incl. the openers for the Timeline / Knowledge / Plugins singletons). |

## Invariants / gotchas

- **Status must be honest.** Cards derive from the same `agent_state`/`dotState`
  vocabulary as the rail; provenance (`protocol` › `hooks` › `output-only`) is
  worn, never faked; a card never renders confident green from a liveness-only
  signal.
- **The Mastermind is the observer, not the observed**: every roster surface
  filters through `isMastermind()` (workspace/sessions.ts) — never re-type the
  predicate inline.
- **Warm chat detail rides the shared chat pool** (`chat/chatPool.ts`), which
  refcounts holds — acquire/release in pairs, and keep the lane acquisition
  bounded (`RICH_LANE_MAX`).
- `visible` into `MastermindDock`/`ChatView` means what it means for a pane tab
  ("this surface is showing"): an open panel passes `true`. Never feed it document
  visibility — `ChatView` freezes a `visible: false` transcript, so a panel opened
  in a background window rendered empty (found live).
- **Only user-clicked turns.** Nothing in this folder starts a Mastermind
  turn on its own — no timer, no reaction to state. "Brief me", the
  suggestion chips and the inbox chip each send ONE canned prompt on the
  user's click, over `acquireChat(id).socket` (the composer's own path);
  `send` returning `false` surfaces as a store notice — never a client queue.
- **"Since you left" is per viewer, not daemon state.** The baseline is the
  seq this browser had looked up to (localStorage, `workspace/timeline.svelte.ts`
  `lastSeen`/`markSeen`), captured when the dashboard becomes visible and
  held while it stays visible; a stored seq above the daemon's head (data
  dir wiped) resets. Within the window, failures and contradictions sort
  first (`workspace/timelineModel.ts`).
- **Timeline / Knowledge / plugin status are stores, not props**
  (`workspace/timeline.svelte.ts`, `workspace/knowledge.ts`,
  `plugins/store.ts`): activated with the workspace in App.svelte, refetched
  off the `/ws/events` `timeline` epoch nudge, quiet while hidden.
