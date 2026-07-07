# Chimaera — Design Document

*Founding design, 2026-07-05. Synthesized from a multi-agent research pass over the July 2026
ecosystem (agent session managers, remote-dev prior art, Claude Code integration surfaces, HPC
constraints) and three competing architecture proposals, each adversarially critiqued.*

## One-liner

**Chimaera is an agent workbench, not an IDE.** A single static Rust binary runs as a
tmux-grade session daemon on whatever host owns the work — a remote server, an HPC login node,
or your laptop — and serves a workspace-oriented UI where the primary objects are *agent
sessions* living inside *folders*, surrounded by the file previews, terminals, and git state
that show what those agents actually produced.

**Chimaera is a general-purpose coding tool** — any language, any repo, any host. The author's
bioinformatics-on-Slurm workflow is the founding use case and the harshest deployment test
(no root, old glibc, SSH-only), not the product boundary: a design that survives an HPC login
node runs anywhere, and the scientific-format previews are one instance of a general
rich-preview layer.

## The problem

The current state of the art for "Claude Code on a remote machine" is a two-tool split:

- **code-server in a browser tab**: great file/image/Markdown/git inspection, but terminals are
  structurally coupled to the browser tab — reload loses them, reconnection tokens break
  permanently ([code-server#2276](https://github.com/coder/code-server/issues/2276),
  [#4773](https://github.com/coder/code-server/issues/4773)) — so you end up nesting tmux inside
  a browser terminal, which is misery.
- **Ghostty + SSH + tmux/zellij**: real persistence, but switching between many agent chats is
  awkward and there is zero rich file viewing. The deliverable of a bioinformatics agent session
  is usually *files* — plots, MultiQC reports, tables — not the conversation.

Anthropic's own surfaces don't close the gap either: the Claude Code desktop app is
**chat-first, workspace-weak** — the folder is an attribute of the chat, so it's hard to see
outputs and what exists on disk. Chimaera inverts that: **workspace-first, chat-many**.

## What exists (July 2026) and the gap

The multi-session manager space is crowded but consolidating (Terragon shut down 2026-02,
Omnara archived 2026-02 noting CLI-wrapping "became unfeasible to maintain", CUI archived,
Crystal deprecated for Nimbalyst). Verified state of the nearest neighbors:

| Tool | Has | Missing |
|---|---|---|
| Claude Desktop (SSH sessions, Apr 2026) | parallel sessions, diff/terminal panes | Electron, no survive-disconnect story, weak file/workspace surface, closed |
| Claude Code Remote Control / claude.ai/code | phone steering, push notifications | no multi-session HPC dashboard, no previews, process must stay alive |
| opencode (`opencode serve`) | headless server + attachable TUI/web clients, SSE bus | its own agent (not Claude Code), zero file previews, unbounded storage growth |
| Zed remote dev | musl server auto-deployed over SSH, ACP agents | no `RemoteCommand` support ([zed#25896](https://github.com/zed-industries/zed/issues/25896)), idle disconnects, no session dashboard |
| Wave Terminal | persistent blocks + previews | Electron, remote previews need extra deployment, not agent-native |
| claude-squad | tmux + worktrees over SSH | terminal-only, no rendering of anything |
| Happy | best mobile/notification story | chat-only, no files |
| Operon (closest in spirit — bioinformatics AI IDE) | Claude-in-tmux over SSH, Slurm-aware | fat desktop client (no headless server), glibc ≥ 2.35, previews limited |
| Vibe Kanban | multi-agent orchestration, runs headless | task/PR-centric, no data-file previews, no HPC awareness |

**The four-way intersection nobody ships:** (1) a no-root single-static-binary server *on* the
cluster, (2) tmux-grade persistence with instant lossless reattach, (3) an attention-aware
multi-agent dashboard, (4) scientific file previews (MultiQC HTML, ipynb, Parquet, PDF).
Every competitor covers at most two. That intersection is Chimaera.

Honest sizing: this is a *niche workbench with a real moat*, not a mass-market platform. The
moat is exactly the part Anthropic won't build — open source, no Electron on the cluster,
RHEL 8/no-root deployment, and previews for the files scientists actually make.

## Product model: workspace-first

The VS Code mental model, applied to agents:

- **Window = workspace = folder.** Opening a folder — local, or `host:path` over SSH — gives
  one window rooted there: file tree, previews, git state, and N agent sessions all scoped to
  that folder.
- **Sessions belong to the workspace, not the client.** This aligns perfectly with Claude Code
  itself: sessions persist as JSONL under `~/.claude/projects/<encoded-cwd>/` and `--resume` is
  cwd-scoped, so workspace root ≡ session cwd is the natural join key.
- **Plain shells are first-class sessions.** A workspace hosts agent sessions *and* ordinary
  persistent terminals (zsh/bash rooted in the workspace) for running things manually, fixing
  up state, poking at pipelines. Under the hood they are the same thing — daemon-owned PTYs
  with server-side terminal state, instant reattach, multi-attach — differing only in chrome
  (shells get no attention badges). This is where tmux/zellij gets replaced, not just the agent
  chats.
- **The daemon owns everything; windows are views.** Close the laptop mid-run: nothing happens
  to the sessions. Reopen: the window reattaches to identical state. This is tmux's ownership
  model (daemon owns PTYs/state, clients are dumb renderers) — the exact inverse of
  code-server's failure mode.
- **A remote window is spawned like code-server**: `claude` + `chimaerad` installed user-side on
  the HPC, logged in once there; the client machine needs nothing but Chimaera (or just a
  browser).

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

Reconnect semantics are Eternal-Terminal-style at the application layer: every event-bus frame
carries a monotonic per-session sequence number backed by a bounded replay ring; a reconnecting
client sends `{session_id, last_seq}` and receives only the gap.

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

**Tier B — stream-json structured chat mode (v1, lands at M6).** Chimaerad spawns
the native `claude` binary with `--input-format/--output-format stream-json` and speaks the
NDJSON control protocol directly from Rust (no Node sidecar): Chimaera draws its own
claude.ai-style chat UI with structured tool calls, permission prompts as native approve/deny
buttons, `interrupt`, permission-mode switching, resume/fork, per-turn `total_cost_usd`,
`maxBudgetUsd`. Caveats, verified: the protocol is semi-documented
([claude-code#24594](https://github.com/anthropics/claude-code/issues/24594)) and actively
churning (the `canUseTool` control-request path has already shifted toward `PreToolUse` hooks),
and this headless surface is the one targeted by the paused billing split. So: pin CLI versions
(`DISABLE_AUTOUPDATER`), key a compat shim on the `system/init` handshake, run a protocol
smoke-test at daemon start that degrades to Tier A instead of crashing, and keep the whole
driver behind a thin `AgentAdapter` trait (<~2k lines).

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
hover (~150 ms) or the chevron opens the launcher popover:

- **Agent rows** — Claude Code, Codex, Gemini CLI…: the daemon detects what's installed
  (login-shell `command -v` per known binary, cached; version probed). Installed agents are
  selectable with a **model** picker (curated per-agent list — e.g. opus/sonnet/haiku via
  `--model`); selecting spawns and becomes the new default.
- **Uninstalled agents stay visible but muted, with an install action** — which opens a new
  terminal session in the workspace with the install command **pre-typed, not executed**
  (transparent, user presses Enter; our own terminals are the install UI).
- **Resume section** — recent resumable Claude sessions *for this workspace* (cwd-scoped, from
  the same `~/.claude/projects` JSONL store the naming pipeline reads): title, age; selecting
  spawns `claude --resume <id>` in a PTY. Searchable past ~8 entries.

Server surface: `GET /api/v1/agents` (installed/version/models/install-hint per agent),
`GET /api/v1/agents/claude/sessions?workspace_id=` (resumables), and POST /sessions gains
`agent`, `model`, `resume`. Non-claude agents start as plain TUI sessions (hook-driven
attention states are claude-only until their integrations land; the UI shows their state as
the muted unknown dot, honestly).

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

- **Git**: shell out to system git and parse porcelain output (adversarial reviews flagged
  gitoxide's diff gaps forcing a two-backend layer — shelling out is simpler and adequate for
  read-mostly status/log/diff/show).
- **Slurm**: `squeue`/`sacct --json` poller with backoff; job strip in the sidebar;
  job↔session linking (detect agent-submitted sbatch); degrades to no-op off-cluster.
  Post-v1: launch agent sessions *as* Slurm jobs for heavy work — but note most compute nodes
  have no outbound internet, so agents needing `api.anthropic.com` generally live on the login
  node and dispatch work to compute via sbatch/srun (support `http(s)_proxy` passthrough for
  centers with allowlisted proxies).

### Security notes

- Bearer token on every request; manifest is 0600 on `$HOME`.
- The sandboxed-HTML `/raw/` endpoint must not receive the main bearer token (self-XSS →
  token theft): use short-lived per-file tokens or a separate cookie-scoped origin.
- The daemon never accepts non-loopback connections. Users who want LAN/Tailscale do it with
  their own tunnels.

## Scope philosophy and non-goals

**v1 targets the optimal build, not a compromised first cut.** Nothing on the roadmap is
scaffolding to be thrown away: the daemon, the wire protocol shape, the web UI, the Tauri
shell, and every agent tier are pieces of the final architecture, added in a buildable order.
The milestone sequence exists so each stage is independently dogfoodable — not so the good
parts can be deferred indefinitely.

True non-goals (product boundaries, not deferrals):

- **No IDE-grade editor** (amended 2026-07-06: author wants markdown/text *editable*).
  Lightweight single-file editing IS in scope: the CodeMirror viewer flips editable, Cmd+S
  saves through the daemon, dirty-dot on the tab, markdown gets an edit/preview toggle. The
  non-goal that remains: no LSP, no completions, no multi-file refactoring, no debugger —
  serious editing still lives in real editors; agents write most code anyway.
- No own mobile app, no E2E relay service (free-ride `--remote-control` / ntfy), no Electron,
  nothing heavy on the cluster. (In-window split panes ARE in scope — author decision
  2026-07-06, superseding the earlier "no tiling WM" line; see In-window layout below. The
  non-goal was solo-scope caution, not product conviction, and panes are the mechanism that
  puts an agent and its outputs side by side.)
- No bespoke GPU-native client (see Client section — a different project, not a deferral of
  this one).

After-1.0 (sequencing, not compromise — these need a working product to be designed well):

- Published/versioned public protocol. tmux and LSP earned protocol status *after* adoption;
  the internal protocol is designed cleanly so publication is a docs-and-freeze exercise.
- Multi-host hub federation (manifest + per-host windows cover multi-cluster use until then).
- sbatch-offloaded agent sessions (agents running inside Slurm jobs).

## Risks (ranked)

1. **The Anthropic billing overhang.** Verified: Anthropic announced (June 15, 2026), then
   paused indefinitely, moving Agent SDK / `claude -p` / third-party-app usage onto small
   monthly credit pools billed at API rates, while interactive TUI keeps subscription limits
   ([support article](https://support.claude.com/en/articles/15036540),
   [Zed's response](https://zed.dev/blog/anthropic-subscription-changes)). Tier B is exactly
   the usage class targeted. **Largely defused by the 2026-07-06 decision**: the primary mode
   runs the interactive TUI (plugin-style, normal subscription billing); the structured chat
   mode is a post-v1 enhancement that can absorb whatever billing shape lands.
2. **Protocol churn.** The stream-json control protocol is semi-documented and has already
   changed shape once this year. Mitigation: version pinning, handshake-keyed compat shim,
   startup smoke-test, thin adapter.
3. **First-party convergence.** Remote Control (Feb 2026) already commoditized phone
   steering; a first-party session dashboard within 18 months is probable. The durable moat is
   previews + no-root HPC deployment + workspace-first UX — which argues for building the
   preview layer *early*, not last.
4. **Per-site HPC policy roulette.** systemd `KillUserProcesses`, process reapers that
   whitelist tmux by name, tmp-scrubbing, login-node reboots, outbound-blocked login nodes.
   `chimaera doctor` diagnoses; some sites simply won't allow it. Also expect one "please
   explain this binary making HTTPS POSTs" ticket from HPC security.
5. **Solo scope.** Every reviewed plan was judged 2–3x optimistic. The mitigations are
   structural: each milestone independently retires a piece of the current workflow, so
   partial completion still pays; and the graveyard (Terragon, Omnara, CUI, Crystal — all dead
   within a year) says keep the core small and the Claude coupling behind one seam.

## Roadmap

Estimates are the adversarially corrected ones (~2x the optimistic drafts), for a strong solo
dev working with AI agents, part-time.

| Milestone | Scope | Retires | Est. |
|---|---|---|---|
| **M0 — Walking skeleton** | musl CI, `serve` + token + hello-world UI, `connect` (ssh push, attach-or-spawn, tunnel), manifest. **Prove the deployment story on the real cluster first** — incl. round-robin login nodes and daemon-survival policy. | — | 2–3 wks |
| **M1 — Persistent terminals** | PTY sessions with server-side terminal state (resize/multi-attach-safe): named plain shells per workspace as first-class sessions, xterm.js+WebGL, event-bus seq replay, workspace + session list UI. | tmux/zellij-in-code-server | 4–6 wks |
| **M2 — Agent sessions + attention** | Tier A: workspace-scoped `claude` TUI sessions in PTYs, hook injection, JSONL transcript viewer, attention state machine, naming pipeline, session strip + triage dashboard + digest (see Interaction model), ntfy/webhook. | scattered agent chats | 5–6 wks |
| **M3 — Previews wave 1** | file tree, images+thumbnails, Markdown, sandboxed HTML reports (MultiQC), CSV/TSV with gzip tier. | most code-server use | 3–4 wks |
| **M4 — Previews wave 2 + git** | Parquet paging, ipynb, PDF, JSON tree, large-file guards; git status/log/diff panel. | code-server, entirely | 3–4 wks |
| **M4.5 — Linked terminals** | OSC 133 shell integration + per-session command journal, daemon HTTP MCP server (`run_in_terminal`/`read_terminal`/`list_terminals`), link UX (agent top-bar chips, reference-band drag, auto-link on `@term:` mention), sentinel fallback for remote shells. | agent↔shell copy-paste | 3–4 wks |
| **M5 — HPC layer** | Slurm strip + job↔session links, `doctor`, transcript pruning/quotas, docs, demo, v0.1 release. | — | 2–3 wks |
| **M6 — Optimal-build completion → v1.0** | Tauri native shell (window per workspace, native notifications, menubar badge), Tier B structured chat mode, Tier C ACP agents (Gemini native, Codex via codex-acp). | Claude desktop app | 6–8 wks |
| After 1.0 | Hub federation, sbatch-offloaded sessions, protocol publication, single-file editing. | | ongoing |

Realistic wall-clock: "code-server uninstalled" at M4 (**~5–7 months part-time**); the full
optimal build (v1.0 at M6) **~8–10 months part-time**. M1 alone already improves daily life,
which is the survival property that matters.

## Decisions log

- **2026-07-06 — Frontend: Svelte 5** + TypeScript + Vite (author's call).
- **2026-07-06 — Primary agent mode: PTY/TUI + hooks.** Claude Code runs inside Chimaera the
  way it runs in VS Code's integrated terminal — the real interactive TUI in a pane, normal
  subscription billing. The stream-json structured chat UI (Tier B) lands at M6 as an
  enhancement; TUI mode remains the default.
- **2026-07-06 — v1 is the optimal build.** No "maybe in v3" deferrals: Tauri native shell,
  structured chat mode, and ACP multi-agent are all on the v1 path (M6). Plain persistent
  shells are first-class workspace sessions from M1. The only exclusions are true non-goals,
  not compromises.
- **2026-07-06 — exited shells vanish (tmux semantics).** The daemon reaps a session when its
  child exits; no gray corpse rows. Persistent lifecycle rows ("finished", awaiting review)
  are *agent*-session semantics and arrive with M2's attention states.
- **2026-07-06 — in-window split panes, focus mode, bundled mono.** Author request: tiling
  panes (drag-and-drop + keyboard), pane zoom, a rail-collapsing focus mode that leaves the
  session strip as the always-on orientation layer, and JetBrains Mono bundled for terminal +
  UI monospace accents. Supersedes the "no tiling WM" non-goal. Builds after M2a.
- **2026-07-06 — window host label.** The daemon indicator shows what the user calls the
  machine: "local" for an untunneled daemon, else the ssh alias passed by `chimaera connect`
  via the `#host=` hash param (the VS Code Remote convention); the raw hostname is hover
  detail. Raw hostnames like `host.example` confused the author on day one — labels must
  be human.

- **2026-07-06 — linked terminals (agent ↔ shell).** One primitive: a user-granted link
  between an agent session and a terminal session. Linked-only access (the agent's tools see
  exactly its links), one agent per terminal (re-link moves the leash), busy shells queue
  execs with a timeout. OSC 133 shell integration + server-side command journal; sentinel
  fallback for SSH'd remotes with a `chimaera shell-integration` one-liner for full fidelity.
  `@term:` mention by the *user* auto-links; agents cannot self-link. Link ≠ layout — the
  sidecar split is default placement, not a container. Approvals remain Claude Code's own.

Still open:

1. **License** — Apache-2.0 (recommended; matches ACP/Zed ecosystem) vs MIT.
2. **`--remote-control` free-riding** as the blessed mobile story vs. building ntfy
   approve/deny round-trips (doc recommends: both are cheap; ntfy is vendor-neutral).

## Verified component notes (2026-07-06)

Crate-level verification sweep completed; all six architecture bets confirmed. Locked
component decisions:

- **mimalloc as the daemon's `#[global_allocator]`, unconditionally.** musl's mallocng
  allocator has a confirmed 7–30x multithreaded penalty that hits tokio/axum and terminal
  churn, not just data paging; static-linking mimalloc into musl is a solved pattern.
- **arrow-rs `parquet` + `csv` crates, not polars**, for the paging service (see previews).
- **alacritty_terminal (0.26, actively maintained) as the headless server-side grid**;
  attach/resize via escape-sequence snapshot re-emission, never serialized grid state (see
  transport).
- **portable-pty is acceptable but single-maintainer** (wezterm's release cadence has slowed);
  `pty-process` (async-native, Unix-only — fine, the daemon is Linux-only) is the named
  fallback. Keep the PTY layer behind a small trait either way.
- **cargo-zigbuild for musl cross-compilation** (`cross` as documented fallback). Keep TLS out
  of the daemon's dependency tree entirely (localhost-over-SSH needs none); if a dependency
  drags in rustls, force the `ring` backend, not `aws-lc-sys`.
- **System ssh for tunnels: confirmed correct** (inherits ControlMaster/ProxyJump/Duo).
  Documented limitation: Windows' built-in OpenSSH lacks ControlMaster, so tunnel startup
  pays full handshake+2FA cost there. russh only becomes interesting if a no-external-binary
  Windows client is ever required.

## Field notes: first cluster deployment (cluster, 2026-07-06)

M0 `connect` validated end-to-end against a production HPC cluster. Findings:

- **The static musl binary ran unmodified on CentOS 7.9 (glibc 2.17)** — a full glibc
  generation older than the design's RHEL 8 worst case. Deployment story confirmed.
- **Shell-parse hang (fixed):** `mkdir ... && nohup daemon ... & disown` backgrounds the
  *whole* `&&` list — the daemon runs as the foreground child of a subshell whose
  stdout/stderr are the ssh channel, so sshd never closes the session and `connect` hangs
  forever. Fix: `;` before `setsid nohup ... < /dev/null &`. Only reproducible on real infra.
- **ControlMaster mux forwards (fixed):** with a live master, `ssh -N -L` registers the
  forward with the master and exits 0 — the master owns the forward. The tunnel lifecycle
  must treat zero-exit as mux-delegation (hold, then tear down via `ssh -O cancel -L ...`),
  not as failure.
- **ControlMaster pins the login node:** all multiplexed sessions ride one TCP connection to
  one node (config pointed at `login-alias`, master landed on `login-node-a`, every subsequent command
  hit `login-node-a`). Round-robin manifest discovery matters only *across* master restarts —
  less scary than the design feared, but still needed.
- **`claude` is not in the non-interactive ssh PATH** on the login node even for a user who
  runs it daily — M2's session spawning must resolve the agent binary via a login shell or
  explicit config, never PATH assumptions.
- cluster login nodes run Duo + `gssapi-with-mic,password` only (no pubkeys): riding the
  user's ControlMaster isn't just convenient, it's the *only* non-interactive path — the
  design's shell-out-to-system-ssh decision is load-bearing here.
- **On containers as a fallback:** Docker never exists on HPC (no root); Apptainer/Singularity
  does, but adds per-site bind-mount/startup variance. The static binary already solves the
  problem class containers address (old glibc, missing deps) — and the bugs we actually hit
  were ssh/shell semantics that would reproduce identically inside a container. Keep an
  Apptainer recipe as a documented fallback for pathological hosts, not as the plan.

## Sources

Key verified references: [Claude Desktop](https://code.claude.com/docs/en/desktop) ·
[Remote Control](https://code.claude.com/docs/en/remote-control) ·
[Agent SDK sessions](https://code.claude.com/docs/en/agent-sdk/sessions) ·
[headless CLI](https://code.claude.com/docs/en/headless) ·
[hooks](https://code.claude.com/docs/en/hooks) ·
[ACP](https://agentclientprotocol.com/) ·
[claude-agent-acp adapter](https://github.com/zed-industries/claude-code-acp) ·
[Zed remote dev](https://zed.dev/docs/remote-development) ·
[zed#25896](https://github.com/zed-industries/zed/issues/25896) ·
[opencode server](https://opencode.ai/docs/server/) ·
[Eternal Terminal](https://eternalterminal.dev/howitworks/) ·
[SQLite locking](https://sqlite.org/lockingv3.html) ·
[Arbiter2](https://github.com/CHPC-UofU/arbiter2) ·
[Agent SDK plan usage / paused billing change](https://support.claude.com/en/articles/15036540) ·
[Operon](https://github.com/swaruplab/operon) ·
[claude-squad](https://github.com/smtg-ai/claude-squad) ·
[Happy](https://github.com/slopus/happy) ·
[Vibe Kanban](https://github.com/BloopAI/vibe-kanban)
