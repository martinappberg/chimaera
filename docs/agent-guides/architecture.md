# Chimaera — Architecture

> The deep architecture + rationale: the source of truth for **how** Chimaera is
> built and **why**. The high-level [DESIGN.md](../../DESIGN.md) links here, and the
> nested `AGENTS.md` maps point at the section they need. Docs drift — verify a
> detail against the code before relying on it, and fix this file when you find it
> wrong (see the anti-drift note in the root [AGENTS.md](../../AGENTS.md)).

## Architecture

```
┌─ laptop ────────────────────┐        ┌─ HPC login node ─────────────────────────┐
│ chimaera CLI / native shell │  ssh   │ chimaerad (one static musl binary)        │
│   └─ browser / webview UI ──┼────────┼→ localhost:port (bearer token)            │
│      xterm.js·previews·git  │ -L fwd │   ├─ workspace registry (folders)         │
└─────────────────────────────┘        │   ├─ session supervisor                   │
                                       │   │   ├─ claude (stream-json / PTY+hooks) │
        any browser, zero install      │   │   ├─ gemini --acp / codex-acp         │
                                       │   │   └─ plain shells (PTY)               │
                                       │   ├─ file service (previews, Arrow paging)│
                                       │   ├─ git service · Slurm poller           │
                                       │   ├─ event bus (seq-numbered replay)      │
                                       │   └─ notifier (ntfy/webhook)              │
                                       └───────────────────────────────────────────┘
```

### One binary, two roles

`chimaera` compiled to fully-static musl (x86_64 + aarch64) — the ripgrep/uv distribution
model. Runs on RHEL 8 (glibc 2.28), no root, no containers. Conveniently, Claude Code itself now
ships as a native binary with musl variants and installs to `~/.local` without root, so the
whole stack needs only SSH + `$HOME`.

- `chimaera serve` — the daemon (chimaerad).
- `chimaera connect <host>` — client side: shells out to **system ssh** (inheriting
  ControlMaster, ProxyJump, RemoteCommand, Duo/2FA from `~/.ssh/config` — deliberately fixing
  Zed's [#25896](https://github.com/zed-industries/zed/issues/25896) HPC gap), pushes/updates
  the musl binary into `~/.chimaera/bin` on first use (Zed's model, with a manual-upload
  fallback for air-gapped clusters), starts-or-finds the daemon, forwards the port, opens the
  UI.
- `chimaera doctor` — per-site sanity checks (glibc, quotas, cgroup policing, tmp-scrubbing,
  outbound HTTPS, login-node process-reaper policies).

### Transport: SSH only, lossless reconnect

No new listening ports, no relay, no inbound firewall asks — ever. The daemon binds a Unix
socket + localhost-only TCP with a per-daemon bearer token (localhost on a multi-user login
node is not trusted alone).

Reconnect semantics are Eternal-Terminal-style at the application layer: structured streams
carry monotonic per-session sequence numbers backed by a bounded replay ring; a reconnecting
client sends `{session_id, last_seq}` and receives only the gap. (Implemented for chat
sessions — journal + ring in `chimaera-agent`; the session event bus itself stays
full-snapshot by design.)

**Critical correction from adversarial review:** raw byte replay is *not* sufficient for PTY
panes — it breaks on resize and multi-attach at different dimensions. Terminal panes therefore
keep **full server-side terminal state** (headless `alacritty_terminal::Term`, tmux's actual
model) and re-render for late joiners; seq-replay applies to the event bus and structured
streams. This exact pattern was independently proposed in Zed's pty-host RFC
([zed#50584](https://github.com/zed-industries/zed/discussions/50584), Mar 2026) — validated,
with one amendment from verification: **don't serialize `Term` grid state across the wire**
(that only works when both ends share the same Rust crate; our client is xterm.js). Instead,
on attach/resize the daemon re-emits an escape-sequence snapshot rendered from the grid (the
`@xterm/headless` + serialize-addon model, reimplemented server-side), explicitly carrying
window title, cursor style, and tab stops. This is the single most underestimated component in
every multiplexer project — budget accordingly.

**Resize repaint refinement (terminal robustness pass, 2026-07-07).** "Re-emit on resize" is
scoped to clients that did NOT initiate the resize: the initiator's xterm reflows natively, and
a full-reset repaint there is visible as flicker + scroll reset (it was the "terminal resets
when I change the font size" bug). Mechanics: (1) `Session::resize` no-ops on unchanged dims —
echoed dims from attached clients must never broadcast; (2) each ws connection tracks the dims
it last requested and skips the resync for matching `Resized` events; foreign resizes repaint
after a 120ms coalescing window (divider drags fire in bursts); (3) `resync` frames are
dimension-tagged and the client resizes its xterm to match BEFORE replaying — a snapshot
replayed at any other width re-wraps every soft-wrapped row wrong; (4) the auth frame carries
the client's current grid so the server adopts it before rendering the attach snapshot
(resizes that happen while the socket is down are otherwise lost forever — ResizeObserver
never re-fires for an unchanged container); (5) snapshots restore mouse/focus/keypad/auto-wrap
modes, which TUIs assert once at startup and never again. Known gap, accepted: a resync while
on the alternate screen cannot restore the primary screen's scrollback
(`alacritty_terminal` does not expose the inactive grid); with resyncs now rare
(lag/reconnect/foreign-resize only) the blast radius is small.

### State storage: HPC filesystem realism

- **No SQLite anywhere near NFS/Lustre** (WAL is unsafe across hosts; advisory locking is buggy
  on NFS; Lustre corrupts under concurrency).
- Hot state (socket, session index, replay rings) on node-local disk
  (`$XDG_RUNTIME_DIR`/`/tmp/chimaera-$UID`) — with the caveat that tmpfs is wiped on
  last-logout and cluster `/tmp` is night-scrubbed; the daemon must treat hot state as
  reconstructible.
- Durable state as append-only, size-capped JSONL under `~/.chimaera` (opencode's multi-GB
  unbounded-growth mistake is a named anti-goal; HPC home quotas are small).
- Claude's own `~/.claude/projects` JSONL transcripts are the agent's source of truth;
  chimaerad stores only an overlay (attention events, tags, Slurm links). Crash recovery is
  nearly free: cold-restart → re-attach every session via `--resume` with preserved cwd.
- A tiny `~/.chimaera/manifest.json` (hostname, port, 0600 token, pid, version) lets clients
  landing on a different round-robin login node discover and route to the node actually
  running the daemon. **Validate this at your own center in week 1** — some sites don't allow
  addressing individual login nodes.
- Resource discipline is a feature: <1 core steady-state, target ~150 MB RSS, no server-side
  rendering (tmux's CPU model, not zellij's ~4x), hard memory ceilings on preview extraction —
  Arbiter2-class login-node policing kills processes, and a Parquet spike must never take the
  daemon and every session with it.

### Agent integration: three tiers, one event model

All adapters normalize into one internal **ACP-shaped event model** (message/thought chunks,
tool calls with kinds + diffs, plan entries, permission requests with options, turn lifecycle).
The UI renders that model, so Claude/Gemini/Codex/PTY sessions all look the same.

**Tier A — PTY + injected hooks (the product's primary mode, by author decision 2026-07-06).**
Sessions run the **real interactive `claude` TUI in a daemon-owned PTY** — the same integration
mode as VS Code's integrated terminal, so it looks, behaves, and bills exactly like Claude Code
everywhere else (normal subscription limits). Chimaerad **never scrapes ANSI for state**; it
injects Claude Code hooks (`http` type: `Notification(permission_prompt|idle_prompt)`,
`PermissionRequest`, `Stop`, `StopFailure`, `SessionStart/End` → POST to the daemon) so TUI
sessions get reliable needs-attention/finished/errored badges, and rich *read-only* transcript
rendering comes from the `~/.claude` JSONL. This tier is billing-safe (see Risks),
agent-agnostic, and survives any protocol churn.

**Tier B — structured chat mode (SHIPPED 2026-07-07; the default view for new
claude/codex sessions).** Chimaerad drives the native binaries through their structured
protocols directly from Rust (no Node sidecar) — `claude` over bidirectional stream-json
(the same surface the official VS Code extension speaks), `codex` over `app-server`
JSON-RPC — behind a thin `AgentAdapter` trait in `crates/chimaera-agent`. The chat surface
covers tool cards with diffs, native permission cards (allow/always/deny), interrupt,
model + permission-mode pickers, slash-command and `@`-mention popovers (incl. `@term:`
linked-terminal grants), image paste, queued messages, markdown, plan panel, per-turn
cost/usage. Every session journals a seq-numbered event stream (append-only capped JSONL
under `~/.chimaera/chat/`, ring + gap-replay on reconnect), and the pane-bar toggle moves
one conversation between chat and the real TUI via resume on the same session id. Risk
posture, unchanged in spirit: the wire formats are unversioned, so drivers are pinned to
live-verified CLI versions (`TESTED_*_VERSION` + `just chat-smoke`; facts ledger in
`crates/chimaera-agent/PROTOCOL.md`), a per-spawn handshake watchdog degrades a failing
driver to a Tier A PTY on the same session id, and Tier A remains fully supported — one
settings default (`agents.defaultView`) flips the world back if the paused billing split
ever lands.

**Tier C — ACP client (other agents, post-v1).** `gemini --acp` natively; Codex via Zed's
`codex-acp`; the ACP registry (40–50 agents) for the long tail. ACP's
`session/request_permission` maps onto the same attention states.

**Attention states are first-class enums** — `running / needs_permission / idle_prompt /
finished / errored / rate_limited` — computed per tier, driving sidebar badges, the event bus,
and the notifier (ntfy topics + generic webhooks, dedup + quiet hours). Optionally spawn
sessions with `--remote-control` to free-ride Anthropic's mobile app and push notifications
instead of building any relay — correct scope discipline.

### Client: web-first, native shell later

**Client zero is a web UI embedded in the daemon** (rust-embed): any browser, zero install,
one SSH port-forward. This is the decisive call, and it deserves honesty since the founding
instinct was "Rust + GPU like Ghostty":

- The preview requirement decides it. MultiQC/FastQC/Nextflow reports are arbitrary
  self-contained HTML — the single format that keeps code-server installed today — and no
  GPU-native Rust toolkit renders HTML/notebooks/PDF. The native-purist design's own honest
  assessment: ~8–10 weeks rebuilding natively what a browser gets free, plus a fragile
  webview-compositing hack for the rest (which Zed, with a full team, has not shipped despite
  years of demand).
- The Rust/GPU instinct is still honored where it pays: the *daemon* is Rust and the deployment
  story (static musl, SSH-native) is the Ghostty-grade part. Terminals use xterm.js with the
  WebGL renderer (VS Code's stack) — fast enough for agent supervision, honestly not Ghostty
  for raw typing. You keep Ghostty for bare shells; Chimaera is the workbench.
- A **Tauri 2 native shell** wrapping the same UI is part of v1 (M6): real windows per
  workspace ("open folder → window" without a browser tab), menubar attention badge, native
  notifications. The one thing deliberately *not* on the roadmap is a bespoke GPU-native
  client (GPUI): adversarial review priced it at 18–24 solo-months on a pre-1.0
  Zed-source-as-docs framework, buying only raw-typing latency that WebGL xterm.js mostly
  matches — that's not the optimal build, it's a different and riskier project. The
  daemon/stateless-client split keeps that door permanently open if terminal feel ever becomes
  the real bottleneck.

**UI principle (author decision 2026-07-06): minimalist and professional.** Default to quiet —
generous whitespace, restrained color (state color appears only where state matters), no
chrome that doesn't earn its pixels; density is opt-in, not the default. The file tree and
preview pane are primary surfaces on equal footing with sessions — the window is
sessions + files + git, not a chat app with a file drawer.

Stack (decided 2026-07-06): **Svelte 5** + TypeScript + Vite, xterm.js 6.x (`@xterm/*` scope)
with the WebGL renderer — DOM renderer is the *only* fallback since 6.0 removed canvas, so
handle `WebglAddon.onContextLoss` explicitly; keep the Terminal instance out of `$state`
(plain non-reactive object, instantiate in `onMount`, dispose on teardown) — TanStack Virtual
(Svelte adapter) for large tables/lists, CodeMirror 6 read-mostly for code viewing
(framework-agnostic), pdf.js vendored.

Layout: left rail = workspace switcher + session list with attention badges + Slurm strip;
center = active session (structured transcript or terminal); right = file tree + preview pane;
Cmd-K global switcher across sessions and workspaces.

### In-window layout: panes, focus mode, cohesive type

*Added 2026-07-06 (author request). Builds after M2a.*

**Split panes.** Each workspace window holds a binary layout tree: internal nodes are
horizontal/vertical splits with a draggable ratio; leaves are panes. A pane hosts a *surface*
— a terminal session today, a file preview at M3, a dashboard later — so the layout layer is
deliberately surface-agnostic ("MultiQC report tiled next to the agent that produced it" is
the product thesis in one screen). Interactions:

- Keyboard: Cmd/Ctrl+D split right, Cmd/Ctrl+Shift+D split down, Cmd/Ctrl+Alt+arrows move
  focus between panes, Cmd/Ctrl+Shift+Enter zoom-toggle the focused pane (tmux prefix-z
  semantics: temporary fullscreen within the window, subtle indicator, same key restores).
  Pane-close via the session's own exit or a quiet hover control — never bound to Cmd+W in
  the browser (the tab-close collision; the Tauri shell can own Cmd+W properly).
- Drag and drop: drag a session row from the rail into a pane's edge drop-zones
  (left/right/top/bottom halves; center replaces); drag dividers to resize; drag a pane
  header to re-tile.
- The focused pane is always visually unmistakable (hairline accent on the pane border —
  the session-hue idea from the interaction model applies here first).

**Tabs within panes** (added same day). A pane holds a *stack* of surfaces with a quiet tab
bar (hidden when a pane has exactly one tab — no chrome tax for the common case): one active
tab per pane. Clicking a rail session opens it as a tab in the focused pane — or focuses the
existing tab if it's already open somewhere (VS Code semantics, no duplicates by default).
Drag tabs to reorder, drag between panes, drag to a pane edge to tear off into a new split;
middle-click closes a tab (closing a tab detaches the view, never kills the session — the
rail is the source of truth for lifecycle). Keyboard: Cmd/Ctrl+Alt+Left/Right cycles tabs in
the focused pane. Cmd+T/Cmd+W stay unbound in the browser (tab-collision; the Tauri shell
owns them properly). At M3, files open the same way: a preview tab in the focused pane.

**Modifier policy (post-audit, 2026-07-06): the terminal owns bare Ctrl, everywhere.** App
chords are Cmd-based on macOS and Ctrl+Shift-based on Linux/Windows (the terminal-emulator
convention). A workbench that steals Ctrl+D from a shell is broken by definition — the first
UX audit caught exactly this.

**The parity principle (author, 2026-07-06): every action has BOTH a keyboard chord and a
discoverable mouse path.** Keyboard for speed, mouse for discovery — neither is a fallback.
Concretely for panes:

- **Hover controls**: a quiet control cluster fades in (≤120 ms) at the top-right of the
  hovered pane — split right, split down, zoom, close view — invisible otherwise, so the
  common case stays chrome-free but every pane operation is one visible click.
- **Zoom is clickable both ways**: the hover cluster's zoom control enters it; the zoom
  badge shown while zoomed is a button that exits it. Double-clicking a tab also
  zoom-toggles its pane.
- **Double-click a divider** resets the split to 50/50.
- Every hover control's tooltip teaches its chord ("zoom ⇧⌘↵") — the mouse path is how the
  keyboard path gets learned.

**Every pane always has a top bar (author, 2026-07-06 — supersedes the earlier
hide-when-single-tab rule).** A slim (~26 px) always-present bar per pane: type glyph +
active tab name (dirty dot for edited files), sibling tabs as compact items, and the pane
controls living at its right edge (fade in on bar hover; the zoom badge stays persistent
while zoomed). You always know which document/session a pane shows; every pane always has a
drag handle; zoom/split/close always have a mouse home. Minimalism is preserved by making the
bar quiet, not absent.

**Surface parity (author, 2026-07-06):** terminal tabs and file tabs are the same kind of
thing — same anatomy (glyph + name + close), same drag behavior, same chords, same top-bar
treatment — differing ONLY in how new ones are created (sessions from the rail/launcher,
files from the tree/palette). Any asymmetry is a bug.

**The full drag grammar** (one mental model, everywhere):
- *Sources:* tabs, the top bar's empty area (drags the pane's active tab), rail session rows,
  file-tree entries.
- *Targets:* pane **edge bands** (~25%) → split that pane on that side; pane **center** → the
  dragged surface **becomes a tab** there (activated); a **tab bar** → insert at the pointer's
  position (insertion caret shown); **window edges** → full-height/width root split.
- *Rules:* moving a pane's last tab away collapses the pane; dropping a surface where it
  already lives is a no-op; anything droppable shows its translucent preview before release;
  Escape always cancels. Same-pane center drops don't duplicate — tabs are unique per
  surface, window-wide.

**Discoverability rules** (from first real use, 2026-07-06): every mode needs a mouse exit:
clicking the workspace name in the strip leaves focus mode (tooltip "show sidebar ⌘B") — a
mode you can only exit via a chord is a trap.

**Polish inventory (author, 2026-07-06 — standing micro-interaction rules for every build):**

- Controls appear on hover of their *owning region* only (pane controls on bar hover, row
  actions on row hover) — never on a whole-surface hover that lights up chrome everywhere.
- Tooltips: ~500 ms delay, then instant for siblings while one is warm (macOS behavior);
  always name the chord.
- Transitions: opacity/transform only, 80–120 ms ease-out, never layout-shifting; respect
  `prefers-reduced-motion`.
- Cursor discipline: grab/grabbing on drags, col/row-resize on dividers, pointer only on
  actual actions.
- Every button has hover, active (subtle press), and :focus-visible states from one token set.
- Drag ghosts: translucent chip, subtle shadow, no rotation gimmicks.
- Scrollable regions get soft edge fades when content overflows; scrollbars thin and quiet
  everywhere (terminal treatment is the reference).
- Loading: nothing for <150 ms, soft pulse to ~400 ms, spinner only beyond; no spinner storms.
- Numbers and timestamps use tabular-nums; truncation is middle-ellipsis for paths, end for
  prose.
- Empty states are quiet, specific, and actionable — never dead ends, never cheerful filler.

**Quality bar ("SOTA usable"):** divider drags and tab drags at 60 fps with translucent
drop-zone previews showing exactly where things land; transitions fast (≤120 ms) and few;
fully keyboard-operable; visible focus states; zero layout jank on resize (terminal refit is
debounced, never mid-drag).

**Focus mode.** Cmd/Ctrl+B collapses the rail to nothing; what remains is the **session
strip** (the tmux-style bottom bar already specced in the interaction model — this is its
first shipped increment): workspace name, one compact chip per session with state dot, the
focused one inverted, aggregate "N need you", host label. So even fully collapsed, the window
always says where you are — and the strip is precisely what scales to many windows later.
Zoom + focus mode together = one terminal, edge to edge, still one glance from total context.

**Layout persistence.** The layout tree is part of daemon-owned per-window view state (the
return-is-a-state-machine decision), so reload/reconnect restores panes, ratios, zoom, and
focus-mode exactly.

**Cohesive type.** The terminal ships a bundled open-source mono (JetBrains Mono, OFL — no
CDN, embedded in the binary like everything else) instead of the system ui-monospace default,
and the same face is used for the UI's monospace accents (session ordinals, paths,
breadcrumbs, the strip), so terminal content and chrome share typographic DNA instead of
feeling like two applications.

### Interaction model: naming, orientation, navigation

*Added 2026-07-06 from a dedicated research + dual-design pass (tmux/zellij mechanics,
claude-squad, Claude Desktop, Anthropic's `claude agents` view, Octobox/Agent-Inbox triage
patterns, interruption/resumption research). Validation note: Anthropic's own agent view
(research preview) independently converges on the same triage patterns — state-grouped queue,
inline asks, aggregate counts — confirming the shape; Chimaera's differentiation stays
persistence + previews + workspace-first.*

**Principles** (each grounded in a research finding):

1. **State-grouped triage queue, not a badge wall.** Hook-derived states are strictly richer
   than tmux's `#`/`!` activity flags — make them the sorting key, notification predicate, and
   filter (`s:blocked`), not per-session unread counts (badge-anxiety research).
2. **Ship all three switching primitives** — last-session toggle, MRU cycle, fuzzy palette
   with live preview. Power users replace tmux's choose-tree with fzf for a reason; never make
   a modal picker the only path.
3. **Steal names, don't invent them.** zellij's random AdjectiveNoun names are the
   anti-pattern; tmux's rename-until-touched is the right shape.
4. **Return-after-hours is a first-class flow.** Only 10% of programmers resume within a
   minute of returning; explicit cues at the point of return cut resumption lag.
5. **Viewing is not resolving** (the Octobox lesson): blocked stays queued until answered,
   finished until archived; archive is searchable forever so closing costs nothing.
6. **One aggregate number** ("2 need you") in tab title/badge — never per-session counts.
7. **Never steal terminal keystrokes** (zellij's top complaint): one leader key, double-tap to
   pass it through, everything else raw to the PTY, zero exceptions.

**Naming rule zero (author, 2026-07-06): a session's name is the most specific thing known
about what it's DOING, never where it merely lives.** The workspace name already appears in
the window title and rail header; repeating it per-row is zero information (the shipped
fallback did exactly this — three rows all reading "chimaera" — and was rightly called out).
Display resolution: shells — foreground command while running (`snakemake`) → workspace-
relative cwd while idle (`results/qc`) → shell name at root (`zsh`); agents — customTitle →
aiTitle → first prompt truncated (captured from the UserPromptSubmit hook we already ingest —
dispatching a task names the row instantly) → agent name. Foreground/cwd via ~2s daemon poll
of the PTY's foreground process (/proc on Linux, libproc on macOS), OSC titles preferred when
the shell emits them. Same resolved name everywhere: rail, tabs, strip, resume lists.

**Auto-naming.** Agent sessions free-ride Claude Code's own naming: the CLI appends an
`{"type":"ai-title"}` record to the session JSONL seconds after turn 1 (verified in real
transcripts), and user renames write `customTitle` records that always win. Precedence,
re-evaluated on every transcript append: Chimaera rename (written as a `customTitle` record
*into the transcript* so `claude --resume` shows the same name — no second title store; pins
the name, permanently disabling auto-rename, tmux's rule) → TUI `/rename`/plan-accept →
`aiTitle` → provisional first-prompt (captured instantly via the `UserPromptSubmit` hook,
rendered italic, crossfades when the aiTitle lands) → dirname + 2-char suffix. Plain shells:
tmux `automatic-rename` upgraded — prefer the OSC 0/1/2 title and OSC 7 cwd escapes shells
already emit; fall back to PTY foreground process + cwd basename (idle → `fastq/`, running →
`samtools sort · 4m`).

**Orientation — three permanent affordances.** (1) A 24 px *session header* above every PTY:
hue-tinted ordinal chip · name · state pill with **elapsed-in-state** (a 12-minute block reads
as neglect, not activity) · branch · cwd · aggregate "N need you" on the right. (2) The
*session strip* (bottom, tmux-style, never hidden): workspace name, one chip per session
(stable ordinal + state glyph + name; focused chip inverted; last session underlined; shells
marked `$`), aggregate + clock on the right. (3) A deterministic *session hue* (hash of id)
tinting the header underline, strip chip, and a 1 px viewport border — peripheral-vision
identity, with glyph redundancy (hues fail past ~8 sessions and for colorblind users).
Switching flashes a 300 ms ordinal+name+state overlay (tmux `display-panes`); zoom keeps a
2 px hue edge. Ordinals are workspace-scoped, assigned lowest-free at creation, and **never
renumbered**.

**Navigation.** *Create:* Leader-c opens the dispatch box — type the task, Enter spawns a
claude TUI with that prompt pre-submitted: born named and working, never a blank "Temporary"
terminal (Esc gives an empty TUI). Leader-s spawns a shell. *Switch:* Leader-1..9 ordinals;
Leader-l last-session A/B toggle; Leader-Tab MRU cycle with a visible-order overlay;
Leader-n/p strip order; Leader-f fuzzy palette across all workspaces (name/path/branch +
`s:blocked` / `ws:rnaseq` tokens) with a **live read-only PTY preview** of the highlighted row
(claude-squad's proven layout). *Peek:* Space on any highlighted session — read-only live
viewport with the pending ask rendered structurally; y/n/number keys answer from the peek;
←/→ walks siblings; most triage never attaches. *Triage loop:* Leader-a jumps to the
oldest-blocked session; answering auto-advances, ending on "Inbox zero — all agents running."
*Return:* each window's view state (focused session, open surface — terminal, dashboard, or
file preview — scroll positions) is daemon-owned state, so reconnecting restores **exactly the
view you left**: a state machine, not a landing-page policy (author decision 2026-07-06). The
**since-you-left digest** — per workspace: finished/failed/blocked and for how long, computed
late-bound by the daemon on reconnect, never fired into a dead SSH tunnel — appears as a
dismissible banner, never a forced landing. Attaching to a session with unseen activity
overlays a **recap card** (final state, last assistant gist, diffstat, elapsed); any key
dismisses it.

**The agent launcher (author request, 2026-07-06).** "+ new agent" is a split button: the
main surface spawns your **default config instantly** (default = latest chosen, persisted);
**the popover opens ONLY from the chevron** (hover ~150 ms on the chevron itself, or click
it) — field feedback 2026-07-06: hover-anywhere-on-the-button opening the menu is intrusive;
the main surface must stay a pure instant-spawn target. The popover itself is held to the
full polish inventory (field verdict on the first cut: "looks a bit cheap and unintuitive" —
it is a first-class surface, not a utility menu: proper section rhythm, glyph alignment,
hover/highlight states, and the install/resume rows designed, not listed). Non-claude spawn
paths (codex, gemini) must be verified against the real installed binaries — the first cut's
codex launch did not work in the field.

- **Agent rows** — Claude Code, Codex, Gemini CLI…: the daemon detects what's installed
  (login-shell `command -v` per known binary, cached; version probed). Installed agents are
  selectable; selecting spawns a NEW conversation and becomes the new default. **No model
  picker** (author, 2026-07-06: "less interesting… skip that") — models are chosen inside
  each agent's own TUI when it matters; the launcher's whole question is *which agent, new
  or resumed*. (The server may keep model/resume params on POST /sessions — harmless API
  surface — but the launcher UI shows agents and conversations only.)
- **Uninstalled agents stay visible but muted, with an install action** — which opens a new
  terminal session in the workspace with the install command **pre-typed, not executed**
  (transparent, user presses Enter; our own terminals are the install UI).
- **Resume section** — recent resumable Claude sessions *for this workspace* (cwd-scoped, from
  the same `~/.claude/projects` JSONL store the naming pipeline reads): title, age; selecting
  spawns `claude --resume <id>` in a PTY. Searchable past ~8 entries.

- **Rail Recents (author, 2026-07-06; restructure same evening):** the rail groups
  **terminals first** (there are few), **agents below** (there are many) — each section
  headed quietly, with its create affordance at the section's foot ("+ terminal" under
  terminals; the "+ new agent" split button under agents). Under agents sits **RECENT**:
  the workspace's recently-ended agent conversations — **across agent types** — last 3
  visible, expandable to a scrollable list. Rows: type glyph + title + relative age.
  **RECENT is the ONE resume surface** (author: the popover's resume list was redundant
  once the rail had it): GET /api/v1/recents merges the daemon's own history
  (recents.json — any agent kind, ended under its watch) with the claude transcript store
  (conversations from before chimaera or run outside it); daemon entries win identity
  collisions. Click resumes when the agent supports it (claude `--resume <id>`); if an
  agent can't resume, the row starts a fresh conversation with an honest tooltip. Live
  conversations never appear; resuming one moves it out (claude forks a new session id
  per resume — records track `resumed_from`, and a resumed-then-ended conversation
  supersedes its ancestor entry). The section order is also the mod+1–9 chord order and
  the focus-mode strip order — what you see is what the numbers mean.

Server surface: `GET /api/v1/agents` (installed/version/models/install-hint per agent),
`GET /api/v1/agents/claude/sessions?workspace_id=` (resumables),
`GET /api/v1/recents?workspace_id=` (ended conversations, live ones filtered out), and
POST /sessions gains `agent`, `model`, `resume`. Non-claude agents start as plain TUI
sessions (hook-driven attention states are claude-only until their integrations land; the UI
shows their state as the muted unknown dot, honestly).

Launcher field notes (2026-07-06, shipped with the build):
- **"Codex does not work" root cause:** legacy codex-cli (0.1.x, npm-era) requires
  `OPENAI_API_KEY` in the environment and exits ~1 within ~400ms without it — and the
  daemon's environment never sourced the user's shell profile, so even an exported key
  wouldn't reach it. Two structural fixes, both general:
  1. **Agent sessions spawn through the user's login shell** —
     `$SHELL -lc 'exec "$0" "$@"' <bin> <args…>` (`exec $argv` for fish). Terminal parity
     is the product promise (VS Code's integrated terminal model): nvm PATHs, exported
     keys, profile env all reach the TUI. `exec` keeps the agent as the PTY's direct child.
  2. **Last words:** when a session's child exits, the manager snapshots the final screen
     (escape stream + exit status) into a bounded buffer (2MB, oldest evicted) BEFORE
     unregistering. A client attaching to an already-dead session gets ready →
     final-screen replay → `exited` instead of a blank pane; the pooled client terminal
     for a visible tab is likewise not disposed while on screen. A fast agent failure now
     reads as what it is ("Missing OpenAI API key. Set the environment variable…").
  The modern Rust codex CLI has `codex login` (OAuth); the legacy one is API-key only —
  the launcher's pre-typed install command upgrades it.
- **Recents vs `--resume` identity:** claude forks a NEW session id on every resume. Agent
  records carry `resumed_from`; the live-exclusion set matches either identity, and when a
  resumed session ends it *supersedes* its ancestor's recents entry (one conversation, one
  row, resumable via the newest id). Untitled claude boots never enter recents (nothing
  recognizable); untitled codex/gemini do (they have no title machinery yet — their rows
  are the only history there is).
- No model picker (author: "less interesting, skip") — launcher = which agent, new or
  resumed. `--model` stays a server capability on POST /sessions.
- Popover opens from the CHEVRON only (hover ~150ms or click); the main surface is a pure
  instant spawn of the persisted default agent.
- **Popover = agent picker only** (2026-07-06 restructure): one row per catalog agent,
  each with a link to its **official docs** (opens in the browser — author: no prose
  warnings, "just highlight that they are not installed and link to the official docs").
  Not installed → muted + "install" chip (pre-typed, never executed). Installed but
  **outdated** (the npm-era codex 0.1.x predates `codex login` and hard-exits without
  OPENAI_API_KEY — Martin's field failure) → "update" chip, same pre-typed flow. The
  catalog must stay CURRENT: Gemini CLI's Google sign-in was retired for individual
  accounts 2026-06-18 (Martin hit Google's own migration error in the field; API-key
  auth still works, and the login-shell env wrap delivers `GEMINI_API_KEY`), and Google's
  successor is the **Antigravity CLI** (`agy`, single static binary via
  `curl -fsSL https://antigravity.google/cli/install.sh | bash` — no node, HPC-friendly).
  BOTH stay in the catalog (author: keep gemini usable, no editorials — the docs links
  carry the story): claude · codex · gemini · agy. Field trap, guarded: the Antigravity
  IDE ships an `agy` symlink to its own app launcher (VS Code's `code` pattern) that
  opens the GUI and exits 0 silently — detection canonicalizes the resolved path and
  refuses anything inside Antigravity.app, so IDE-only hosts see the honest install row
  for the real CLI instead of a pane that just says "[exited]".
- Dev-loop nicety: the vite dev server exposes `~/.chimaera/manifest.json` at
  `/dev/manifest` (dev-only middleware, never in a build) so the dev page can self-auth
  without hand-copying tokens.
- **Detection self-corrects (field: "I updated codex but it still says update"):** the
  cache is daemon-lifetime, so the popover renders cached rows instantly and re-detects
  in the background (`?refresh=true`), swapping in the truth — an install/update made
  since the daemon started surfaces on the next popover open, and the refresh refills
  the daemon-wide cache POST /sessions spawns from.
- **Chimaera owns renaming (field: "codex /rename doesn't do anything"):** claude's
  in-TUI /rename flows through OSC titles by luck of its implementation; no other agent
  has one. `PATCH /api/v1/sessions/{id} {name}` pins a display name at the PTY layer for
  ANY session kind — double-click a rail row's name (or F2 on the focused row) for
  inline rename; the pin outranks every derived name on every surface (rail, tab,
  strip) and survives into Recents when the session ends.

**Daemon self-update on connect (author, 2026-07-07 — field find #2 on a cluster).**
Connecting to a host reused a 21-hour-old M0 daemon forever: `connect` only checks
"running?", and every build calls itself 0.0.1, so builds are indistinguishable. Spec:
- Binaries embed a **build id** (git hash + dirty flag + build time, via build.rs env);
  the manifest carries it. A missing field = ancient = outdated by definition.
- `connect` compares the remote build id to the binary it would deploy. Same → reuse
  (today's behavior). Different →
  - **zero live sessions on the remote daemon** (asked over ssh with the manifest's
    own token) → auto-update: graceful stop, redeploy via the existing
    ensure_remote_binary/dist path, start, new `Phase::Updating` progress state.
  - **live sessions** → NEVER silently kill. Connect succeeds against the old daemon
    but surfaces "daemon is from <build>, yours is newer, N sessions would end" with
    an explicit update affordance: `chimaera connect --update-daemon` (CLI) and an
    "update daemon" action on the host row (app). Count unavailable → treat as busy.
- Local parity: the app/CLI starting a LOCAL daemon applies the same comparison to a
  running localhost daemon (the manifest is on disk; same rules, no ssh).

**Session lifecycle: activity-honest states + dormancy reaping (author, 2026-07-07 —
queued, after daemon self-update lands).** "Alive green forever just for existing" is
dishonest. The daemon sees every byte both ways, and shell integration knows the
phase (prompt vs running), so:
- States become activity-based for EVERY session kind: **active** (output flowing /
  command running / agent working), **idle** (at prompt, quiet minutes — glyph dims),
  **dormant** (quiet hours — dimmer; may sort below active rows). Track last input +
  last output per session in the daemon; surface `last_activity` on session JSON.
- **Reaping** splits by what's lost: agents auto-retire after long dormancy
  (default ~24h, a setting) — cheap, they land in Recents and resume in one click.
  Terminals are NEVER auto-killed by default (an idle shell may hold module/conda
  state — the exact thing linked terminals exist to reuse); `terminal.idleReap`
  exists as an opt-in setting, and the shell-integration journal allows a smarter
  future condition ("no command ran in N hours") than byte-silence.
- **Update as a standing affordance** (extends daemon self-update): the status bar
  (by the daemon dot) and the home screen show "update available" whenever the
  running daemon's build id differs from the client binary's — local or remote,
  one click, the same never-silently-kill-sessions rules.

**Managed agent runtimes (author, 2026-07-07 — SHIPPED).** Chimaera installs and updates
the agent CLIs itself, in-app, while credentials stay entirely the user's: every CLI
keeps auth in the user's HOME (~/.claude + keychain, ~/.codex/auth.json, ~/.gemini),
and managed binaries run as the user with their HOME + login-shell env, so login flows
are identical regardless of who installed the binary. Design:
- Daemon downloads OFFICIAL artifacts only (hardcoded curated sources, HTTPS, never
  sudo) into `~/.chimaera/agents/<agent>/<version>/` with a `bin/` symlink swap; spawn
  prefers the user's own PATH install, falls back to managed.
- Phase 1 needs NO bundled runtime: claude (official installer, self-contained), codex
  (GitHub release binaries), agy (single Go binary) — only gemini-cli truly needs node.
  Phase 2 (if wanted): an official node runtime under `~/.chimaera/runtimes/` (the
  nvm/code-server pattern) to carry node-based CLIs.
- This AMENDS the pre-typed-never-executed rule with a transparency-preserving middle:
  the launcher's install/update chip shows exactly what it fetches and where, executes
  on one explicit click, and streams installer output into a visible terminal pane. No
  silent execution; silent auto-update only as a later opt-in setting. Update
  availability rides the existing background re-detection → "update" chip.
- HPC is the payoff case: no node, no sudo, daemon-side download means one-click
  install works identically for remote windows; air-gapped clusters pre-stock
  artifacts via the existing `~/.chimaera/dist/` pattern.
- Running sessions keep their binary across updates (processes hold the old file);
  new spawns get the new version. Claude Code self-updates in place — managed claude
  defers to it rather than fighting it.
- `~/.chimaera` as the install prefix is confirmed (author): it is the user's own,
  needs no root, and survives on HPC — the same guarantee as ~/.local/bin. Updates are
  UI-driven (the update chip) or auto (opt-in) — "control without owning anything."

**Agent theming + shell shims (author, 2026-07-07 — queued with managed runtimes;
"pretty important").** Agents must START with a theme that fits chimaera's UI, whether
launched from the rail or typed into a chimaera shell:
- The general mechanism is a shim dir: chimaera-spawned sessions (shells AND agents)
  get `~/.chimaera/shims` prepended to PATH via spawn env only — user dotfiles are
  never touched; a shell that resets PATH simply drops the shims. Each shim execs the
  real binary (user's own install first, managed fallback) with the right theme
  injected for the client's current scheme.
- Per-CLI theme levers: claude — theme merged into the SAME generated `--settings`
  file the hooks ride (respect an explicit user-set theme; only fill the gap); gemini —
  its settings.json theme field; codex/agy — mechanisms verified against the real
  binaries at build time (rule zero of this launcher work). TUIs that just use ANSI-16
  already fit: the pane palettes are measured for both schemes.
- The shim's bigger consequence, deliberately in scope: an agent TYPED into a chimaera
  shell goes through the wrapper too, so it gets hook injection (attention states),
  auto-naming, and Recents retirement — hand-started agents become first-class, not
  just launcher-spawned ones. The shim must know its session context (CHIMAERA_SESSION
  env set at spawn) and must NEVER apply outside chimaera sessions (the dir simply
  isn't on PATH elsewhere).
- Scheme is passed by the client at spawn (it knows prefers-color-scheme); mid-session
  scheme flips restyle the pane immediately (xterm theme) and the TUI on next launch.

**Triage dashboard (Leader-d).** Groups top to bottom: **Needs you** (needs_permission +
idle_prompt + errored), sorted longest-blocked first, rows "ripen" (border saturates with
age), never folds; **Rate-limited** pinned with reset countdown + auto-resume toggle;
**Running** by recency with a throttled one-line semantic summary (transcript tail,
Haiku-class, ≤1/15 s + turn end; degrades to the last transcript line with no API key);
**Finished** folds past 5 (errored never folds); **Shells** collapsed. Inline action zone per
state: needs_permission shows **the actual ask** (`Bash: sbatch --array=1-96 align.sh`) with
[y]/[n]/[Space]-peek — destructive-class commands (rm -rf, force-push, sudo) disable one-key
allow and force a peek; idle_prompt shows the question with number-key answers; finished
[o]pen/[e]archive; errored shows the last error line + [r]estart. A soft WIP nudge appears
past a threshold ("6 finished awaiting review") — the human review queue, not agent count, is
the bottleneck. Secondary surface: Leader-m monitor wall, a 2×2 live-terminal grid for ambient
babysitting of 2–4 sessions — explicitly not the triage surface (terminal pixels don't scale
to 15).

**Shells vs agents.** One session primitive — same ordinals, chips, peek, rename, scrollback —
two state vocabularies: agents run the six-state hook machine; shells have busy/idle/exited
plus an opt-in bell flag. Shells never enter the needs-you queue, never notify, never count in
the aggregate. Running `claude` inside a plain shell promotes it live to agent semantics (the
daemon exports `CHIMAERA_SESSION` into every shell; hooks bind the new session to the PTY) and
demotes on exit.

**Keymap.** Leader = Ctrl+Space by default; first-run picker offers ctrl-b (tmux) and ctrl-a
(screen) presets; double-tap sends the literal key. Holding Leader 400 ms shows a which-key
hint bar (zellij's one great idea, without its key-stealing). Everything rebindable
(`~/.config/chimaera/keys.toml`, tmux/zellij presets). CLI parity against the daemon socket:
`chim ls`, `chim attach align-fix`, `chim dispatch "fix the star index"`, `chim wait
s:blocked` — the fabric stays scriptable, tmux-culture style.

**Notifications.** Self-locating (workspace + session + the literal ask in the body); click
focuses that exact session; Allow/Deny as notification action buttons where the OS supports
them; suppressed for the focused session; batched per session; DND holds non-critical events
for the digest. Only needs_permission/errored may interrupt — finished accumulates silently.

**Known risks** (from the designs' own adversarial self-review): the state machine rides on
injected hooks — a claude update or a user's own hook config can degrade sessions to
`state: unknown`; render that honestly (gray badge) and detect it in `chimaera doctor`. Live
summaries can be confidently wrong — label them auto, throttle them. Any default leader
collides with someone (nested-tmux users especially) — hence the first-run choice.
Approve-from-anywhere was cut entirely (author decision 2026-07-06): blocked sessions are
*highlighted* — strip, dashboard, Leader-a jump — but approval always happens in context (peek
or attached), with the destructive-ask guard on top. Seeing what you approve is
non-negotiable.

### File previews: the moat

Server extracts, client renders; **never load whole files**. One `preview/open|page` dispatch
so each new format is one server match-arm + one UI component.

- **HTML reports (MultiQC/FastQC/Nextflow) — the killer feature**: served byte-for-byte under a
  `/raw/` path into a sandboxed iframe (`sandbox=allow-scripts`, no `allow-same-origin`, strict
  CSP, all external network blocked). MultiQC is self-contained by design, so it just works.
  Nobody else ships this.
- **CSV/TSV/Parquet**: server-side paging via the arrow-rs `parquet` crate (footer
  metadata/schema fast path, row-group and page-level reads, projection pushdown — no query
  engine needed) plus the `csv` crate with byte-offset indexing for TSV/CSV; hard memory caps;
  paginated slices as Arrow IPC binary frames into a virtualized table. A 50 GB Parquet opens
  instantly because only the visible page materializes. Verification note: full polars would
  add 20–40 MB+ to the binary and isn't needed for paging — add it (`default-features =
  false`) only if server-side filtering/aggregation later becomes a real feature. Scope
  honestly: row-group-metadata stats and paging, not full-scan sort/global-stats (which would
  violate the daemon's own resource budget).
- **Compressed reality check (from adversarial review)**: bioinformatics tabular files are
  overwhelmingly `.tsv.gz`/`.csv.gz`/`.vcf.gz`/bgzip. Gzip has no random access — the preview
  layer needs a decompression tier from day one: stream the head immediately, background-spool
  to a node-local cache for random access on files under a size cap, honest "sequential only"
  UX above it. bgzip (BGZF) *does* allow block random access — worth exploiting, it's the
  bioinformatics-native compression.
- **Jupyter notebooks**: ipynb is just JSON — parse server-side into a cell stream; markdown
  cells through the same renderer, code cells highlighted, PNG outputs inline, ANSI outputs
  through the terminal renderer, HTML/plotly outputs in sandboxed iframes. No kernel, no
  Python dependency, read-only.
- Images (+ server-side thumbnails), Markdown (server-rendered, sanitized, repo-relative images
  resolved), PDF (pdf.js via range requests), JSON/JSONL tree view, chunked text/code for giant
  files, hex fallback.
- Directory listings decorated with git status and scratch-vs-home filesystem hints.

**The context bridge (author request 2026-07-06): selection knows its source.** Select text
in a file view and a quiet floating affordance appears (plus a chord — parity principle):
**"reference in agent"**. It types a precise reference into the focused agent session's
input — Claude Code's native `@path` mention plus the line range and the quoted selection —
*without submitting*, so you review and press Enter. The workbench knows exactly what you're
looking at (path, line range), so agents get surgical context instead of pasted mystery text.
Plain Cmd+C stays untouched (never spooky). Wave 2 of this: selections in *terminal* panes
reference the session's scrollback.

**Clickable paths — the bridge's return direction (author, 2026-07-06):** agents produce
files; opening them should be one click, both ways of detecting them:

- **Terminal link detection**: an xterm.js link provider scans rendered lines for path-like
  strings (workspace-relative and absolute), validates candidates against the file service
  (only real files underline), and click opens the file as a tab in an adjacent pane
  (Cmd+click = new split). Works in every terminal — agent output, `ls`, pipeline logs —
  exactly the iTerm/VS Code affordance, but wired to our preview surfaces.
- **Hook-derived "files touched"**: the daemon already receives PostToolUse hook payloads
  from claude sessions — Write/Edit tool events carry the file path. Track a per-agent-session
  touched-files list server-side and surface it as a quiet chip row ("3 files") in the
  session's pane bar → click a file, it opens. Structured, reliable, and zero parsing of
  terminal text — no injectable skill needed, the hooks we already inject ARE the mechanism.

(The raw-terminal vs own-UI toggle the author floated as a fallback is already the plan for
Tier B — the structured chat mode from the roadmap — with the toggle living in the pane bar;
references and clickable paths must work in both views.)

**Viewer UX pass (author, 2026-07-06 — queued):** a focused audit-and-polish of the
inspection surfaces against real scientific files, since viewer *feel* is the moat's finish:

- **Images/figures**: wheel-zoom anchored at the cursor, drag-to-pan, fit / 100% / percentage
  indicator, double-click toggles fit↔100%, crisp SVG at any zoom, pixel-grid rendering past
  ~800% (plot inspection), subtle checkerboard under transparency.
- **Tables (CSV/TSV/Parquet-to-come)**: column resize + auto-fit, sticky header behavior at
  horizontal scroll, cell/row selection with plain-text copy, monospace numeric alignment,
  page-boundary feel (no jumps), row-count/position always visible.
- **PDF**: zoom smoothness, page-position memory per tab, text selection.
- Verified against real artifacts (MultiQC HTML, matplotlib/ggplot PNGs+SVGs, wide VCF-like
  tables), screenshots both schemes.

**Terminal readability pass (author, 2026-07-06 — queued):** current agent/terminal sessions
are "quite hard to read." Audit and tune: ANSI palette contrast against both scheme
backgrounds (claude's dim text especially), default font size (evaluate 13.5–14px),
line-height, per-pane font-size chords (Cmd+plus/minus with persistence), and how claude's
heavy box-drawing UI renders in JetBrains Mono. Treat as a focused mini-audit with
screenshots against real claude sessions, not a blind retheme.

**Drag-to-reference (same feature, drop-based):** dragging a file (tree entry or file tab)
over a *session* pane adds one zone to the drag grammar — a labeled band over the input area
at the pane's bottom: **"@ reference"**. Dropping there types into the session instead of
opening a tab: Claude agent sessions get the native `@path` mention (workspace-relative);
plain terminals get the shell-escaped path (relative to the session's current cwd when under
it — the naming-v2 cwd poll already knows it — else absolute). Never auto-submits. Center
drop still means "open as tab"; the two intents get two visibly distinct zones, no modifier
keys to memorize. This is a signature workbench feature — file views and
agent sessions living in one window is what makes it possible.

**File-navigation niceties (author request 2026-07-06, second pass after M3 wave 1):**

- **File-type icons — one system, applied everywhere a file appears** (tree, tabs, pane top
  bars, quick-open palette): inline 14–16 px SVGs, no icon font. Coverage goal is BROAD
  (author: "as many filetypes as possible") — ~40+ mappings: per-language code icons (rust,
  python, js/ts, svelte, R, shell, …), json/toml/yaml, markdown, html/css, notebooks,
  csv/tsv/parquet, images, pdf, archives, lockfiles, dockerfiles, makefiles/justfiles,
  env/config, git files, licenses — plus the first-class bio set (fasta/fastq, bam/cram/sam,
  vcf/bcf, bed/gtf/gff, h5/h5ad). Implementation guidance: rather than hand-drawing 40 glyphs,
  curate a subset from an MIT-licensed icon set (e.g. Material Icon Theme's SVG paths,
  license-verified), normalized to our muted palette tints and stroke weight so they read as
  one family with the hand-made session glyphs. Unknown types get a quiet generic-file glyph.
  Icon lookup is by extension with a few filename specials (Dockerfile, justfile, LICENSE).
- **Session-type icons, same system**: sessions carry a leading type glyph everywhere they
  appear (rail rows, pane tabs, strip chips, launcher rows, later the dashboard) — plain
  terminal = prompt glyph, Claude Code = spark-style glyph, Codex / Gemini = their own
  distinct marks. Type glyph and state dot coexist and mean different things: the glyph says
  *which tool*, the dot says *what state*. Trademark care: original abstract marks evoking
  each agent, never embedded vendor logos (this ships in an open-source binary).
- **Quick-open search (Cmd/Ctrl+P)**: one palette overlay (same visual language as the folder
  picker) fuzzy-matching **files and sessions** in the active workspace — files open as tabs
  in the focused pane, sessions focus. Server side: a workspace file-index endpoint (walker
  with .git/node_modules/target/venv ignores, result cap, mtime-ranked); content search
  (ripgrep-style) is a later wave. A small filter affordance at the top of the rail's files
  section covers the browse-narrowing case without opening the palette.

### Linked terminals: agents at your shell

*Added 2026-07-06 (author idea). Builds after M3; the OSC 133 groundwork touches the M1 PTY
layer and pays for itself beyond this feature.*

**The problem.** HPC state lives in long-lived interactive shells: ssh to a login node, `module
load`, `conda activate`, `salloc`. Recreating that per-command is the tax every agent pays
today. The daemon already owns every terminal (input channel, headless grid, scrollback) — so
hand the agent a *leash* to a live shell instead.

**One primitive: the link** — a daemon-owned edge between an agent session and a terminal
session. Every creation mode converges on it:

- **Sidecar**: "new linked terminal" in the agent pane's hover cluster (and palette) spawns a
  normal session, links it, and opens it as a split beside the agent.
- **Attach existing**: dragging a terminal (tab or rail row) over an *agent* pane reuses the
  drag-to-reference grammar — center still means "become a tab" (surface parity, untouched);
  the labeled band over the input area reads **"link to agent"** and dropping there creates
  the link *and* types `@term:name` into the composer without submitting — one gesture
  teaches both the link and the tagging syntax. Parity path: "Link to agent…" in the terminal
  pane's top-bar menu.
- **Mention**: typing `@term:name` (or `@term:3` by stable ordinal) in the composer. A
  `UserPromptSubmit` hook (same injection mechanism as the attention hooks) detects a mention
  of an unlinked terminal and auto-links it — the *user* typing the mention is the consent.
  Agents cannot self-link; links are user-granted only.

**Link ≠ layout.** A linked terminal stays a completely normal session — normal rail row,
normal tab, drag it anywhere. The sidecar is just default placement, not a container; there is
no new pane type and no docking machinery.

**Making the bond visible** (resolves the author's "subtab" instinct without occlusion —
tabs hide each other, and a sidecar you can't watch defeats the purpose): the agent pane's top
bar grows a **chip row of its linked terminals** — type glyph + name + state dot
(at-prompt / running / agent-executing / queued) — click a chip to focus that pane (reopening
it if closed). Reciprocally, the linked terminal's top bar carries the agent's glyph in the
agent's hue, click to jump back; the pane border pulses that hue while the agent executes
there. The terminal itself is the audit log — agent keystrokes land in the PTY and its
scrollback like anyone else's.

**Scope and cardinality (author decisions, 2026-07-06):**

- **Linked-only access.** The agent's tools see exactly its linked terminals — the chip row
  is the complete truth, and a rogue prompt can't touch an unlinked salloc shell.
- **One agent per terminal** (re-linking moves the leash; the hue follows). An agent may hold
  many terminals.
- **Approvals stay in Claude Code.** `run_in_terminal` prompts like any MCP tool; Chimaera
  adds zero permission UI.

**Execution semantics — where OSC 133 earns its keep.** Chimaerad injects shell integration
into the shells it spawns (bash `PROMPT_COMMAND`, zsh hooks, fish — the VS Code trick) and
taps the escape stream server-side (it already parses every byte for the headless grid),
yielding exact prompt/command/output/exit boundaries and a **structured command journal** per
session: command, output span, exit code, duration. The daemon exposes an HTTP MCP server
(config generated next to the per-agent settings file, per-session key auth) with three tools:

- `list_terminals` — linked sessions with name, cwd, shell state, last command + exit.
- `run_in_terminal` — types only when the shell is **at prompt**; if busy, **queues with a
  timeout** (author decision; the chip shows "queued" so waiting is never invisible), waits
  for the done-marker, returns output + exit code.
- `read_terminal` — the last N journal entries (commands + outputs), not a raw scrollback
  blob; raw tail available as a fallback.

**SSH degradation is first-class** (HPC is the point): a remote shell reached *from* a linked
terminal won't emit OSC 133. `run_in_terminal` falls back to sentinel-wrapping
(`cmd; printf` marker — works on any remote, zero install) and `chimaera shell-integration`
prints a one-liner to source remotely for full journal fidelity; the journal marks
lower-fidelity spans honestly rather than guessing.

**Subshells and cloning (author question, 2026-07-06).** The agent's own Bash tool is
untouched — it keeps spawning its own subprocesses. Background jobs in the linked shell
(`sbatch`, `nohup … &`) work naturally, serialized at prompt boundaries. True "fork this
shell" is wave 2: locally, snapshot env + cwd into a fresh session ("duplicate with
environment"); for remote shells the journal enables **setup replay** — it knows the `ssh`,
`module load`, `conda activate`, `cd` sequence, so cloning becomes replaying the reviewed
setup into a new session. Distinctive, deferred.

### Git + Slurm

- **Git** (design pass 2026-07-07 — read-only inspection first, worktree-aware). Shell out to
  system git and parse porcelain (adversarial reviews flagged gitoxide's diff gaps forcing a
  two-backend layer; shelling out is simpler and adequate for read-mostly status/log/diff/show).
  A `git` service (`chimaera-server/src/git.rs`, modeled on `view_state.rs`) that:
  - **Discovers the repo per workspace** and caches it: `git -C <root> rev-parse
    --show-toplevel --git-common-dir` → repo or `None`. `--git-common-dir` groups all worktrees
    of one repo — the workspace root may itself be a *linked* worktree (Chimaera is developed in
    one), so that is the common case, not the edge.
  - **One status command carries almost everything**: `git --no-optional-locks -C <top> status
    --porcelain=v2 --branch -z --untracked-files=all` — per-path X/Y staged/unstaged codes,
    rename scores, submodule state, *and* the branch header (name, upstream, ahead/behind) in a
    single call. `--no-optional-locks` is load-bearing: status must never contend on the index
    lock with a `git commit` the user or an agent runs in a terminal (slow/shared FS).
  - **Resource discipline is the design** (login-node budget): every invocation is
    `tokio::process` with a hard timeout (kill the child), an output-size cap, and an
    entry-count cap (truncate to "N+ changes", never materialize a 50k-file list); one
    in-flight status per repo, concurrent asks coalesce. **Git state is never persisted** — it
    is reconstructible, so nothing new lands under `~/.chimaera`.
  - **Refresh is event-driven, not polled** (a 2 s `git status` loop is exactly the steady-state
    cost the budget forbids, and NFS/Lustre inotify is unreliable where Chimaera runs, so no
    file-watcher either). Recompute on signals the daemon already has: a daemon file save
    (`PUT /fs/file`), an **agent PostToolUse write** (`files_touched` is already ingested — the
    signature integration: agent writes → tree lights up, zero polling), and explicit UI refresh.
    A 12 s backstop poll catches out-of-band edits (an external editor, a `git` command in a
    terminal) that fire no trigger. Its gate is an explicit **watch registration**: each
    `/ws/events` connection sends `{type:"watch", workspace_id}` naming the workspace its window
    shows, refcounted and released (RAII guard) on disconnect. Gating on "pulled recently" instead
    is a trap — pulls only happen when something *changed*, so a recency window decays to zero on
    a quiet repo and the backstop stops watching exactly when it is needed (found in live
    verification, not in tests). With no window open, nothing is polled.
  - **Wire = invalidate-and-pull**, not push-the-payload. `/ws/events` gains only a tiny
    `{type:"git", epochs:{workspace_id: epoch}}` nudge (the settings-generation trick); the
    client, if it is showing that workspace, pulls `GET /api/v1/git/status?workspace_id=`. Big
    path-lists stay off the daemon-wide firehose and materialize per-active-workspace.
    **The published-status hash is daemon-owned, not owned by whoever pulls**: a pull that
    discovers an unannounced change bumps the epoch (and reports the post-bump epoch, so the
    caller is already current and does not refetch) — otherwise one client's fetch would silently
    absorb a change, hiding it from every other client *and* from the backstop. An event-driven
    bump invalidates that baseline, so the pull it triggers adopts the new status without
    double-announcing. Diffs are always pull:
    `GET /api/v1/git/diff?workspace_id=&path=&mode=unstaged|staged|head`, capped, with "binary
    differs" and "diff too large → open the file" fallbacks.
  - **Tree decoration is an overlay, not baked into `fs::list`** (status changes independently of
    directory contents, and the tree lists lazily per-dir): a client `gitStatus` store keyed by
    path (parallel to the `dirtyFiles` store), rolled up to collapsed ancestor folders on the
    client. VS Code's grammar, muted to the theme: filename tint + a single M/A/D/U/? badge at
    the row's right edge; new `--git-*` tokens in `app.css` for both schemes (semantic and shared
    across every theme, like the `--ficon-*` fallbacks — status color is not a per-theme choice);
    one shared status→color helper reused by tree rows, pane tabs (a modified file's tab tints),
    and the changes panel. An epoch bump also **re-lists the visible directories**: a brand-new
    untracked file has a status but no row to hang it on until its dir is re-listed, so without
    that, "the agent just created three files" stays invisible.
  - **Diff is a new pane surface** `{surface:"diff", path, mode}` (the `FileTab`
    payload-carrying template): a diff tile *tiles next to the agent that produced the change*
    and participates in the context bridge (select a hunk → "reference in agent"). **Side-by-side
    from the start** via vendored `@codemirror/merge` `MergeView`.
  - **Changes panel is a full-pane singleton surface** `{surface:"git"}` (the `SettingsTab`
    template) — deliberately simple: branch header (branch · ahead/behind), grouped changed
    files, click-to-diff. Plus always-on orientation: a `⎇ branch ↑2 ●7` indicator in the status
    strip, and the branch on each session row (the header `branch` slot the interaction model
    already reserves).
  - **Worktree dimension (author, 2026-07-07 — a worktree is a dimension of ONE workspace, not a
    peer workspace).** Parallel agents collide in one checkout; the fix is a worktree per agent
    (shared `.git`, own branch, isolated tree). The workspace stays "the folder you opened"; the
    git service knows the repo's worktrees (`git worktree list --porcelain`) and **agent↔branch
    is derived, not stored** — match each session's polled cwd against the worktree paths.
    Phased: **P1 (SHIPPED)** the read-only inspection above. **P2 (SHIPPED)** worktree
    orientation: `GET /api/v1/git/worktrees` runs `git worktree list --porcelain`, and the client
    maps each session into a worktree by its polled cwd — **longest-root-wins**, because linked
    worktrees usually live INSIDE the main checkout (`.claude/worktrees/…`) and a first-match
    would file every session under `main`. The changes panel grows a **Branches** section: one
    block per worktree, its branch, a `current` tag, and the live sessions in it. A repo can
    carry dozens of stale worktrees (this one carries 17), so only the current worktree and those
    holding sessions are listed; the rest fold into "N other worktrees, no sessions" (managed
    worktrees always show, so the ones you can remove from here are never hidden). Deferred
    from P2: a branch chip on every rail session row (the Branches view already answers "which
    agent is on which branch"), and scoping status/diffs to a non-active worktree.
    **P3 (SHIPPED)** orchestration — the Branches section's "+ branch" composer: type a name, pick
    agent/terminal, and the daemon runs `git worktree add -b <branch>` then spawns the session
    INTO it, switching the window to the new worktree's workspace (the session auto-reveals once
    that workspace's layout boots — deferred past bootViewState, never racing it). Managed
    worktrees live at `~/.chimaera/worktrees/<repo-key>/<branch>` (author, 2026-07-07 — the
    daemon-owned prefix, consistent with managed runtimes; `repo-key` = repo dirname + a hash of
    the common git dir, so two same-named checkouts never collide). Removal is fenced FOUR ways,
    and the fences ARE the design: chimaera removes only worktrees under its managed root (never a
    checkout it did not create), never the one the workspace is open on, never one a live session
    sits in (matched by cwd — not even with `force`), and never one with uncommitted work unless
    forced. The branch always survives — removing a worktree is not `rm -rf` history. The remove
    control shows in the UI exactly where the daemon would allow it. All four fences are
    unit-tested, and the create → land-in-session → remove loop was driven live.
- **Slurm** (design pass 2026-07-14 — detection first, two placement modes; deep spec in
  *Environment prelude & compute-node sessions* below). The cluster-side daemon detects the
  scheduler *locally* (`command -v sbatch`) and serves it — the git-service model, not a
  connect-time ssh probe — so it lights up automatically on HPC and is a no-op off-cluster. A
  bounded `squeue --me` / `sinfo` poller (git-service resource discipline: `tokio::process` +
  hard timeout + output cap + coalescing + backoff, never a tight loop on a shared login node),
  a `GET /api/v1/compute` route, a client store parallel to `gitStatus`, and a job strip in the
  rail; job↔session linking detects agent-submitted `sbatch`. **Inbound and outbound are
  different facts and must not be conflated** (the earlier note here did): the *login node CAN
  reach a compute node's ports* — verified on Sherlock 2026-07-14, both a direct TCP route and
  `pam_slurm_adopt` ssh — so a daemon *on* a compute node is reachable through the one login-node
  hop; what compute nodes *may* lack is *outbound* internet to `api.anthropic.com`, which is the
  real gate on running an agent there — a per-cluster fact (Sherlock's nodes have direct egress,
  verified 2026-07-14; `http(s)_proxy` passthrough where centers allowlist a proxy). See below.

### Environment prelude & compute-node sessions

*Design pass 2026-07-14 (author idea, developed in-session; tunnel reachability verified live on
Sherlock). Two independent axes were deliberately separated — **environment** (what runs before
your shell/agent) and **placement** (where it runs). Keeping them apart is what makes this
extendable instead of a pile of per-scheduler special-cases; they compose (a compute-node job
still applies your prelude).*

**Axis A — the environment prelude** *(slice 1 SHIPPED 2026-07-15: host/workspace/launch
scopes, the spawn-seam injection, the Environment settings panel — see
[features/environment.md](../features/environment.md); capture mode and named profiles
remain).* A *prelude* is opaque shell text run before a shell or
agent starts: `micromamba activate env`, `ml bcftools`, `source setup.sh`, `export FOO=bar`.
Chimaera **never parses it** — it is the user's own commands in the user's own login shell, so
conda/lmod/spack/venv/nix all "just work" with zero tool-specific code (the not-too-specific
invariant). Honest scoping: shells already run interactive and source `~/.bashrc`/`.zshrc`, and
agents already run under `-lc` (login profile sourced), so the prelude is exactly *the
per-session setup your dotfiles don't do* — the things nobody puts in an rc.

- **Scopes concatenate, they do not override.** Effective prelude = host-default ⊕
  workspace-default ⊕ session/launch, in order; env vars merge last-wins. Concatenation matches
  how shells actually work and the HPC mental model ("always `ml bcftools` on this host" + "this
  project also `conda activate hello`" + "this session also `export DEBUG=1`").
- **Federated storage falls out of the daemon-per-host model.** A host-default lives in *that
  host's* daemon config (the cluster daemon owns "always `ml …` on the cluster"); a
  workspace-default lives with the daemon that owns the workspace — no cross-host config sync,
  each daemon persists its own. Storage: a dedicated `env-profiles.json` store (structured,
  multi-line) mirroring `WorkspaceStore` + `atomic_write_json` with a route; bindings (which
  profile is a host/workspace default) as settings keys; a settings **Environment** category
  edits them (bespoke panel, the `AgentsSettings` special-case pattern).
- **One injection seam, two hooks that already exist.** Write the effective prelude to a
  per-session file and export `CHIMAERA_PRELUDE`; the shell-integration rc sources it *after*
  the user's rc (`shellint`), and the agent login-wrapper sources it before `exec`
  (`wrap_login_shell`). Env vars ride the existing `SpawnOpts.env` overlay (`session_env`). Both
  terminal and agent spawns already funnel through these, so the prelude applies uniformly.
  Preludes run once per real spawn, **not on reconnect** (reconnect reuses the live PTY — no
  repeated `module load` cost).
- **Capture (later).** A scratch terminal where the user sets up interactively (`micromamba
  activate`, `ml load`), then snapshots the resulting env (`env -0` diff, powered by the OSC-133
  command boundaries already parsed) into a profile — captures whatever they did without
  Chimaera understanding any of it. Handles env-only tools; explicitly *cannot* capture `srun`
  (that changes *where*, not env — that is Axis B, and the clean line between them).
- **Trust boundary.** A prelude is the user's own commands (same privilege as their rc — nothing
  to enter, no credential). A prelude sourced from a *checked-in* workspace file would be a
  supply-chain vector: require explicit confirmation before running repo-provided commands.

**Axis B — compute placement.** *Where* a session runs. Today: whatever host the daemon is on
(laptop, or the login node `connect` reached). Slurm adds compute nodes — two modes, by intent,
not either/or:

- **Mode 1 — login node, agent-driven (the safe default; works wherever Slurm exists).** Detect
  the scheduler, hand the agent a **Slurm skill** (the cluster's `sbatch`/`srun`/`squeue`/`sinfo`
  conventions + default partition/account/prelude) and let it orchestrate; Chimaera just detects
  and exposes. No new daemon, no tunnel. Matches the standing HPC wisdom — the agent needs the
  API, the login node *has* internet, and heavy work dispatches to compute via `sbatch`/`srun`.
  It is also where the prelude pays off: the skill's `sbatch` script body = the workspace prelude
  + the job.
- **Mode 2 — own the full session ON the compute node (the isolated, premium experience).**
  `sbatch --job-name=chimaera-<ws>` runs a script whose body is the workspace prelude then
  `chimaera serve`. The whole workbench — daemon, file-watch, git, previews, every agent/terminal
  — runs *inside the allocation's cgroup*: correct resource accounting, and Slurm kills the whole
  tree cleanly at walltime / `scancel` (the isolation the design wants, for free). The compute
  node shares the parallel filesystem with the login node, so the workspace path is identical and
  **no file sync is needed**, and the same static binary is already visible — no redeploy.
  - **`squeue` IS the registry.** Jobs named `chimaera-*` are the discovery/reconnect index:
    `squeue --me --name=chimaera-*` → `%N` is the (routable) node name, `%L` the walltime shown on
    the daemon chip. Close the laptop → the job keeps running → reconnect → `squeue` finds it →
    re-establish the tunnel. No separate registry is built; Slurm already is one.
  - **The shared filesystem is the coordination channel.** The serve script writes
    `{node, port, token}` to `~/.chimaera/compute/<jobid>/manifest.json` — visible from the login
    node (same Lustre/GPFS), polled the way `RemoteHome` already polls a manifest. Per-user
    `$HOME` = per-user isolation for free; jobid keys multiple jobs within a user.
  - **Dynamic ports, never fixed.** The compute daemon binds `:0` (OS-assigned) and records the
    actual port in its manifest. Any number of users or workspaces on one fat node get distinct,
    non-colliding ports automatically — a fixed port collides the moment two of anything share a
    node.
  - **Reaching it is a negotiated ladder** — decided by bounded probes, surfaced to the user,
    never guessed per-cluster. **B (preferred):** loopback bind + `ssh`-forward through the login
    node (`ProxyJump`, reusing the login-node ControlMaster) — the port is *not exposed to
    co-tenants* at all, and with `pam_slurm_adopt` the ssh channel is adopted into the job cgroup
    (verified on Sherlock). **A (fallback):** routable bind + direct login→node forward (the
    existing `spawn_tunnel`, with the `%N` node name as the `-L` target instead of `127.0.0.1`) +
    per-session token as the only gate (verified reachable on Sherlock). **Neither → Mode 2
    unsupported on this cluster:** degrade to Mode 1 and *say so plainly* in the compute panel —
    no reverse-tunnel heroics (that rung was dropped 2026-07-14 as scope, not lock-in; the ladder
    stays open for a third rung if a real cluster ever needs it). "Not supported" here means only
    *own-the-session-on-the-node* — detection, the job strip, and Mode 1 still work.
  - **Compute-node outbound is a per-cluster fact — probe it, never assume it.** Mode 2 runs the
    agent on the node, so it must reach `api.anthropic.com` from there — directly or via an
    allowlisted `http(s)_proxy`. Verified on Sherlock 2026-07-14: **direct egress works from a
    compute node** (HTTP 405 from the API endpoint — TLS + HTTP path intact, no proxy configured),
    so Mode 2 is fully viable there. Other centers differ (many do block node egress), so the
    generalizable mechanism is a probe at job start: the serve script checks outbound and records
    the result in the manifest, letting the client say "agents can't reach the API from this node
    — terminals/previews here, agents via Mode 1" instead of an agent failing mysteriously. Where
    egress is blocked, Mode 2 still carries non-agent sessions (terminals, previews, file work
    inside the allocation) — the mode degrades by capability, not all-or-nothing.
  - **Two-tier persistence, stated honestly.** The login-node daemon persists indefinitely (the
    classic "close the laptop, nothing dies"). A compute-node daemon persists **until its
    allocation ends** — reconnectable via `squeue` the whole time, but walltime-bounded by
    construction. Surface walltime-remaining on the chip; this is correct for HPC (you asked for
    N hours, you get N), not a regression.

### Security notes

- Bearer token on every request; manifest is 0600 on `$HOME`.
- The sandboxed-HTML `/raw/` endpoint must not receive the main bearer token (self-XSS →
  token theft): use short-lived per-file tokens or a separate cookie-scoped origin.
- The daemon never accepts non-loopback connections. Users who want LAN/Tailscale do it with
  their own tunnels.
