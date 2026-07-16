# The workspace dashboard

The landing surface of a workspace: which agents need you, what everyone is doing, what
they produced, and where you left off — one glance, every element one click from the live
session. An empty workspace layout opens onto it; ⌘0 / the rail's `dashboard` row reach it
any time. This is the larger dashboard/Mastermind design
([docs/agent-dashboard-plan.md](../agent-dashboard-plan.md)) through v1: the surface
(v0.1), the status-feed depth (v0.2), and the Mastermind dock (v1).

**Where it lives (shared):** UI `web-ui/src/lib/dashboard/` (`DashboardView.svelte`,
`AgentCard.svelte`, `AttentionCard.svelte`, `MastermindDock.svelte`, `dash.ts`); the
`dashboard` surface in `web-ui/src/lib/layout/layout.ts` (`DashboardTab`, `openDashboard`,
key `v:dashboard`) and its branches in `Pane.svelte`/`PaneTabs.svelte`; the landing switch
+ rail row + ⌘0 in `web-ui/src/App.svelte` (`pruneAndAutoOpen`, `openDashboardSurface`,
`dashCtx`). The surface renders from the existing `/ws/events` roster, the chat journal
via `web-ui/src/lib/chat/chatPool.ts` (`acquireChat`/`peekChat`), the git status store,
and the rail's recents; the dock additionally rides the Mastermind daemon surface
(`PUT`/`DELETE /api/v1/workspaces/{id}/mastermind` in
`crates/chimaera-server/src/api/workspaces.rs`, the `mastermind` fields on the
workspace/session wire, helpers in `web-ui/src/lib/workspace/sessions.ts`).

## Landing & entry points

- **What & when.** Opening a workspace whose layout is empty lands on the dashboard instead
  of the first session. A restored non-empty layout is never touched.
- **How it's used.** Automatic (setting `dashboard.landing`, default `auto`; `never`
  restores the old first-session behavior). Manual: the fixed `dashboard` rail row above
  the terminals section, or Mod+0 (the same pinned-chord family as Mod+1–9; the digit `0`
  ride-along lives in `web-ui/src/lib/shared/keys.ts::chordDigit`).
- **Key behaviors.** Singleton tab (re-opening focuses it); serialized additively in the
  layout blob (`{v:"dashboard"}` — older builds skip it without resetting the layout).
  When **nothing is running** (no agents, no terminals, no Mastermind) the surface shows
  no dashboard chrome at all — a launcher-style blank state (brand mark, `+ new agent` /
  `+ terminal` / a quiet `+ mastermind` leading to the dock's setup card, recents,
  quick-open hint). A **bound Mastermind counts as something running**: the chrome shows
  (the dock plus an honest "No workers running yet." roster area with the same spawners)
  instead of the blank state. Before the first session snapshot it shows a skeleton,
  never a false "everything died".

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
  dashboard (`LayoutCtrl.openChangesFrom`). A work line ("✳ 2 subagents · 1 background
  task") expands in place to per-row detail — live subagents ∪ background tasks on the
  shared `WorkTray` shell, with stop controls (claude chat: `{type:"stop_task"}` for both
  kinds) — the `AgentsTray`/`BackgroundTray` derivations promoted workspace-wide. Cards
  without a warm store get the same drop-down fed by the wire `subagents` field (claude
  TUI hooks): label + relative age, read-only — hooks can't stop a TUI subagent, and the
  wire and store rows never merge (double counting).
- **Key behaviors.**
  - **Provenance is worn openly**: every card carries its fidelity tier — `protocol`
    (chat, authoritative) › `hooks` (claude TUI) › `output-only` (other TUIs) — with a
    tooltip saying what that means (`dash.ts::provenanceOf`). Output-only cards read the
    daemon's output-recency signal for the now-line ("working — terminal output flowing" /
    "quiet — no recent output"); "state unknown" survives only on old daemons. Hooks
    cards read the v0.2 status-feed fields: the wire `now_line` ("ran Bash" /
    "editing foo.rs") beats the last-file-edited fallback, and `stalled: true` (the
    record claims running but the PTY has been silent 3+ min) overrides the now-line
    with a warn-toned "stalled — no output for 3+ min" — the claim is likely stale and
    the card says so.
  - **Density-adaptive**: one agent → a hero card (plan snapshot, subagents open by
    default); 2–6 → a card grid; 7+ → compact triage rows. The side column collapses under
    the pane's own width (container query), not the window's.
  - **Bounded rich detail**: warm chat stores are acquired for attention-lane chat
    sessions always, plus running chats up to a small cap (`RICH_CAP = 4`), so the
    dashboard can never churn the chat pool's LRU out from under open tabs; released on
    unmount (`releaseChat`, never `disposeChat`). Cards beyond the cap render wire truth
    only — empty is honest, fabricated is lying.
  - **Cost + context ride the v0.2 `usage` field** (the claude-TUI statusline heartbeat):
    with no warm store the same context meter renders from `usage.context_pct` (the
    tooltip names the model when the heartbeat carries one) and a quiet mono `$0.42`
    joins the footer from `usage.cost_usd`. Chat rows carry `usage` null on the wire —
    their meter stays store-derived and a chat cost line is a later pass.
- Plain shells collapse to one summary row ("N terminals · M running a command") with
  per-shell chips, plus `+ terminal` / `+ agent` spawners.

## The activity column

- **Changed files** — newest-first union of every agent's `files_touched` (attributed with
  a per-agent chip) and uncommitted git paths no agent claimed (chip `you`, tooltip states
  attribution is best-effort). Click opens the file in the active pane. Cap 10.
- **Recents** — the rail's daemon-persisted recents, resumable, cap 5. Shown only in
  quiet moments (attention lane empty, no agent mid-work) — during live work the column
  stays lean; the blank state keeps its own recents regardless.
- **Git** — branch, ahead/behind, change count; opens the source-control panel. Reads the
  live `gitStatus` store (epoch-driven, never polled).

## The Mastermind dock

- **What & when.** The one home of the workspace's privileged agent (plan §7: exactly one
  per workspace, it delegates rather than does): a full-height third column right of the
  activity column, `MastermindDock.svelte`. Until one exists the dock is a **setup card**
  — brand mark, plain-English help (sees every session, answers questions, delegates
  work; never does the work itself; bills as your own account), the agent choice, the
  ask-first/auto mode choice, and Start.
- **How it's used.** Start `PUT`s `/workspaces/{id}/mastermind {agent, mode, theme}`; the
  daemon creates the chat session AND binds it in one step and the dock swaps to the
  identity header (mark · "Mastermind" · agent + mode chips) over an **embedded
  `ChatView`** — the same component the panes use, on the same chat pool, so permission
  prompts of an ask-mode Mastermind render inline in its own chat (never in the attention
  lane). The header's `⋯` menu offers "switch to ask/auto" (an inline confirm — a mode
  change is a re-PUT that restarts the session; a running claude never re-reads its
  settings) and "retire" (inline confirm → `DELETE`, which unbinds and ends the session).
- **Key behaviors.**
  - **Ask first vs auto** is worn on the chip: ask-first means acting on the workspace
    raises the agent's own permission prompt (reads never ask); auto acts without asking,
    every act audited. The gating rides the agent's native harness, set at spawn.
  - **Claude-only in v1**: codex shows in the setup card but disabled, wearing the
    server's refusal verbatim (no per-tool permission gate for MCP calls, so ask-first
    can't be enforced) — the same 400 the daemon would return. PUT errors (the 409
    missing-binary conflict included) surface inline in the server's own words.
  - **Reactive-only**: nothing in the UI ever triggers a Mastermind turn — no briefing
    prompt on setup, no event-nudged sends. It speaks when the user types.
  - **The observer, not the observed**: session rows flagged `mastermind: true` are
    filtered out of the rail, the roster/lane, the chord map, quick-open, the home-screen
    rollups, and the recents-adjacent surfaces. The dock is the only place it renders.
  - **Collapse**: panes ≥ ~1240px (the pane's own measured width, not the window's) get
    the docked 360px column with a header `»` collapse; narrower panes get a slim
    right-edge pill ("mastermind" + the mark, plus an amber dot when it waits on you)
    that opens the dock as a right-pinned overlay. No horizontal scroll, no overlap.
  - **Honest gone-state**: a binding whose session is missing/dead says "the Mastermind
    session is gone — set it up again" with a reset (DELETE, then the setup card) —
    never a ghost chat.

## Key constraints

- **Status must be honest** (design spine): a card can never fake-green; every state comes
  from the same `agent_state`/`dotState` vocabulary the rail uses.
- **No new wire, no new routes** in v0.1 — the surface composes existing client state; the
  daemon is untouched.
- The return strip is the workspace vital-signs line: name · branch chip · the summary
  sentence ("2 working · 1 waiting on you · 1 finished" — counts agents; terminals
  summarize separately) · a compute chip when the daemon sees a scheduler (`computeStatus`:
  Slurm running/pending counts, or a live walltime countdown inside an allocation; nothing
  renders otherwise — never a queue table).

**Verified live (2026-07-15):** empty-workspace landing + blank state; spawn terminal +
claude chat from the dashboard; the attention lane rendering a real Write permission and
answering it inline (file created); provenance/now-line/ctx meter on the hero card;
changed-files attribution (`claude` vs `you`); evidence link opening the session Changes
view beside the dashboard; container-query collapse.

**Verification pending (Mastermind dock, 2026-07-16):** setup → embedded chat → mode
switch → retire; the blank-state variants (`+ mastermind`, bound-but-no-workers chrome);
the collapse pill at narrow pane widths; hidden-from-roster across rail/lane/chords.

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
