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
via `web-ui/src/lib/chat/chatPool.ts` (`acquireChat`/`releaseChat`, refcounted), the git status store,
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
  ride-along lives in `web-ui/src/lib/shared/keys.ts::chordDigit`). The
  `dashboard.cardDensity` setting keeps roster cards comfortable, forces compact rows, or
  retains the automatic seven-agent threshold.
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
  wire and store rows never merge (double counting). The "still working off-screen" CUE
  (a pulsing state dot for an idle turn with live background work) needs no warm store at
  all: it reads the wire `background_running` count, the same predicate the rail glyph
  uses, so a card and its rail row always agree.
- **Key behaviors.**
  - **Provenance is worn openly, in plain words**: the fidelity tier is `protocol`
    (chat, authoritative) › `hooks` (claude TUI) › `output-only` (other TUIs)
    (`dash.ts::provenanceOf`), but the card never shows the raw jargon. A chat row's
    kind chip (`claude · chat`) already says it's the authoritative tier — the tier
    story rides its tooltip. Only the degraded tiers wear an extra chip, worded for a
    human: `status via hooks` / `output only`. Output-only cards read the
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
  - **Calm ordering**: the roster sorts live-before-dead only (`dash.ts::rosterWeight`),
    NOT per activity state — a chat agent cycles running↔finished every turn, and a
    roster that re-ranks on each turn boundary reads as chaos (the state dot + strip
    sentence + attention lane already say who's working). Within a bucket the order is
    stable (created_at). Cards that REORDER within a list (the live-before-dead resort, a
    new sibling shifting the grid) glide via Svelte `animate:flip` rather than teleporting
    (zeroed under reduced motion); a card crossing between the lane and the roster is an
    add+remove across two lists, so that boundary cuts — fine, since it marks an
    attention-state change worth noticing.
  - **Unread output**: an agent whose whole turn has FINISHED (the main turn handed the
    floor back AND no subagents are still on the wire) while it is NOT the focused session
    earns a quiet "unread" cue — a bolder name (the unread-mail convention) over a
    barely-there accent wash on the roster card, and just the bolder name on the dense rail
    row / focus-strip chip (`workspace/unread.svelte.ts`). It is deliberately NOT triggered by mid-turn output: a
    paused stream (a hook-less TUI thinking, a gap between tool calls) is still "working",
    so a genuinely busy session never wears the mark. Per-window and in-memory ("unread"
    is about this viewer's eyes, not daemon state); focusing the session clears it. Never
    a state color — the state dot owns state; the highlight only says "new".
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
  **settled** quiet moments (attention lane empty, no agent mid-work) — during live work
  the column stays lean, and quiet must HOLD for a beat (~10s) before recents reappear so
  the per-turn idle gap of a chat agent doesn't flicker the section in and out (hiding
  stays immediate — kicking off work is a deliberate moment). The blank state keeps its
  own recents regardless.
- **"Last active"** — above the columns, a one-click jump back into this window's most
  recently active agent (shown only when there's more than one agent, so it isn't the
  whole story). Named for what it IS (the row you were last in), not the old bare
  "continue" that read like a stuck state.
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
  identity header (mark · "Mastermind" · agent chip · the clickable `acts:` gate badge)
  over an **embedded `ChatView`** — the same component the panes use, on the same chat
  pool, so permission prompts of an ask-mode Mastermind render inline in its own chat
  (never in the attention lane). The header's `acts:` badge (and its `⋯` menu) switches
  the gate with an inline confirm — a mode change is a re-PUT that restarts the session;
  neither agent re-reads its gating after spawn — and the menu's "retire" (inline confirm
  → `DELETE`) unbinds and ends the session.
- **Key behaviors.**
  - **The act gate (ask first vs auto) is its own badge — `acts:`** — deliberately named
    for what it gates (the Mastermind's OWN workspace acts), not to be confused with the
    embedded agent's native permission-mode picker in the chat header. Ask-first raises
    the agent's own permission prompt on an act (reads never ask); auto acts without
    asking, every act audited. The gating rides the agent's native harness, set at spawn.
  - **The two-mode caveat (claude, ask mode).** The ask-first gate works by NOT
    pre-allowing acts — which only bites while claude's own permission mode actually
    asks. If the user flips claude's native mode to auto/bypass (its header picker or
    shift+tab), the dock says so in a warn line rather than wearing an `acts: ask first`
    badge that's silently moot. Codex has no such caveat — its gate is the driver
    answering elicitations, which no native mode bypasses.
  - **Claude or codex.** Both enforce the mode through their own harness: claude via
    the generated settings' pre-allows; codex via the driver answering its MCP
    tool-call elicitations from the recorded mode (its app-server elicits every MCP
    call regardless of approval-mode config — live-probed, PROTOCOL.md Pass 19 — so
    the pre-allow answers at the prompt). Both gates generate from the one shared
    read-tool list, so ask-mode semantics are identical: reads silent, acts prompt.
    A mode switch keeps the bound agent. PUT errors (the 409 missing-binary conflict
    included) surface inline in the server's own words.
  - **Reactive-only**: nothing in the UI ever triggers a Mastermind turn — no briefing
    prompt on setup, no event-nudged sends. It speaks when the user types.
  - **The observer, not the observed**: session rows flagged `mastermind: true` are
    filtered out of the rail, the roster/lane, the chord map, quick-open, the home-screen
    rollups, and the recents-adjacent surfaces. The dock is the only place it renders.
  - **Collapse + resize**: panes ≥ ~1240px (the pane's own measured width, not the
    window's) get the docked column with a header `»` collapse; narrower panes get a slim
    right-edge pill ("mastermind" + the mark, plus an amber dot when it waits on you)
    that opens the dock as a right-pinned overlay. The docked column is **drag-resizable**
    from a left-edge handle (the rail-resize idiom: drag to size, double-click to reset;
    width persists per browser profile) all the way up to the **whole surface** — dragging
    past the clamp, or the header's `⤢` button, expands it to fill the dashboard (a
    transient focus mode, not persisted). No horizontal scroll, no overlap.
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

**Verified live (2026-07-16, billed real-agent runs):** the v0.2 feed on a real claude
TUI (hook state transitions, `now_line` per tool, statusline `usage` — ctx meter +
footer cost on the hooks-tier card — `subagents[]` around a real Task fan-out, and the
full `stalled` lifecycle via a SIGSTOP-frozen process flipping true at ~3 min and
clearing on resume); the Mastermind end to end — setup card → ask-mode session whose
generated settings pre-allow exactly the read tools → `workspace_status`/`read_session`
answered with zero prompts → `spawn_agent` + `message_agent` each raising claude's
native permission card inline in the dock → the attributed `[from the workspace
Mastermind]` hand-off kicking off a worker whose own Write permission landed in the
attention lane and was answered there (docstring on disk); daemon restart resurrecting
the binding, the mode, and the pre-allow; the `⋯` mode switch to auto (re-PUT, whole
`mcp__chimaera` pre-allow) acting with no prompt + the `tracing` audit line; codex chat
carrying the injected MCP URL, listing and calling `chimaera.list_terminals` with its
approval answered from chimaera (post-fix — see PROTOCOL.md Pass 18); blank-state
`+ mastermind`; hidden-from-roster across rail/roster/home rollups; light + dark.

**Verified live (2026-07-16, codex-as-Mastermind, billed):** the setup card appointing
a **codex** Mastermind through the UI; ask mode — `workspace_status` running with zero
prompts (three separate turns) while `spawn_terminal` raised the native permission card
inline in the dock and ran on Allow (terminal in the roster); auto mode — both tools
unprompted, `tracing` audit lines for each act; the role prompt via
`-c developer_instructions` ("I'm the workspace Mastermind, overseeing and delegating
work"); a daemon restart resurrecting the codex Mastermind with binding, mode, and
driver pre-approval intact; retire → setup card → re-appoint through the card. The
gating mechanism (driver-answered elicitations) exists because codex's app-server
elicits every MCP tool call regardless of approval-mode config — five live probes,
recorded in PROTOCOL.md Pass 19. `chat-smoke` after the driver change: 16/16 (one
claude-side flake passed alone).

**Not exercised live yet:** the compute chip against a real Slurm scheduler (it reuses
`ComputeStrip`'s exact parsing; needs a Sherlock pass); the output-only TUI now-line
flip on screen (the shared `output_active`/`dotTitle` path shipped verified in #59);
the store-tier work drop-down rendering subagents ∪ background tasks simultaneously.

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
- **The Mastermind (v1 shipped 2026-07-16):** exactly one user-picked privileged agent
  per workspace, living only on the dashboard dock, may direct workers (message / spawn /
  interrupt); nobody commands sideways. Its act tools are gated by an ask-first/auto mode
  routed through the agent's own permission harness (its prompts render in its embedded
  chat). Reactive-only in v1 (maintainer, 2026-07-16): nothing triggers a turn but the
  user typing — `suggest` and worker-side `ask_mastermind` moved to v1.x (fire-and-forget
  queueing would trigger unprompted billed turns). No Mastermind → the dock is the setup
  card.
- **Deliberately deferred:** memory indexing (files-as-links first), chat-card cost
  (TUI cards carry it from the statusline heartbeat; the chat wire doesn't yet), codex
  TUI telemetry (consent-gated by codex itself — pitched as a dashboard card),
  kanban-style managed columns (never — derived states only).
