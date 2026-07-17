# web-ui/src/lib/dashboard — the workspace dashboard

Orientation for coding agents. The workspace-first landing surface (the
feature page: [docs/features/dashboard.md](../../../../docs/features/dashboard.md);
the design spine: [docs/agent-dashboard-plan.md](../../../../docs/agent-dashboard-plan.md)).
Parent map: repo-root [AGENTS.md](../../../../AGENTS.md).

## File map

| File | What it owns |
|---|---|
| `DashboardView.svelte` | The surface: attention lane (inline permission answering over warm chat sockets), roster (hero/grid/compact by density, flip-animated), activity column (changed files + git), the vital-signs strip incl. the Slurm compute chip + the "last active" jump-back chip, settled-quiet recents, and the Mastermind dock column/pill wiring (user-resizable to full width). |
| `AgentCard.svelte` | One roster card: provenance tier (worn as words by the degraded tiers only), state dot, unread mark, now-line (incl. the post-turn `status_detail`), ctx meter/cost, the work drop-down (subagents ∪ background tasks — workflow rows carry name + agent tally), evidence rows. |
| `MastermindDock.svelte` | The Mastermind's only home: setup card (agent + ask/auto), embedded `ChatView` on the chat pool, the clickable `acts:` gate badge, the native-permission-mode caveat line, expand/collapse, mode-switch/retire, the honest gone/degraded states. |
| `TerminalRow.svelte` | The compact terminal-session row. |
| `dash.ts` | Shared derivations: `provenanceOf`, `rosterWeight`, `relPath`, the `DashCtx` type. |

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
- The dock is reactive-only: nothing in this folder ever triggers a Mastermind
  turn.
