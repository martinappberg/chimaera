# The Agent Dashboard — design & plan

Status: **design draft for discussion** (2026-07-15). Nothing here is built.
This document synthesizes a research pass over the codebase, the current
Claude Code and Codex integration surfaces (verified July 2026), and prior art
(Conductor, Crystal, claude-squad, Vibe Kanban, Sculptor, Cursor agents, Codex
cloud, Devin, Terragon, Omnara, cmux, disler's hooks-observability), plus a
four-lens design panel (mission-control / workbench-home / mastermind /
skeptic). Decisions marked **[decide]** are the maintainer's call.

## 0. The one-paragraph version

A per-workspace **dashboard surface** — the page you land on when a workspace
has no layout yet — that answers, in five seconds: *which agents need me,
what is everyone doing, what did they produce, and where was I?* It composes
almost entirely over plumbing that already exists (`/ws/events` roster,
`agent_state`, `files_touched`, `pending_permission`, the chat journal, the
claude hook pipeline, the per-session MCP server). Beneath it, a **workspace
status service** (the "mastermind"): one normalized feed folded from chat
protocol events + TUI hooks, exposed back to the agents themselves through the
existing `chimaera` MCP server — so any agent (claude *or* codex) can ask
"what's going on in this workspace?", and — behind explicit per-capability
user grants — act on it. The concierge ("chat with your workspace") is then
not a feature: it's a normal chat session of the user's own agent with that
MCP attached.

This is not a new direction. DESIGN.md's founding moat already names an
"attention-aware multi-agent dashboard" as one of its four legs, and M2
shipped its seed (the attention state machine + session strip). This plan is
that leg, grown up.

## 1. What already exists (the surprising inventory)

The research pass found the vision far more built than expected:

| Primitive | Where | State |
|---|---|---|
| Per-session attention state machine (`running / needs_permission / idle_prompt / finished / errored / rate_limited / unknown`) | `agent_state.rs`, `sessions.ts` | shipped; drives rail dots + home-screen rollup |
| Claude TUI hooks → daemon | `agents.rs`: generated `--settings`, 8 events, HTTP POST to `/agent-events/{id}?key=` | shipped |
| `files_touched` per agent (PostToolUse) | `AgentRecord`, on the wire | shipped, cap 100 |
| Chat journal: turns, tools, subagents, permissions, cost, context %, plan entries, rate windows | `chimaera-agent/src/journal.rs` + `model.rs` | shipped, both vendors |
| Daemon-wide status bus | `/ws/events` full snapshots, ≤4/s, `state.changes` Notify | shipped — **the dashboard's core feed already exists** |
| Per-session MCP server, key-in-URL auth | `mcp.rs` (`/mcp/{id}?key=`), injected via generated `--mcp-config` | shipped (linked-terminal tools) |
| Consent model precedent | `links`: user grants terminal↔agent edges; agents cannot self-link; `@term:` mention = consent | shipped |
| Exec-into-shell + command journal | `exec.rs`, OSC 133 journal; **refuses agent sessions (409)** | shipped; the 409 is a deliberate wall |
| Subagents visibility (claude chat) | `task_started/progress/notification` → Agent-kind ToolCalls → `AgentsTray` | shipped, per-chat only |
| Session identity that survives restarts & view toggles | ledger + `AgentRecord` | shipped |

**The gaps** (everything the plan below adds): a workspace-scoped UI surface;
codex TUI telemetry; TUI cost/context; subagent visibility *workspace-wide*;
a persisted recent-files aggregate; workspace-status MCP tools; a REST/MCP
chat-send; grants/audit for agent-initiated actions; memory.

## 2. Agent integration surfaces (verified July 2026)

### Claude Code (~v2.1.20x)
- **Hooks:** ~31 lifecycle events now, incl. `SubagentStart/Stop` (payload
  carries `agent_id`, `agent_type`, `last_assistant_message`,
  `tool_use_count`) — and tool events *inside* subagents fire with
  `agent_id` set. Native **`http` hook type** (`{"type":"http","url":…}`) —
  no curl subprocess needed. `"async": true` for fire-and-forget.
- **Injection:** `--settings <file>` is an additive layer; hooks in it are
  documented to work (we already rely on this). `--mcp-config` +
  `--session-id` as today. Zero dotfile footprint.
- **Statusline:** the statusline script receives rich JSON per assistant
  message — `cost.total_cost_usd`, `context_window.used_percentage`,
  `rate_limits`, `model` — a **free per-turn telemetry heartbeat for TUI
  sessions** if the generated settings add a statusline command that tees its
  stdin to the ingest endpoint.
- **Memory/transcripts:** auto-memory at `~/.claude/projects/<slug>/memory/`,
  CLAUDE.md hierarchy, transcript JSONL (presence version-dependent — the
  2.1.204 field note stands).
- MCP server name `workspace` is **reserved** by claude; ours stays `chimaera`.

### Codex CLI
- **Hooks: codex now has a Claude-shaped lifecycle hooks system**
  (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`,
  `PermissionRequest`, `SubagentStart/Stop`, `Stop`, …) configured in
  `~/.codex/hooks.json` / `[hooks]` in config.toml / project
  `.codex/hooks.json` — **but trust-gated**: non-managed hooks require the
  user to approve via `/hooks`, and project config loads only for trusted
  projects. Chimaera cannot (and should not) silently self-install them.
- **MCP:** `-c 'mcp_servers.chimaera.url=…'` dotted-key overrides are
  documented — per-session injection without touching `~/.codex` works. This
  closes the "codex MCP comes later" note in `spawn.rs`.
- **Rollout files** (`~/.codex/sessions/…/rollout-*.jsonl`) are tail-able for
  TUI sessions but schema-internal → pinned-not-trusted; treat as a
  fast-follow experiment, never the default path.
- **Subagents exist** (opt-in `[features] multi_agent/collab`), observable in
  app-server via `parentThreadId` and via `SubagentStart/Stop` hooks.
- Chat mode needs none of this — app-server protocol is already full-fidelity.

### The honest asymmetry (design input, not a bug)
Fidelity tiers, which the UI must wear openly: **protocol** (chat mode, both
vendors — authoritative) > **hooks** (claude TUI today; codex TUI after a
one-time user consent) > **liveness** (PTY output timestamps only) >
**none**. A liveness-only session never renders as confident green; codex TUI
before consent reads `unk · enable codex telemetry?`. "Status must be honest"
is already the design spine; the dashboard inherits it.

## 3. Prior-art patterns adopted (and pitfalls dodged)

Adopted: attention-need is the primary axis (everything ranks by "needs me");
status from lifecycle events plus an *independent* stall detector; diff +
evidence is the unit of "done" (a green dot without evidence lies); every
card is one click from the live session (Chimaera's unfair advantage: nobody
else has overview + real TUI + survives-laptop-close); ambient beats
destination (rail badge and tab pulse first, page second); agent-readable =
agent-drivable (cmux/Devin validate the MCP idea — and nobody has it
per-workspace, cross-vendor); derived states only, never managed kanban
columns.

Dodged: the bypassed splash screen (value must accrue with zero user effort);
becoming a second IDE (Crystal) or a ticket system (Vibe Kanban);
notification fatigue (batch, tier, quiet means quiet); wrapper fragility
(no PTY scraping for status — hooks/protocol only, with liveness as the
fallback); building the orchestration layer without owning the runtime
(Terragon died of this; Chimaera owns the runtime).

## 4. The surface (web-ui)

### Placement
- `DashboardTab { surface: "dashboard" }` in `layout.ts`, `tabKey →
  "v:dashboard"` — singleton for free, exactly the Settings/Git pattern.
  Branch in `Pane.svelte`, label + glyph (BrandMark monogram, 12px) in
  `PaneTabs.svelte`, serializer case.
- **Landing:** in `pruneAndAutoOpen()`, an *empty* restored layout opens the
  dashboard instead of the first session. A non-empty layout restores
  untouched — it earns the center by taking the stage only when the stage was
  empty. Setting `dashboard.landing: auto | always | never` (default `auto`).
- **Ambient entries:** fixed rail row above the terminals section (glyph +
  "home" + amber count when attention > 0), `⌘0`, tab-badge pulse on
  attention while unfocused. No OS notifications in v1.

### Layout (max ~1040px centered; `1fr + 300px` grid, single column <880px)

**A. Return strip** — workspace name, branch/worktree chip, host label; one
honest sentence composed from the roster ("2 working · 1 waiting on you · 1
finished"), each segment tinted by its state token and clickable; and
**Continue:** the most-recently-active session as a one-keystroke resume
(Enter opens it). The five-second answer.

**B. Attention lane** (renders only when non-empty; quiet line otherwise) —
every `needs_permission / idle_prompt / errored` session as a wide card,
ranked by wait time. For **chat** sessions the journaled `PermissionRequest`
payload (title, `input_preview`, options) renders **inline with its option
buttons**, wired to `AgentCommand::Permission` over the existing chat socket
(chatPool-warm). Answering a permission without opening the pane is the
dashboard's first daily-value moment. For **claude TUI** we only know *that*
it waits (Notification hook): the card says so and offers "open terminal".
Honest asymmetry, surfaced not hidden.

**C. Roster** — density-adaptive on the same data:
- **1 agent (the common case):** a hero card — state, now-line, plan-panel
  snapshot, inline permission, subagents, evidence row, and a send-only
  composer (no slash palette, no attachments — the second-IDE trap starts
  there). For a single TUI agent: no fake composer.
- **2–6:** cards, `auto-fill minmax(300px, 1fr)`; errored → running →
  finished → unknown. Plain shells collapse to one summary row.
- **7+:** compressed triage rows.

Card anatomy (top→bottom: *who, doing what, needs what, produced what*):
SessionGlyph + state dot (`dotState()` classes) + `displayName` + model chip
(canonical vocabulary — `xhigh` stays `xhigh`) + **provenance glyph**
(protocol/hooks/liveness/none, with a `dotTitle`-style tooltip); a one-line
now-line (current plan step, else last ToolCall title, else last hook signal,
else honestly nothing); subagent line ("✳ 2 subagents · 14 tools · 31k tok" —
the `AgentsTray` derivation promoted workspace-wide, expandable, per-agent
stop where supported); meter row (context bar, amber >80%; cost for claude,
token totals for codex — never fake a unit the vendor doesn't report);
evidence row on finished/errored ("+142 −38 across 6 files · view diff" →
Changes tab scoped to that session's files). Disclosure ladder: glance →
peek (hover popover: last ~10 journal blocks via one bounded `attach` replay,
strictly one peek at a time) → enter (click: open in active pane; dashboard
stays alive behind it) → split-enter (⌥-click).

**D. Activity column** — recent files (union of `files_touched` across
sessions, attributed by agent glyph, + git-status paths attributed "you";
click opens the file), recents (the existing store, finally discoverable),
git summary (ahead/behind + last 3 commits, epoch-driven).

**Empty workspace:** no fake dashboard — BrandMark intro, the Launcher rows
inlined, a quickopen hint.

**Restore race:** the dashboard renders a skeleton until the first
post-restore snapshot (field note: mid-restore rosters read as "everything
died").

## 5. The status service (daemon)

New module `crates/chimaera-server/src/mastermind/` (name **[decide]** —
`workspace_feed` may age better than `mastermind`):

- **`feed.rs` — WorkspaceFeed.** One fold, three inputs the daemon already
  has: the `ChatManager` `EventHook` (every journaled chat event, both
  vendors), TUI hook ingest (extended payload set), git/fs dirty signals.
  Normalized `StatusEvent v1` records (`{seq, ts, sid, agent, src:
  chat|hook|fs, ev, data}`, caps at construction), appended to
  `~/.chimaera/workspace/<ws-id>/feed.jsonl` with **exactly the chat-journal
  discipline** (gap-free seq, 4 MiB file compacting at turn boundaries,
  256 KiB line cap, in-RAM ring, shared 100 MiB directory budget). Hot
  derived state (per-session counters: cost today, last activity, subagent
  roster) is a `HashMap` in `AppState`, reconstructible from the ring.
- **Wire:** additive-only fields on the existing session rows
  (`attention_reason`, `provenance`, `last_evidence_ms`, `subagents[]`,
  `usage`, `now_line`) — the daemon↔UI wire is public; new optional fields,
  never reshaped ones. Same ≤4 snapshots/s throttle.
- **Claude TUI deepening:** regenerate `--settings` with native `http` hooks
  (all telemetry hooks `async`) for the full observer set — SubagentStart/
  Stop, PermissionRequest, PostToolUse with tool detail, TaskCreated/
  Completed — plus a statusline command that tees its stdin JSON to the
  ingest endpoint (per-turn cost/context/rate for TUIs). The launcher keeps
  scrubbing `CLAUDE_CODE_*` child markers (field note: leaked markers kill
  transcripts). **Never install a synchronous hook for telemetry.**
- **Liveness (the anti-lying layer):** `chimaera-pty` stamps
  `last_output_at` per session on write (a timestamp — the
  never-serialize-the-grid rule holds). `working` with no event and no
  output for 90s (setting, not constant) degrades to **`stalled`**; any byte
  revives. OSC 133 `running` suppresses the stall badge (long silent
  compiles). Independent of hooks, free, and something no scraper has.
- **Codex TUI:** a one-time consent card on the dashboard ("enable codex
  telemetry") writes project `.codex/hooks.json` and walks the user through
  codex's own `/hooks` approval. Until then: honest `unk`. `-c` injection of
  `[hooks]`/`notify` is unverified — experiment behind a flag, never the
  default. Codex chat MCP injection via `-c mcp_servers…` ships regardless
  (verify live first — repo rule).

## 6. The workspace MCP (agents observing, then acting)

Extend `mcp.rs` — same endpoint (`/mcp/{sid}?key=`), same stateless
streamable-HTTP, same per-session secret. Tools gated by a capability set.

**Observe (default grant for every session):** `workspace_status` (bounded
roster + counters + git summary + active subagents), `list_agents`,
`agent_transcript_tail {sid, n≤50}` (compact rendered events — never raw
grids), `recent_files`, `shell_journal` (stays link-scoped),
`list_surfaces`. The last needs the one new client obligation: alongside the
opaque view-state blob, the client PUTs a tiny normalized **surface
manifest** (`{surfaces:[{surface:"file",path}|{surface:"terminal",sid}|…]}`,
≤4 KB, versioned, additive) — "what's open" becomes machine-readable without
teaching the server the layout tree.

**Act (each individually user-granted):** `message_agent {sid, text, mode:
propose|send}` — `send` maps to `AgentCommand::Send` on the existing pump
(chat sessions only; **TUI targets are propose-only forever**; the exec-409
wall stands); `spawn_agent` (through the normal spawn path — env scrubbing
and `--session-id` minting stay centralized); `interrupt_agent` /
`stop_subagent` (never kill); `open_in_pane` (a UI *intent* frame on
`/ws/events`, executed by the focused client through its normal open path).

**Grants** (`grants.json`, atomic tmp+rename): tiers `observe` →
`message.propose` (default: proposals are inert cards on the dashboard and
the target's rail row; the user clicks to deliver) → `message.send` /
`spawn` / `interrupt` / `ui.intent`. First over-grant call returns a
structured `needs_grant` error and raises an amber consent card ("claude
s-9f2c wants to send messages to other agents — allow once / always /
never") — the `@term:` linking idiom generalized. No agent may grant to
itself or another agent. Act-tier calls are rate-limited (e.g. 6 sends/min).

**Audit** (`audit.jsonl`, 4 MiB cap): every act-tier call — proposed,
executed, denied, expired — with caller, tool, target, resolver. Rendered as
a collapsible "agent actions" strip on the dashboard, and itself readable via
`workspace_status`: agents can see what agents did.

### Threat model (read before shipping any act tool)
The mastermind is a **confused-deputy amplifier**: agent A ingests poisoned
content (a README, a web page, tool output), plants "run X in terminal 3" in
its transcript; agent B (the concierge) reads it via the MCP and — with act
grants — obeys. Mitigations, in order of necessity: (1) v1 MCP is
**read-only**; (2) actuation is per-capability, per-target, user-granted,
never self-escalating; (3) **provenance stamping** — every MCP-initiated
send renders in the target's journal and UI as `via chimaera MCP (from
session X)`; (4) data minimization — status by default, transcript bodies
behind a separate grant (cross-context privacy: the repo-cleanup agent must
not read the HR-adjacent chat); (5) relayed text is wrapped in an explicit
untrusted-content frame; (6) the concierge spawns/resumes only through the
daemon's normal paths.

## 7. The concierge ("chat with your workspace")

Zero new engine: a normal chat-mode session of the user's configured default
agent (claude or codex — both drivers exist), `cwd = workspace root`, the
`chimaera` MCP attached, a one-time user-approved grant bundle, and a
Chimaera-shipped **skill** (claude: injected skill dir; codex: a generated
AGENTS.md layer) teaching the tool grammar: triage by attention state, cite
session ids, prefer propose when uncertain, never spawn without stating why.
It bills as the user's own account — the two-tier honest-billing bet intact.
The dashboard embeds the existing `ChatView`; the subagents tray and plan
panel come along free. Skills are for the orchestration layer — **not**
needed for status (hooks + MCP injection at spawn cover that).

## 8. Memory — deliberately last

The skeptic lens wins here: don't build the index before anyone asks a
question. Phase 1: surface CLAUDE.md / AGENTS.md / claude auto-memory as
read-only links on the dashboard and via MCP file reads — 70% of the value at
2% of the cost. Phase 2 (when real questions demand it):
`memory.jsonl` card records (turn summaries by pure extraction — no LLM
calls; doc chunks mtime-gated) + an in-RAM BM25-ish inverted index,
reconstructible, dropped under memory pressure, 20 MiB inside the shared
budget. No embeddings, no database — HPC rules. Honest tradeoff: lexical
search misses paraphrase; acceptable for a small, jargon-heavy corpus.

## 9. Phasing

| Phase | Ships | New consent surface |
|---|---|---|
| **v0.1 — the surface** (days) | DashboardTab + landing switch + return strip + attention lane w/ inline permissions + roster cards + activity column — all from the existing wire. Rail row, ⌘0. | none |
| **v0.2 — honest depth** | Additive wire fields + feed fold; claude TUI http-hooks upgrade + statusline heartbeat; PTY liveness → `stalled`; recent-files JSONL log; codex chat MCP injection | none |
| **v0.3 — agents can look** | Observe MCP tools + surface manifest; codex TUI hook consent flow | codex `/hooks` walk-through |
| **v1 — supervised action** | Grants + propose/send + spawn/interrupt + audit strip; provenance stamping; REST/MCP chat-send | per-capability grant cards |
| **v1.x** | Concierge preset + skill; memory phase 1 (files), then phase 2 (index) if pulled | grant bundle |

Each phase is independently verifiable live (`verify-app`) and useful alone.
The **wedge** (v0.1's reason to exist): *the attention queue with inline
permission payloads, workspace-wide, surviving laptop-close.* Overview +
persistence + honest cross-vendor state — no competitor has all three.

## 10. Open questions **[decide]**

1. Tab label & noun: "home" vs "dashboard" (rail row says `home`?).
2. Default landing mode `auto` (empty-layout-only) — or `always` for a true
   "center page"?
3. Module/name for the status service: `mastermind` is fun; `feed` is boring
   and durable.
4. Does v0.1 ship behind a setting, or on for everyone?
5. Concierge default agent: the user's `agents.defaultView` agent, or an
   explicit picker on first open?
6. How loudly to pitch the codex hooks consent (dashboard card vs settings
   page only)?

## Appendix: what we deliberately do NOT build

Kanban columns (derived states only) · cross-host federation (per-daemon by
design, after-1.0) · PTY screen-scraping for status · an OTLP receiver
(heavier and poorer than hooks) · transcript search index in v1 · unprompted
agent-commands-agent (possibly never default-on) · a full composer on
dashboard cards (send-only, or it's a second IDE) · claude-transcript-watch
as a status source (hooks exist; scraping is the fragile road).
