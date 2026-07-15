# The workspace dashboard

The landing surface of a workspace: which agents need you, what everyone is doing, what
they produced, and where you left off — one glance, every element one click from the live
session. An empty workspace layout opens onto it; ⌘0 / the rail's `dashboard` row reach it
any time. This is v0.1 of the larger dashboard/Mastermind design
([docs/agent-dashboard-plan.md](../agent-dashboard-plan.md)); the workspace status feed,
the workspace MCP tools, and the Mastermind agent phase in behind it.

**Where it lives (shared):** UI `web-ui/src/lib/dashboard/` (`DashboardView.svelte`,
`AgentCard.svelte`, `AttentionCard.svelte`, `dash.ts`); the `dashboard` surface in
`web-ui/src/lib/layout/layout.ts` (`DashboardTab`, `openDashboard`, key `v:dashboard`) and
its branches in `Pane.svelte`/`PaneTabs.svelte`; the landing switch + rail row + ⌘0 in
`web-ui/src/App.svelte` (`pruneAndAutoOpen`, `openDashboardSurface`, `dashCtx`). No new
daemon surface: it renders from the existing `/ws/events` roster, the chat journal via
`web-ui/src/lib/chat/chatPool.ts` (`acquireChat`/`peekChat`), the git status store, and
the rail's recents.

## Landing & entry points

- **What & when.** Opening a workspace whose layout is empty lands on the dashboard instead
  of the first session. A restored non-empty layout is never touched.
- **How it's used.** Automatic (setting `dashboard.landing`, default `auto`; `never`
  restores the old first-session behavior). Manual: the fixed `dashboard` rail row above
  the terminals section, or Mod+0 (the same pinned-chord family as Mod+1–9; the digit `0`
  ride-along lives in `web-ui/src/lib/shared/keys.ts::chordDigit`).
- **Key behaviors.** Singleton tab (re-opening focuses it); serialized additively in the
  layout blob (`{v:"dashboard"}` — older builds skip it without resetting the layout).
  When **nothing is running** (no agents, no terminals) the surface shows no dashboard
  chrome at all — a launcher-style blank state (brand mark, `+ new agent` / `+ terminal`,
  recents, quick-open hint). Before the first session snapshot it shows a skeleton, never
  a false "everything died".

## The attention lane ("needs you")

- **What & when.** Every live session waiting on the human — `needs_permission`,
  `idle_prompt`, `errored` — as a wide card ahead of everything else.
- **How it's used.** For **chat** sessions the pending permission renders inline as the
  real `PermissionCard` (the same component the chat surface uses) and answering it rides
  the session's live `/ws/chat/{id}` socket (`{type:"permission"}` — answering here IS
  answering there). Structured questions surface as "has a question — answer in the chat"
  (the door, not an inline form). TUI sessions say honestly what is known ("needs
  permission — answer in the terminal") and offer the door.
- **Key behaviors.** Renders only when non-empty — quiet means quiet. Dead errored
  sessions stay in the roster, not the lane.

## Roster cards

- **What & when.** One card per agent session: who, doing what, needs what, produced what.
- **How it's used.** The whole card opens the live session (via the same reveal path the
  git panel uses, `LayoutCtrl.revealWorktreeSession`-adjacent `onOpenSession`). The
  evidence row ("N files · view changes") opens the session-scoped Changes view beside the
  dashboard (`LayoutCtrl.openChangesFrom`). A subagent line ("✳ 2 subagents · 14 tools")
  expands in place to per-subagent rows with live progress and a stop control (claude
  chat: `{type:"stop_task"}`) — the `AgentsTray` derivation promoted workspace-wide.
- **Key behaviors.**
  - **Provenance is worn openly**: every card carries its fidelity tier — `protocol`
    (chat, authoritative) › `hooks` (claude TUI) › `output-only` (other TUIs, honestly
    unknown) — with a tooltip saying what that means (`dash.ts::provenanceOf`).
  - **Density-adaptive**: one agent → a hero card (plan snapshot, subagents open by
    default); 2–6 → a card grid; 7+ → compact triage rows. The side column collapses under
    the pane's own width (container query), not the window's.
  - **Bounded rich detail**: warm chat stores are acquired for attention-lane chat
    sessions always, plus running chats up to a small cap (`RICH_CAP = 4`), so the
    dashboard can never churn the chat pool's LRU out from under open tabs; released on
    unmount (`releaseChat`, never `disposeChat`). Cards beyond the cap render wire truth
    only — empty is honest, fabricated is lying.
  - Cost is deliberately absent in v0.1 (no wire field carries a session total yet — the
    v0.2 feed adds usage); the context meter shows only when a warm store reports it.
- Plain shells collapse to one summary row ("N terminals · M running a command") with
  per-shell chips, plus `+ terminal` / `+ agent` spawners.

## The activity column

- **Changed files** — newest-first union of every agent's `files_touched` (attributed with
  a per-agent chip) and uncommitted git paths no agent claimed (chip `you`, tooltip states
  attribution is best-effort). Click opens the file in the active pane. Cap 10.
- **Recents** — the rail's daemon-persisted recents, resumable, cap 5.
- **Git** — branch, ahead/behind, change count; opens the source-control panel. Reads the
  live `gitStatus` store (epoch-driven, never polled).

## Key constraints

- **Status must be honest** (design spine): a card can never fake-green; every state comes
  from the same `agent_state`/`dotState` vocabulary the rail uses.
- **No new wire, no new routes** in v0.1 — the surface composes existing client state; the
  daemon is untouched.
- The return strip's summary sentence ("2 working · 1 waiting on you · 1 finished") counts
  agents; terminals summarize separately.

**Verified live (2026-07-15):** empty-workspace landing + blank state; spawn terminal +
claude chat from the dashboard; the attention lane rendering a real Write permission and
answering it inline (file created); provenance/now-line/ctx meter on the hero card;
changed-files attribution (`claude` vs `you`); evidence link opening the session Changes
view beside the dashboard; container-query collapse.

---

## Intent

Captured from the maintainer's design review (2026-07-15) — the full record is the
decisions block + §10 of [docs/agent-dashboard-plan.md](../agent-dashboard-plan.md).

- **Core bet (this page's reason to exist):** the dashboard is one leg of DESIGN.md's
  founding moat ("attention-aware multi-agent dashboard") — an *attention router into live
  sessions*, never a monitor that replaces them, and never a second IDE. The wedge is the
  attention lane: answering an agent's ask, workspace-wide, surviving laptop-close.
- **Landing:** on for everyone with an off switch; when nothing is running, *nothing
  dashboard-shaped shows* — just the launcher-style blank state.
- **Honest asymmetry is intentional:** cross-vendor status differs by tier (protocol /
  hooks / output-only) and the card must say so rather than fake parity.
- **The Mastermind (future phases, decided):** exactly one user-picked privileged agent
  per workspace, living only on the dashboard dock, may direct workers (message / spawn
  panes / interrupt / suggest); workers get observe + `ask_mastermind` only — nobody
  commands sideways. Its act tools are gated by an ask-first/auto mode routed through the
  agent's own permission harness (ask-first prompts land in this attention lane). No
  Mastermind → no `ask_mastermind` tool and the dock shows a setup card instead.
- **Deliberately deferred:** memory indexing (files-as-links first), cost on cards (needs
  the v0.2 feed), codex TUI telemetry (consent-gated by codex itself — pitched as a
  dashboard card), kanban-style managed columns (never — derived states only).
