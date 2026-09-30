# web-ui/src/lib/dashboard — the workspace dashboard

Orientation for coding agents. The workspace-first landing surface (the
feature page: [docs/features/dashboard.md](../../../../docs/features/dashboard.md);
the design spine: [docs/agent-dashboard-plan.md](../../../../docs/agent-dashboard-plan.md)).
Parent map: repo-root [AGENTS.md](../../../../AGENTS.md).

## File map

| File | What it owns |
|---|---|
| `DashboardView.svelte` | The surface, re-centred on questions (design §3): the vital-signs strip (name · the branch chip with ahead/behind + uncommitted count, opening source control · the sentence · the Slurm compute chip), **Needs you** (the attention lane, inline permission answering over warm chat sockets, then agent communication's wake requests — `WakeRequestCard` — which also count in the strip's "needs you"), **Since you left**, **Where things stand**, **Now** (one line by default; `dashboard.roster: "cards"` shows today's roster as `AgentCard`s), the blank state with recents, and the quiet "Ask the Mastermind ⌘J" pill (the Mastermind itself lives in the window panel). It captures the viewer's "last look" baseline (localStorage, per workspace) each time it becomes visible + focused. |
| `SinceYouLeft.svelte` | The Timeline past the viewer's last look: grouped per session, bad news first, ≤8 rows of `../workspace/TimelineRow.svelte`, "open timeline →"; empty is one line ("Nothing new since 14:02"). A turn the daemon opened itself (a transfer's or a restart's pick-up) reads as its divider line ("Continued in the cloud"), never as the agent-facing text quoted as the person's prompt (`chat/transfer.ts` `pickupNote`); a read that met a project state (asleep, unreachable) is one muted line (`timelineStore.note`), not an error. |
| `PluginPanels.svelte` | Active plugins' `slot = "panel"` views (the 0.2 platform), after core's sections: the view's title and the plugin's name as the heading, the screen in a card (`plugins/ui/PluginScreen.svelte`). Nothing when no active plugin has one. |
| `WhereThingsStand.svelte` | The top of Knowledge (`knowledge/overview.ts` over `knowledgeLookup`): Waiting on you, Recently recorded (corrections and supersessions first, never a rating), next steps, a warn-toned blocker; each row opens its entry (`focusKnowledgeEntry`); the words over its lists and the "from …" source are the provider's (`labels`); without an active provider one calm sentence — the installed knowledge plugin's own summary (`knowledgePlugin`) and that it is off here, or that Extensions lists them — and one link, **Extensions** (`DashCtx.onOpenExtensions`). Core names no plugin here. |
| `GuidanceRow.svelte` | What the agents are told — the route's `guidance` (the knowledge plugin's own files first, then `AGENTS.md`, `CLAUDE.md`, claude memory) as one row of links, after Where things stand. It lives here because it answers whether or not a knowledge plugin is on. |
| `NowLine.svelte` | Who is running as one line (name in mono + an honest phrase from the rail's vocabulary, terminals folded into a count, "show cards"). |
| `ActivityLine.svelte` | The surface's last line: this workspace's last seven days from `GET /activity` ("This week: 38 sessions · 1.2M tokens", no dollars), opening Settings → Activity (`DashCtx.onOpenActivity`); fetched while visible and on the history nudge; hidden when nothing was recorded. |
| `AgentCard.svelte` | One roster card (cards mode): provenance tier (worn as words by the degraded tiers only), the warn-toned same-file notice (`../workspace/SameFileNotice.svelte`, from `sameFile.svelte.ts`), state dot, unread mark, a quiet "✉ 2 messages waiting" in the meta line while other agents' messages sit in its inbox (`../workspace/comms.svelte.ts`), now-line (incl. the post-turn `status_detail`), ctx meter/cost, a quiet branch label in the meta line when the session is in a repository (`shared/BranchChip.svelte`, opens "Changes on this branch"), the work drop-down (subagents ∪ background tasks — workflow rows carry name + agent tally), evidence rows. |
| `MastermindDock.svelte` | The Mastermind's content, hosted by `MastermindPanel`: setup card (agent + ask/auto), embedded `ChatView` on the chat pool, the clickable `acts:` gate badge, **Brief me** + the one question about the focused tab (`context`, from App) + **What's next?** + the empty-transcript "Anything conflicting?" + the inbox chip — the comms store's unread count for the Mastermind's session; a click hands them over (`comms.deliver`, one message, one turn; a refusal shows in the daemon's words), the quiet "Use mycelium" line, the native-permission-mode caveat line, close (expand only when a host passes `onToggleExpand`), mode-switch/retire, the honest gone/degraded states. The Mastermind is part of agent communication: with `agents.communication.enabled` off, a calm "Agent communication is off" strip with **Open Settings** (`onOpenSettings`, App → Settings at that row) replaces the prompt row, a bound Mastermind's transcript stays readable (dormant, not retired), and the setup card's controls are disabled with that reason. The header reflows against its own width (container query) so no control is pushed off the edge. |
| `MastermindPanel.svelte` | The window's ONE right-hand Mastermind panel (mounted beside the stage in `App.svelte`, lazy-loaded on first open so `ChatView` stays out of the entry bundle): a sibling card of the panes, left-edge resize, docked or — on a narrow window — floating over the stage. |
| `mastermindPanelState.svelte.ts` | Its runes state: open/closed per window (sessionStorage), width per profile (localStorage), and what the corner icon in `PaneTabs` needs (`available`, `cornerPaneId` = `layout.topRightPane`, `attention`). Mutated only through its functions. |
| `AttentionCard.svelte` | One "needs you" lane entry (unchanged by the rework). |
| `WakeRequestCard.svelte` | A wake request in Needs you (the "Ask me" wake policy): "⟨from⟩ wants to wake ⟨to⟩" with vendor marks, the message as one clamped plain-text line, **Wake** / **Leave in inbox**; `reason: "hop_limit"` asks "⟨from⟩ and ⟨to⟩ have been going back and forth — let them continue?" with **Continue**. The daemon drops a request whatever the answer, so a refused one (`comms.wakeFailures`) stays as this card with the daemon's words and **Dismiss**. |
| `dash.ts` | Shared derivations: `provenanceOf`, `rosterWeight`, `relPath`, the `DashCtx` type (incl. the openers for the Timeline / Knowledge / Extensions singletons). |

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
  turn on its own — no timer, no reaction to state. "Brief me" and the
  suggestion chips each send ONE canned prompt on the user's click, over
  `acquireChat(id).socket` (the composer's own path); `send` returning
  `false` surfaces as a store notice — never a client queue. The inbox chip
  and a wake request's **Wake** are the user's click on a daemon route
  (`comms.deliver` / `comms.wake`), never a client-built prompt.
- **"Since you left" is per viewer, not daemon state.** The baseline is the
  seq this browser had looked up to (localStorage, `workspace/timeline.svelte.ts`
  `lastSeen`/`markSeen`), captured when the dashboard becomes visible and
  held while it stays visible; a stored seq above the daemon's head (data
  dir wiped) resets. Within the window, failures and contradictions sort
  first (`workspace/timelineModel.ts`).
- **Timeline / Knowledge / plugin status / comms are stores, not props**
  (`workspace/timeline.svelte.ts`, `workspace/knowledge.ts`,
  `plugins/store.ts`, `workspace/comms.svelte.ts`): activated with the
  workspace in App.svelte, refetched off their `/ws/events` epoch nudges
  (`timeline`, `comms`), quiet while hidden. Comms also refetches after the
  events socket reconnects (a restarted daemon renumbers its epochs).
