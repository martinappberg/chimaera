# Agents — launch, lifecycle & runtimes

Launching and managing Claude Code, Codex, Antigravity and Grok Build. Chimaera runs
the agent's own runtime as its **real interactive TUI** in a daemon-owned PTY
(Tier A) or as a **structured chat session** through its native protocol
(Tier B — see [chat-mode.md](chat-mode.md)). Claude Code and Codex can switch a
saved conversation between these views. This page covers getting an agent running,
the session rail that tracks it, renaming/killing, managed installs, and resuming
ended conversations.

**Where it lives (shared):** UI `web-ui/src/lib/workspace/{Launcher.svelte,launcher.ts,
sessions.ts}` + the rail/split-button in `web-ui/src/App.svelte`. Daemon:
`crates/chimaera-server/src/{api/sessions.rs,launcher.rs,agents.rs,runtimes.rs,agent_state.rs,
spawn.rs,recents.rs}`. Wire: `POST/GET/DELETE/PATCH /api/v1/sessions*`, `GET /api/v1/agents`,
`POST/DELETE /api/v1/agents/{id}/install`, `GET /api/v1/agents/claude/sessions`,
`GET /api/v1/recents`, `POST /agent-events/{id}?key=`, and `/ws/events` for the roster.

## Your existing agent setup

Chimaera launches the agent's own runtime in your workspace, using that host's
account, environment, project instructions, skills, and configured connections.
A local workspace uses your local setup; an SSH workspace uses your user's setup
on the remote host. Chimaera adds workspace integration through session-scoped
settings, hooks, and MCP tools. Chat controls select the model and permissions
for the current session; see [chat mode](chat-mode.md) for provider behavior.

## Launching an agent

- **What & when.** Start a coding agent in the focused pane, as a TUI or a chat session.
- **How it's used.** `POST /api/v1/sessions` with `kind:"agent"`, `agent:"claude"|"codex"|…`,
  optional `model` (an agent-provided id), `resume` (the saved native conversation id),
  `theme`, `cols`/`rows`, and
  `ui:"term"` (default, real TUI) or `ui:"chat"` (structured driver). New agent sessions default
  to chat when `agents.defaultView === "chat"` and the agent is chat-capable.
- **Where it lives.** `api/sessions.rs` (`create_session`, `spawn_chat_ui`); TUI spawn `spawn.rs`;
  chat spawn `chat.rs`; argv assembly `launcher.rs` (`build_agent_command`/`build_chat_command`,
  `safe_arg`).
- **Key behaviors.** The binary is resolved through the interactive **login** shell (`-ilc`, not
  `-lc` — the claude installer's PATH line lives in `.zshrc`, and `claude` isn't on the
  non-interactive ssh PATH on HPC nodes), then well-known prefixes, then the managed bin dir; a
  hit is cached for the daemon's life, a miss left uncached to self-heal. `model`/`resume` are
  charset-validated (`safe_arg` refuses flag-shaped/control-byte values). The launcher env scrubs
  the daemon's own `CLAUDE_CODE_*`/`CLAUDE_AGENT_*` markers so a spawned claude doesn't think it's
  a nested child. Terminal `resume` is accepted for Claude and Codex (400 for other
  agents); all four chat adapters resume saved native sessions. Codex TUI resumes map to
  `codex resume <thread-id>`. Chat is gated to
  `chat_capable()` agents: Claude, Codex, Antigravity and Grok Build. Gemini CLI remains readable in old records but is retired from the new-agent catalog.

## The launcher — split button & popover

- **What & when.** The primary way to start an agent: one click spawns your persisted default;
  the popover answers "*which* agent" with provenance and install state.
- **How it's used.** The split button starts the saved default. Its chevron opens a ready-first
  agent menu with one **chat** action per ready agent and a secondary terminal button. Agents
  needing installation or Antigravity chat setup appear below. Provider labels are visible;
  binary/version provenance lives in the tooltip and Agents settings. Arrow keys navigate,
  Enter follows the visible chat action, and Command/Ctrl+Enter opens terminal.
- **Where it lives.** `App.svelte` (`.new-split`, `spawnDefaultAgent`), `Launcher.svelte`,
  `launcher.ts` (`listAgents` → `GET /api/v1/agents`; `?refresh=true` bypasses the detection cache;
  `LaunchPick.ui` carries the explicit surface choice into `createSession`).
- **Key behaviors.** Default persists in localStorage (`chimaera.agentDefault`, falls back to
  `claude`). If the default is missing, the main surface doesn't spawn a doomed pane — it installs
  in place (if managed) or opens the popover. Binary provenance and the resolved path stay in the tooltip. When the daemon knows a strictly newer release, the version line grows **`→ <new>`**:
  an accent one-click curated update for a chimaera-managed build, quiet muted information for your
  own (chimaera never touches an install it doesn't own — the docs link is the affordance). The
  popover paints INSTANTLY from the window's last-known catalog (no "checking…"
  flash per open) and re-detects in the background, swapping in the truth; only a window that has
  never seen a catalog shows the pulse. The picker’s chat/terminal choice overrides the
  `agents.defaultView` setting for that one spawn (the setting still governs the split button's
  instant spawn). Agents with no curated managed install get only the official setup link.

## Managed runtimes — install / update / theming shims

- **What & when.** Chimaera installs and updates the agent CLIs itself (curated scripts, official
  sources, checksums when published, never sudo), streaming the installer into a visible terminal pane —
  and writes tiny theming "shims" that inject a scheme-matched theme into agent spawns.
- **How it's used.** Click an install chip → `POST /api/v1/agents/{id}/install {workspace_id}`
  spawns the curated command as an ordinary shell session you watch. Click an update affordance
  (launcher `→ <new>`, or Settings → Agents "Update") → `POST /api/v1/agents/{id}/update`
  re-runs the same curated script (it always fetches latest and re-swaps atomically) as a session
  named `update <agent>` — **managed binaries only**; for your own binary the daemon 400s and the
  UI never offers it. `DELETE /api/v1/agents/{id}/install` uninstalls the managed copy (driven
  from the Agents settings panel, after an in-app `ConfirmDialog` that keeps a failure inline).
- **Where it lives.** `runtimes.rs` (`install_agent`, `update_agent`, `start_install`,
  `install_script`, `write_shims`, `regenerate_shims`); latest-release awareness in
  `agent_updates.rs`.
- **Key behaviors.** Scripts are composed by the daemon (never the client), `set -euo pipefail`,
  HTTPS-only, no sudo, version charset-whitelisted, downloads in a `mktemp` dir. Layout
  `~/.chimaera/agents/<agent>/<version>/bin/` with an atomic per-agent symlink swap (running
  sessions keep their exec'd inode — the update session says so up front). Cluster workspace
  daemons share this directory instead of installing into each workspace's `data/agents`;
  `CHIMAERA_HOME` still isolates development installs. Existing workspace-local copies are
  read fallbacks; updates install into the shared directory. A shared per-agent file lock
  serializes installation, updates, and removal across workspace daemons. If preparing an
  install fails because storage is full or the user's quota is reached, the error names the
  affected installation folder and asks the user to free space or check the host's quota.
  Updates automatically remove superseded packages, keeping the activated version and
  packages leased by running agents. Cleanup runs before/after installation, after sessions
  close, and after daemon startup restoration. Agent launches pin their executable to the
  protected package so a concurrent update cannot change it between detection and execution.
  Open plain terminals conservatively delay cleanup (they can launch an agent at any time).
  Older daemons on the same shared storage also delay cleanup until closed or upgraded.
  Antigravity's replaced chat packages are reclaimed once their version is unused.
  Codex installs as its
  whole release package rather than just the entrypoint — it spawns companions
  (`codex-code-mode-host`, bundled rg/zsh) from beside its own executable, and an entrypoint-only
  install shipped a codex whose code mode failed closed. One install/update per agent (409 while
  running, either verb, including another workspace sharing the install). Antigravity installs its complete Google chat package alongside the
  terminal executable. Grok uses xAI’s standalone release. These two chat/runtime artifacts are
  fetched over HTTPS from the vendor; they do not publish separate checksums at these endpoints.
  Gemini CLI is retained for reading existing records, with no new managed install. Shims are written **only**
  when chimaera owns the binary (never shadow your own install) and theme injection is skipped when
  your own config already sets a theme (fill the gap, never fight a choice). Typing
  `<agent> update` in a chimaera terminal against a **managed** binary is intercepted by the shim
  with a pointer at chimaera's own updater (field failure: codex's self-updater can't detect an
  install method for a bare binary behind our symlink, and claude's would install a second copy
  elsewhere, leaving the managed link stale); your own binary's `update` passes through untouched.

## Agent detection & catalog

- **What & when.** The launcher popover's truth: which agents this host has (installed? version?
  outdated? newer release upstream?), their curated model lists, install hints, and (for claude)
  resumable past conversations.
- **Where it lives.** `launcher.rs` (`list_agents`, `detect`, `resolve_bin`, `models`,
  `is_outdated`, `claude_resumables`); upstream latest-release probes in `agent_updates.rs`.
  Routes `GET /api/v1/agents`, `GET /api/v1/agents/claude/sessions`.
- **Key behaviors.** Detection runs the four registered agents' login shells + version probes concurrently (serial would
  visibly stall the popover), with a 6s timeout backstopped by well-known paths. `is_outdated` flags
  npm-era codex (0.1.x). The Antigravity IDE's `agy` symlink (which just opens the GUI) is detected
  and refused. Resumables are scanned off the reactor, exclude transcripts already open in a live
  session, and are titled by the same custom > ai > first-prompt chain as live naming. Latest
  releases are probed from the same official endpoints the install scripts trust (bounded curl,
  10s/1MB) by a slow checker (every 6h, gated by `update.autoCheck` like the daemon's own release
  check); rows carry `latest_version`/`update_available` (never guessed — unparseable versions stay
  false), and `GET /api/v1/agents?check=true` (Settings' re-check) probes inline. The launcher
  never blocks on a probe — it reads the cache.

## The session rail — state, rename, kill

- **What & when.** The left rail lists every live session in the active workspace (terminals above
  agents) with at-a-glance state; it's where you focus, rename, and end sessions.
- **How it's used.** Click a row (or Enter/Space) to focus it in the current pane; holding the
  modifier fades in `⌘1–9` badges for direct switching. Double-click a label or `F2` to rename
  (an inline pin). Click `×` to end a session (a live one asks an inline confirm first).
- **Where it lives.** `App.svelte` (the `sessionRow` snippet, `startRename`/`requestKill`); session
  model + state helpers in `sessions.ts` (`dotState`, `isBusy`, `displayName`, `needsAttention`).
  Roster: `GET /api/v1/sessions` polled every 5s + `/ws/events` snapshots. Rename
  `PATCH /api/v1/sessions/{id}`; kill `DELETE /api/v1/sessions/{id}`.
- **Key behaviors.** State dot: running=accent "alive", needs_permission/idle_prompt=amber "attn",
  finished="done", errored="err", rate_limited="rate". Hook-less agent TUIs (Codex, Antigravity and Grok; legacy Gemini records)
  never get hook state — their busy signal is **output recency**: the PTY reader stamps
  `last_output_at` per chunk (`chimaera-pty`, kept off the wire), and `session_view.rs` derives a
  boolean `output_active` for agent rows still in state `unknown` (a working TUI streams tokens /
  animates its spinner continuously; quiet ≥2s = idle — the window sits above a working TUI's ~1 Hz
  repaint so a mid-turn agent never flaps). The dot reads accent "alive" while active, calm "idle"
  when quiet; only an old daemon without the field keeps the muted "unk". A claude TUI whose
  "running" claim has gone silent past the daemon's stall window (`stalled: true`, 180s) also
  drops to "unk" (tooltip "agent says working — no output for a while") — the claim is likely
  stale and the dot says so without touching the record. Every COUNT — the home-screen amber
  rollup, the rail pill, the focus strip, and the window-title prefix — is `needsApproval`
  (needs_permission only: a permission, plan approval, or question blocking the agent; a chat
  row's additive `needs_permission` counts the same, `awaitsDecision`), alive-gated
  because a crashed chat driver stays registered (alive:false, errored) until deleted. Finished and
  waiting-for-input sessions are news, not a number: they wear the unread mark (bold name + accent
  dot, see [notifications.md](notifications.md)). The dashboard's attention lane stays the broader
  `needsAttention` (needs_permission | idle_prompt | errored). Chimaera owns renaming for **all** session kinds (only claude has an
  in-TUI `/rename`); the pin outranks every derived name on every surface. Kill drops the row locally
  even if the DELETE fails (already-gone/unreachable), and **tombstones** the id until a daemon
  snapshot no longer lists it: `DELETE` only signals the process and returns while the session is
  still in the roster until its wait thread reaps it, so lifting the tombstone on the response let
  the row pop back for a beat and vanish again — a visible flicker over a remote link. A tombstone
  that a snapshot never confirms (the kill didn't take) is dropped 20s after the request settles and
  the roster refetched, so the truth returns. Rail rows are drag sources.
- **Agent status line (chat sessions).** When claude emits a `post_turn_summary` (its own post-turn
  "where things stand" one-liner, e.g. "workflow launched, 2 agents spawning"), the driver maps it
  to a latest-wins `SessionStatus` event: the chat row's name tooltip includes the line (`status_detail`
  on the row JSON), and a summary flagged `needs_action` lands the amber `idle_prompt` attention
  state. Emission is conditional and CLI-version-dependent (see `chimaera-agent/PROTOCOL.md`
  Pass 17) — the surface is dormant when the CLI stays quiet. TUI rows never carry it.

## Attention hooks (claude TUI)

- **What & when.** Claude Code TUI sessions POST their lifecycle hooks back to the daemon, which
  folds them into the rail's attention state and tail-polls the transcript for a title.
- **Where it lives.** `agents.rs` (`ingest`, `write_settings`, `write_mcp_config`). Route
  `POST /api/v1/agent-events/{id}?key=` (registered *after* the bearer layer; the per-session key
  in the URL authorizes it — claude's hooks can't know the daemon token).
- **Key behaviors.** Settings/mcp files are written 0600 (they embed the secret). Attention state
  is **Claude-only for TUIs**. In chat mode protocol events provide the richer state; Claude
  chat still receives hooks for context, agent-message delivery and transcript integration.

## Recents — resume ended conversations

- **What & when.** Below the rail: the workspace's *ended* agent conversations (any agent, newest
  first), remembered by the daemon across restarts. Pick a finished thread back up.
- **How it's used.** Click a `recent` row to reopen it (shows agent glyph, title, relative age).
  The section fills whatever height the sessions column has left and shows **as many rows as fit**
  (measured by a `ResizeObserver`, fixed 26px rows; a floor of 3 below which the column scrolls);
  "more" appears only when more exist and expands into a scrollable list ("less" collapses it);
  it and the all-sessions history mark beside it reveal while the pointer is over Recents
  (always shown on touch).
- **Where it lives.** `App.svelte` (`refreshRecents`/`openRecent`), `launcher.ts` (`listRecents`).
  Route `GET /api/v1/recents?workspace_id=` (server `recents.rs`); reopening rides
  `POST /sessions` with `resume` + `title_hint`. History replay: `chimaera-agent/src/transcript.rs`
  (`import_transcript`) + `journal.rs` (`seed_journal`), glued in `chat.rs`
  (`seed_resumed_journal`) and `api/sessions.rs` (`spawn_chat_ui`, explicit resume validation). Refetch
  driven by a `recents` epoch on `/ws/events`.
- **Key behaviors.** Reopening honors the conversation's last surface; legacy/scanned histories
  default to terminal, independently of the preference for new chats. A saved chat reopens with
  its name and history:
  the row's title seeds the soft `ai_title` (`title_hint`), and the journal is seeded from the
  previous life — copied when a chat journal exists, otherwise **imported from the claude
  transcript** (`~/.claude/projects/<enc>/<id>.jsonl` → `AgentEvent`s, bounded newest-tail with an
  explicit `Truncated` marker). A saved Claude chat whose history cannot be reconstructed reports
  a recoverable error with a terminal suggestion, rather than silently changing views. A Codex
  recent resumes on its last surface: `thread/resume` in chat, `codex resume <thread-id>` in the
  TUI. A normally finished Codex chat copies its native `ChatInfo` thread id into Recents before
  registry removal. Resume stays **honest per row**: a row with no captured native handle starts
  fresh and says so without claiming the agent lacks resume support; Claude handles additionally
  require a real transcript (Claude 2.1.x interactive sessions can persist none). Cap 20/workspace;
  live conversations are hidden at read time (they return when the session ends). Right-click a
  row → **Archive**, or the header → **Archive all** (with an Undo), hides conversations from the
  rail without deleting anything; a quiet **All sessions** link ends the list
  ([session-history.md](session-history.md#archiving-recents)).
- **Codex terminal capture (Pro-configured projects).** `codex_notify.rs` injects an argv `notify`
  wrapper for terminal launches and chat-to-terminal switches in projects with a Pro cloud profile;
  elsewhere a Codex TUI's argv and notify stay exactly the user's. It posts to the existing
  per-session-key hook route and then runs the user's notify argv unchanged: the one Codex itself
  would run (the project's own `.codex/config.toml` over its Codex home, where a `CODEX_HOME`
  exported by the login shell counts; probed once per daemon life). The wrapper lives under the
  daemon's data directory, not the runtime directory HPC hosts scrub. Verified against Codex 0.157.1: the payload has
  `type:"agent-turn-complete"`, `thread-id`, `turn-id`, `cwd`, `input-messages`, and
  `last-assistant-message`; the rollout starts with a `session_meta` record carrying `payload.id`
  and `payload.cwd`. Only matching on-disk rollouts mint terminal resume handles. Reads are capped
  (64 KiB header; 32,768 directory entries / two-second discovery budget), and the hook key stays
  in a private header file rather than process arguments. If user notify config cannot be read
  faithfully, its hook stays in place and capture is skipped with a daemon warning.

## Documents: the portable dialect, `check_document` and the issues chip

- **What & when.** Agents write reports, READMEs and notes the user reads in the markdown
  preview. Every session is taught a portable dialect (GFM, GitHub alerts, `$…$` math, mermaid,
  YAML frontmatter, relative links and embeds with fragments; it also renders on GitHub and in
  Obsidian) and can check a document before handing it over. The user sees the same findings as
  a chip on the markdown toolbar.
- **How it's used.** For sessions with Chimaera MCP integration (all four chat providers, Claude terminals
  and eligible Codex terminals), the chimaera MCP server's
  `initialize` instructions carry a six-line documents paragraph, `document_guide` (no args)
  returns the full guide, and `check_document {path}` returns a readable report (a relative path
  resolves against the session's cwd, then its workspace root). Both are read-only and
  pre-approved for native Claude/Codex sessions (`mcp::ALWAYS_ALLOWED_TOOLS`, beside
  `notify`); ACP providers retain their native approval behavior. In the preview, a quiet
  **"N issues"** chip appears on the markdown toolbar when the document has errors or warnings
  (red with any error, amber otherwise; notes only appear in its popover); clicking an issue
  reveals its line in reading, live or source. For agents launched **outside** Chimaera,
  Settings → **Documents** offers two opt-in writes, each behind a dialog showing the exact
  text: a delimited `<!-- chimaera:docs:start -->…<!-- chimaera:docs:end -->` section in the
  workspace's `AGENTS.md` (created if missing; re-running replaces only the block), and a Claude
  Code skill at `~/.claude/skills/chimaera-docs/SKILL.md`.
- **Where it lives.** Daemon `crates/chimaera-server/src/doc_check.rs` (the checker, the route,
  the MCP report), `agent_docs.rs` (the installs and their text), `doc_guide.md` (the guide),
  `mcp.rs` (`DOCUMENTS_INSTRUCTIONS`, the two tools). UI
  `web-ui/src/lib/previews/DocIssues.svelte` (mounted in `MarkdownView`'s toolbar),
  `web-ui/src/lib/settings/{DocumentsSettings.svelte,agentDocs.ts}`. Routes
  `GET /api/v1/fs/check_document?path=&root=` → `{issues: [{line, severity, code, message, fix}],
  truncated}`, `GET /api/v1/agent-docs?workspace_id=` (each target's path, state and exact text),
  `POST /api/v1/agent-docs/install {target: "agents_md"|"claude_skill", workspace_id?}` →
  `{target, path, changed, created}`; all bearer-authed.
- **Key behaviors.** The checker parses the document exactly as the reading view does (the same
  comrak extensions, frontmatter rule and `$$` promotion; lines stay in source numbering), then
  scans the lines outside code. **Errors:** broken relative links and embeds (with a case-mismatch
  or root-relative fix when one exists; a link that differs from the file on disk only in case is
  broken even where the filesystem ignores case, since the Linux hosts and GitHub do not), `#heading` fragments missing from a target `.md` (GitHub
  slug rule, the nearest slug suggested), `#L` ranges past a file's end, same-document anchors
  with no heading or footnote, dangling footnote references and unused definitions.
  **Warnings:** absolute local paths (`/home/`, `/Users/`, `/scratch/`, `/tmp/`, `~`, `file://`,
  `C:\`; the fix names the relative path), images without alt text or over 10 MB, wikilinks,
  MDX `import`/`export` lines and capitalized tags, `:::` fenced divs, MyST directives and roles,
  Markdoc tags, alert types GitHub does not render (with the standard type to use) or a title on
  the marker line, frontmatter lines that are not `key: value` or list shaped. **Notes:**
  duplicate heading slugs, plain `http://` links. Viewer locators (`#page=`, `#row=`, `#/json`)
  are not checked. Bounds: documents ≤ 4 MB, ≤ 500 issues (the least severe dropped first),
  ≤ 200 target stats, ≤ 20 target reads of ≤ 2 MB, a 10 s budget (the rest reported as one
  "unchecked" note), all under the shared filesystem limiter, off the reactor. Only regular
  files are ever opened (non-blocking, re-checked after the open, and read through the size cap),
  so a link to a device such as `/dev/zero` or to a FIFO is never read. The chip checks
  only while it can be seen (window visible, its tab's layer active), debounced after a disk
  change, and stays hidden when a check fails. Installs are atomic (temp, fsync, rename), refuse
  an existing file over 1 MiB or with an unmatched marker, and are idempotent (`changed: false`).
  The `AGENTS.md` merge re-reads the file right before its rename, so an edit that landed
  meanwhile (an agent, the user) is merged once more rather than overwritten, and a file that
  keeps changing is refused; a write refreshes the git panel and open previews like a save.
  There is no Codex skill install; Codex reads the `AGENTS.md` section.

## Status: partial

- Antigravity and Grok Build share ACP chat; their same-session terminal/chat switch and native
  rewind are not offered. Managed installs include their official native runtimes. Google's
  chat package is pinned independently of `agy`; Linux chat needs glibc. Third-party harness
  registration (including Pi) is [designed, not yet implemented](../agent-guides/agent-integrations.md).
- Gemini CLI is retired from new launches; existing records retain their identity.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why agents are launched this way
_Captured 2026-07-09 — drafted from docs/design/README.md + code, confirmed live with the maintainer._

- **Problem it solves.** Workspace-scoped agent sessions with attention state, replacing scattered
  agent chats — the workbench's reason for being.
- **Core.** The two-tier model with **Tier A (the real TUI) always fully supported and one toggle
  away**, plus `agents.defaultView` as the flip-back lever, is a **deliberate structural hedge**
  against the paused-billing risk — not an accident of history. Keep it.
- **Improvable (additions).** Which agents are first-class (gemini/antigravity today are sequencing,
  not a boundary), attention-state coverage, and the managed-install scope are additions that can
  grow.
- **Do not change:** Tier A staying fully supported and one toggle from chat.

### Why the rail shows the agent's own status line — _Intent pending_
_Shipped 2026-07-16 (claude `post_turn_summary` → `SessionStatus`). The maintainer's why has
not been captured yet — run **capture-feature-intent** for this entry._

### Why recents replay full history (and fall back to the terminal)
_Captured 2026-07-09 (from the maintainer, in-session)._

- **Problem it solves.** Opening a recent used to load "neither name nor history" — a resumed
  conversation the user couldn't recognize. The maintainer chose the fuller option explicitly:
  reconstruct the conversation in chat (importing the claude transcript when no chat journal
  exists) rather than resuming into a blank pane.
- **The fallback is deliberate**, in the maintainer's words: "if a chat can't be opened in a good
  way because it is too 'old' or something goes wrong, we could just open it in the TUI or as it
  is intended in the terminal" — a recent must never open as a blank chat.
- **How settled:** the outcome (name + history, or an honest terminal) is the requirement; the
  import mechanics (bounded tail, `Truncated` marker, reuse of the driver's block mapping) are
  implementation, free to improve.

### Why the launcher spells out open-vs-terminal
_Captured 2026-07-09 (from the maintainer, in-session)._

- **Problem it solves.** How a row would open was invisible (a setting decided it). The maintainer
  specified the row design himself: provenance/version as a subheader under the name, an `open`
  affordance plus a small terminal-icon button on the right, and **"the default should be UI so if
  I press the whole button thing it should open it in the UI"**.
- **How settled:** chat-by-default from the launcher row and the explicit per-spawn terminal
  affordance are deliberate; the exact visuals can evolve.

### Agent update awareness & one-click managed updates — why it exists
_Captured 2026-07-16 (from the maintainer, in-session)._

- **Problem it solves — both halves equally.** The field dead end (a managed codex's own
  `codex update` on an HPC login node failing with "could not detect the installation method" — managed
  binaries had no update path at all) AND staleness awareness: agent CLIs release near-daily, and
  the user should see at a glance, on every host chimaera runs on, whether an agent is stale — and
  fix it in one click.
- **How settled (early feature):** all of it is provisional **except the why**. Even the
  managed-only action gate is today's choice, not a locked contract — the maintainer explicitly
  leaves open growing a personal-binary update path later (e.g. running the agent's own
  self-updater, `claude update`/`codex update`, in a visible terminal). The surfaces, the
  `→ <new>` affordance shape, the 6h cadence, the settings poll — additions, improvable freely.
- **Core bet — do not change: never guess `update_available`.** The signal is honest or absent —
  an unparseable version (either side) must never claim an update. Everything else in this area is
  an addition, open to change if improved.

### Agent-first documents (the dialect, `check_document`, the issues chip) — _Intent pending_

### Busy/idle session status — why it exists
_Captured 2026-07-11 (from the maintainer)._

- **Problem it solves.** An active (accent) dot should **mean something is actually happening** — not
  a perpetual green. A session is "active" only while a foreground command runs (OSC 133 / an agent
  turn); idle shells and between-turn agents read as a quiet dot.
- **Where it pays off.** It makes the update flow honest: the local-daemon update button warns only
  about **busy** sessions ("N busy will restart") — idle ones restore across the stateful restart.
  See also [terminals.md](terminals.md) (the terminal dot) and
  [lifecycle-and-persistence.md](lifecycle-and-persistence.md) (restart).
- **Grade — addition:** the keeper is the *principle* (status must be honest), not the specific state
  machine.


### More agents, one understandable experience
_Captured 2026-10-01 from the maintainer's requests in this session._

- **Problem it solves (verbatim):** “both Grok and Gemini are coming out with new models that I think we should 100% also be able to serve in Chat UIs the same way.”
- **Forks and correctness (verbatim):** “Can we not fix so one can fork all of those ? and we should not falsilely mislead users”. Reopening after updates was described as “quite wonky”, including a Claude terminal conversation opening in chat instead.
- **UX priority (verbatim):** “UI / UX needs to be an extreme priority and how easy it is to use for the user” and “we dont want weird jargon and too much stuff for the user that it can't understand”.
- **Google choice (verbatim):** “Only agy if that is the new one, I think we can even retire Gemini CLI if that is not supported”. This is a product choice for new integrations, not a claim that upstream Gemini CLI is unsupported.
- **Open direction (verbatim):** “how you can extend this to Pi harness etc as well (or any other harness you want) with plugins” and “whether or not we should keep only the main providers active right now and have other harnesses as plugins”.
- **How settled / core versus addition (verbatim):** “Four built-ins; others through Extensions, set can evolve.” The chosen set is an evolving addition, not a permanent list.
