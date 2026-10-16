# Agents — launch, lifecycle & runtimes

Launching and managing Claude Code, Codex, Antigravity, Grok Build and Cursor CLI. Chimaera runs
the agent's own runtime as its **real interactive TUI** in a daemon-owned PTY
(Tier A) or as a **structured chat session** through its native protocol
(Tier B — see [chat-mode.md](chat-mode.md)). Claude Code and Codex can switch a
saved conversation between these views. This page covers getting an agent running,
the session rail that tracks it, renaming/killing, managed installs, and resuming
ended conversations.

**Where it lives (shared):** UI `web-ui/src/lib/workspace/{Launcher.svelte,launcher.ts,
sessions.ts}` + the rail/split-button in `web-ui/src/App.svelte`. Daemon:
`crates/chimaera-server/src/{api/sessions.rs,launcher.rs,agents.rs,runtimes.rs,agent_setup.rs,operation.rs,agent_state.rs,
spawn.rs,recents.rs}`. Wire: `POST/GET/DELETE/PATCH /api/v1/sessions*`, `GET /api/v1/agents`,
`POST/DELETE /api/v1/agents/{id}/install`, `GET /api/v1/agents/claude/sessions`,
`GET /api/v1/recents`, `POST /agent-events/{id}?key=`, and `/ws/events` for the roster.

## Your existing agent setup

Chimaera launches the agent's own runtime in your workspace, using that host's
account, environment, project instructions, skills, and configured connections.
A local workspace uses your local setup; an SSH workspace uses your user's setup
on the remote host. Chimaera adds workspace integration through session-scoped
settings, hooks, and MCP tools. Chat controls select the model and permissions
for the current session; see [chat mode](chat-mode.md) for provider behavior and
custom model selection. Settings → Chat → New Chat Model can follow the runtime's
configured model instead of Chimaera's last-used choice. A CLI's identity does not
establish its upstream provider; launcher subtitles describe installation provenance.

Claude Code 2.1.277+ reads `AGENTS.md` as a fallback when `CLAUDE.md` is absent.
Existing explicit `CLAUDE.md` → `AGENTS.md` bridges remain useful for older
runtimes and Claude-specific additions; Chimaera does not rewrite them at launch.
Before the first live model catalog arrives, Codex's curated fallback starts with
GPT-6.1 Sol. Once available, the host's account-specific catalog replaces it.

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
  non-interactive ssh PATH on HPC nodes), then the daemon's inherited PATH, well-known prefixes,
  and the managed bin dir. Fallback requires a completed shell probe; a timeout or startup failure
  asks the user to retry or set an explicit executable in Settings. A runnable explicit path wins
  without probing the shell; a
  hit is cached for the daemon's life, a miss for 90 s (`launcher::MISS_TTL`, so a catalog read
  with uninstalled agents doesn't pay the login-shell probe each time) — the launcher's own
  refresh, a runnable explicit path and a landed managed install see past it, and a changed path
  setting forgets every miss (`forget_misses`). `model`/`resume` are
  charset-validated (`safe_arg` refuses flag-shaped/control-byte values). The launcher env scrubs
  the daemon's own `CLAUDE_CODE_*`/`CLAUDE_AGENT_*` markers so a spawned claude doesn't think it's
  a nested child. Terminal `resume` is accepted for Claude and Codex (400 for other
  agents); all five chat adapters resume saved native sessions. Codex TUI resumes map to
  `codex resume <thread-id>`. Chat is gated to
  `chat_capable()` agents: Claude, Codex, Antigravity, Grok Build and Cursor CLI. Gemini CLI remains readable in old records but is retired from the new-agent catalog.

## The launcher — the new-agent row & the agent menu

- **What & when.** The primary way to start an agent. The rail's **Agents** section ends in one
  split row, **+ new agent · <saved default>**, styled like **+ terminal**. Every other agent waits
  one step away in the agent menu behind the row's chevron, so the rail never lists every runtime
  a host might have.
- **How it's used.** Click the row to start the saved default in the `agents.defaultView` surface
  (chat or terminal); ⌘/Ctrl-click starts it in the terminal. The same default is what ⇧⌘E and the
  dashboard's **+ new agent** start. With the worktree switch on, the row shows a branch glyph. A
  default that can't start keeps the row with a small accent step instead of spawning a doomed
  pane: **install** (curated install, opens the setup dialog), **set up chat**, or **set up** (no
  curated install → Settings → Agents).
- **The agent menu.** The chevron opens it on hover, click, or ArrowDown. The installed agents
  come first, the saved default leading (no built-in bias); the ones not installed stay folded
  behind one *other agents* row (remembered open/closed), and with nothing installed every agent
  shows. Each row is one line: glyph, name, version in mono (binary + path in the tooltip; a dated
  build such as Cursor's `2026.10.14-941492e` reads as its date), `→ <new>` when a newer release is
  known, and a terminal glyph that fades in on hover — the row itself opens the chat — or, when
  not ready, *not installed* / *needs an update* / *chat needs setup* with an install/update/set-up
  chip. Docs live in Settings → Agents; the menu has no docs links and no chat pill. The footer
  holds the **In a new worktree** switch in git workspaces (the `⌘↵ terminal` hint elsewhere) and
  **agent settings →**, which opens Settings at Agents.
  Keyboard: focus stays in the menu with a roving highlight (`aria-activedescendant`);
  Arrow keys wrap through the rows, the *other agents* row, the worktree switch and the settings
  link, Home/End jump, Enter does the row's
  action, ⌘/Ctrl+Enter opens the terminal, Escape closes and returns focus to the chevron.
- **Where it lives.** `App.svelte` (`.new-split`, `railStart`, `quickStart`, `openAgentSettings`,
  `launcherPick`), `Launcher.svelte`, `launcher.ts` (`primaryStart`, `primaryAgentId`,
  `groupAgents`, `quickAction`; `listAgents` → `GET /api/v1/agents`, `?refresh=true` bypasses the
  detection cache; `LaunchPick.ui` carries an explicit surface choice into `createSession`).
- **Key behaviors.** The default persists in localStorage (`chimaera.agentDefault`, falling back
  to `claude`); when the loaded catalog no longer lists it (e.g. gemini) the row starts the first
  installed agent in catalog order, else the catalog's first. A menu choice
  becomes the default; an explicit chat/terminal choice there also becomes the sticky
  `agents.defaultView`. When the daemon knows a strictly newer release, a chimaera-managed build
  gets an accent one-click update; your own install gets quiet information (chimaera never
  touches an install it doesn't own). The menu paints instantly from the window's last-known
  catalog and re-detects in the background; only a window that has never seen a catalog shows
  the pulse. The Mastermind dock still offers Claude Code and Codex and starts on whichever of
  them is the launcher default. Home has no agent list of its own.

## Managed runtimes — install / update / theming shims

- **What & when.** Chimaera installs and updates its own agent copies from official sources,
  with checksums when published and no sudo. Launcher and Settings → Agents open the same setup
  dialog with a short progress or result message. Host, paths, exit code, and installer output
  are available under Details. Installers
  run without a terminal or a session tab; opening or closing the dialog never starts an agent.
- **How it's used.** Choose Install, Update, or Advanced → Reinstall, then start the operation
  in the dialog. Keep working closes the dialog; View progress/result in Agents settings returns
  to it. Reopening from a stale launcher refreshes the catalog and recovers a completed background
  result rather than starting another install. The window remembers a pending operation across
  reloads until its result is shown, even if the catalog refresh was lost. Cancel stops only that
  installer. Failure and cancellation keep the last 64 KiB of output
  and the exit code when available; Retry is explicit. Storage/quota, permission, and incompatible
  system-library failures explain what to fix. Results last until the daemon restarts; a lost
  connection is unknown status, never success.
- **Installation is one step.** A zero installer exit code says files were installed, not that
  the account is signed in or structured chat works. Afterward the dialog shows the agent's
  [sign-in card](#sign-in) unless the CLI already reports a login (then "Signed in. Open chat
  when you're ready."); Open chat starts a chat using the account on the host, and a chat started
  while signed out shows the same card in place of the startup error. The terminal is never the
  way in. Settings says “Installed · Supports chat and terminal” rather than claiming readiness.
  Running conversations and account data are kept.
- **Where it lives.** `agent_setup.rs` (on `operation.rs`, the bounded-operation engine it shares
  with the sign-in), `workspace/agentSetup.ts` (on `workspace/operations.ts`, the shared client
  half: phases, `operationRunning`, the request helper), and `workspace/AgentSetupDialog.svelte`: authenticated `GET/POST /api/v1/agents/{id}/setup` and
  `DELETE /api/v1/agents/{id}/setup/{operation}`. POST carries `workspace_id`, an
  `install|update|reinstall` action, and an idempotency `request_id`; same-daemon duplicate clicks
  join active work. The catalog's additive `setup` summary lets Settings rediscover it. One result
  per provider, bounded memory, a 15-minute deadline, and process-group cancellation keep work
  bounded. Polling pauses with a hidden document. Legacy `/install` and `/update` PTY endpoints
  remain compatible. Curated scripts/shims live in `runtimes.rs`; update discovery in
  `agent_updates.rs`. `DELETE /api/v1/agents/{id}/install` still removes only the managed copy,
  after Settings' confirmation, leaving account data and other installations alone. Removal refuses
  while any workspace holds a package or terminal lease, or an older daemon may still use it.
  Shared and legacy roots are checked together before any files are removed.
- **Key behaviors.** Scripts are composed by the daemon (never the client), `set -euo pipefail`,
  HTTPS-only, no sudo, version charset-whitelisted, downloads in a `mktemp` dir. Layout
  `~/.chimaera/agents/<agent>/<package>/bin/` with an atomic per-agent symlink swap (running
  sessions keep their existing package). Cluster workspace
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
  whole release package into a fresh directory even for a same-version repair, so it never
  replaces companions a running session still needs. It installs the package rather than just the entrypoint — it spawns companions
  (`codex-code-mode-host`, bundled rg/zsh) from beside its own executable, and an entrypoint-only
  install shipped a codex whose code mode failed closed. One install/update per agent across both setup APIs (409 when another workspace holds the shared lock). Antigravity installs its complete Google chat package alongside the
  terminal executable. Grok uses xAI’s standalone release. Cursor CLI installs the package the
  official installer names (see [Cursor CLI](#cursor-cli)). These two chat/runtime artifacts are
  fetched over HTTPS from the vendor; they do not publish separate checksums at these endpoints.
  Gemini CLI is retained for reading existing records, with no new managed install. Shims are written **only**
  when chimaera owns the binary (never shadow your own install) and theme injection is skipped when
  your own config already sets a theme (fill the gap, never fight a choice). Typing
  `<agent> update` in a chimaera terminal against a **managed** binary is intercepted by the shim
  with a pointer at chimaera's own updater (field failure: codex's self-updater can't detect an
  install method for a bare binary behind our symlink, and claude's would install a second copy
  elsewhere, leaving the managed link stale); your own binary's `update` passes through untouched.

## Sign in

- **What & when.** Every built-in agent signs in from the app: no terminal, no "run `x login`
  and start the chat again". The daemon knows per agent whether its CLI is signed in on the
  workspace host, and one small flow signs it in — from the agent's card in Settings → Agents,
  from the launcher row (a signed-out agent reads *not signed in* with a **sign in** chip), and
  in a chat that could not start because the agent is signed out, where the card replaces the
  raw error and the chat starts itself once the login lands, and at the tail of a live chat whose
  CLI turns out to be signed out — Codex's app-server starts without a login and fails only at the
  first turn (`401 Unauthorized`), so an auth-shaped turn error re-asks the status command and,
  when it says signed out, the card appears there with **Check again** (a login done in a
  terminal or another window) and the composer open — only a chat that has not started yet keeps
  its composer closed ("sign in to Codex first") so its first turn isn't a 401. A sign-in that lands — from any card, Settings or
  another window — relaunches every chat of that agent that waited on it: the ones that died at
  startup for want of a login and the live ones started while the CLI said signed out (their
  process never reloads credentials). Same session id, a second life in the same journal; a
  conversation that had a prompt resumes by its native id, one that had none starts fresh (Cursor
  refuses to load an empty session). A chat mid-task is left alone, and a chat that completed a
  turn no longer counts as waiting. The daemon is the one owner of that relaunch; a card only
  refreshes the verdict. `/login` typed in a chat relaunches that chat too (`session_id` on the
  start request). No sign-in runs beside an install or update of the same agent.
- **How it's used.** Click **Sign in** — or type `/login` in any chat, which opens the card there
  even when the agent is signed in (switch accounts, refresh a stale login; Antigravity's headless
  login only finds the existing one, so its card offers no second sign-in). The card shows the page to open (**Open ↗** goes
  through the native shell's browser opener, so it works for a daemon on another host;
  **Copy link** for a browser elsewhere), a device code to enter on that page when the CLI's
  flow uses one, and a field to paste a code back when the page shows one. Finish in the
  browser; the card says *Signed in* when the CLI itself reports the login, and a waiting chat
  starts again in place. **Cancel** stops the CLI's login; a failure says what the CLI said and
  offers **Try again**. Antigravity's page waits 60 s for the callback (its CLI's limit), so a
  slow sign-in simply means **Try again**.
- **Per runtime.** Detection is each CLI's own status command, never a credential file;
  chimaera never reads, copies or refreshes a credential.

  | Agent | Signed in? | Sign in | In the card |
  |---|---|---|---|
  | Claude Code | `claude auth status --json` → `loggedIn` | `claude auth login` under the daemon (`BROWSER=true`, so only the app opens the page) | page + paste back the code the page shows |
  | Codex | `codex login status` (exit 0 "Logged in" / exit 1 "Not logged in") | `codex login --device-auth` | page + a device code to enter |
  | Grok Build | `grok models` ("You are logged in" / "You are not authenticated") | `grok login --device-auth` | page + a device code to enter |
  | Cursor CLI | `cursor-agent status --format json` → `isAuthenticated` | `NO_OPEN_BROWSER=1 cursor-agent login` | page |
  | Antigravity | `agy models` (lists the account's models; signed out it says "Please sign in", exit 1; any other failure is unknown) | `agy -p /skills` (no login command, but print mode signs in headlessly: the native zero-turn `/skills` triggers its OAuth URL and waits 60 s for Google's hosted callback; no stdin — print mode reads a piped stdin as its prompt and abandons the login — so no pasted code; a no-op `open`/`xdg-open` first on its PATH keeps it from opening a browser itself, it ignores `BROWSER` and `NO_BROWSER`) | page (finish within 60 s, else Try again) |

  A status command that does not answer in 10 s is *unknown* (nothing is offered), never a
  verdict. A device code is single-use and leaves the wire with the operation's end.
- **Where it lives.** `agent_auth.rs` (daemon; its operation runs on `operation.rs`, shared with
  the managed installs), `workspace/agentSignIn.ts` (on `workspace/operations.ts`) +
  `workspace/AgentSignInCard.svelte` (the one card; Settings, the chat view). Wire, all
  additive: `GET /api/v1/agents` rows carry `signed_in: true|false|null` and
  `sign_in: {how: browser, command, accepts_code, operation}`;
  `GET /api/v1/agents/{id}/sign-in[?check=true]` (the status, re-asked with `check`),
  `POST /api/v1/agents/{id}/sign-in {workspace_id, request_id, theme?}` (start or join — a
  repeated click, a second window — the one operation per agent),
  `DELETE …/sign-in/{operation}` (cancel, generation-scoped), `POST …/sign-in/{operation}/code
  {code}` (the pasted code, for a login that reads one). A relaunch after a failed startup is
  `POST /sessions/{id}/view {ui: "chat"}` on the dead chat, for every agent (a surface switch
  stays Claude/Codex-only); `{ui: "chat", relaunch: true}` does the same for a live chat.
- **Key behaviors.** The catalog answers at once: the verdict cached in the last 10 minutes, or
  *unknown* while a missing one is probed in the background (the catalog refetches on the change
  bus), so a launcher paint never waits on five CLIs and `?refresh=true` forces no probe. A probe
  runs inline only for an explicit ask — Settings' Check (`?check=true`), a card's Check again, a
  sign-in's end, a chat's startup failure (so the row and card say *not signed in* when that is
  why) — and like a chat it runs the CLI through the login shell with the host prelude, in its own
  process group, output capped. A verdict is the CLI's own words only: an offline or quota failure
  is unknown, never "signed out". A startup failure
  whose words are a missing login, or a chat started while the status said signed out, marks the
  session as waiting on a sign-in (`AgentRecord.signed_out_at_start`); a landed sign-in relaunches
  those (`chat::relaunch_after_sign_in`, at most 8) and clears the mark. The login runs
  as its own process group with a 10-minute deadline, output private to the daemon (64 KiB;
  only the URL, the code and a short failure tail reach the wire), no session or PTY entry;
  cancel is SIGTERM then SIGKILL to the group. A clean exit is never success by itself — the
  status command is asked again. Concurrent status probes for one agent share a run. The
  chat view shows one line per failure: a startup error that a fatal banner shows
  is not repeated as a transcript line, and a fatal banner is not followed by *agent exited*.

## Cursor CLI

- **What & when.** Cursor's terminal agent (`cursor-agent`, linked as `agent` too) as a fifth
  built-in runtime: its own TUI in a PTY, or structured chat over ACP.
- **How it's used.** Start it from the agent menu (under *other agents* while it isn't installed),
  or make it the default.
  The CLI keeps its own sign-in, done from the app ([Sign in](#sign-in)): a chat started while
  signed out fails fast — never a browser opened from the daemon — and shows the sign-in card.
  The terminal view shows Cursor's "Press any key to log in…" until then.
- **Where it lives.** `agent_state.rs` (`AgentKind::Cursor`, id `cursor`, `path_bin()` =
  `cursor-agent`), `launcher.rs` (detection, `build_acp_chat_command` → `cursor-agent acp`),
  `chat.rs` (ACP spawn, `NO_OPEN_BROWSER=1`), `runtimes.rs` (managed install, `cursor-agent`
  shim), `agent_updates.rs` (release from the official installer),
  `chimaera_agent::acp::CURSOR` (pinned to `TESTED_CURSOR_VERSION` 2026.10.14-941492e).
- **Key behaviors.** Detection searches the PATH for `cursor-agent`, never `cursor` — that name
  is the Cursor editor's app launcher — and the shim is named `cursor-agent` for the same
  reason. Chat is the shared ACP adapter: approvals, cancel, queued sends, resume through
  `session/load`, conversation-copy forks; models and the `agent`/`plan`/`ask` modes appear only
  as the signed-in session advertises them (no curated list, no custom model input, no native
  terminal↔chat switch). Cursor's blocking `cursor/ask_question` and `cursor/create_plan`
  requests are refused with a notice pointing at the terminal view. The managed install reads
  the release from `https://cursor.com/install`, downloads
  `downloads.cursor.com/lab/<version>/<os>/<arch>/agent-cli-package.tar.gz` over HTTPS (Cursor
  publishes no checksum), unpacks the whole ~600 MB package (bundled node + modules) into
  `~/.chimaera/agents/cursor/<package>/` with `bin/cursor` linking its entrypoint, and never
  writes `~/.local/bin`. Releases are dated builds (`2026.10.14-941492e`): updates order by
  date, and two builds of one date are never claimed newer.
- **Extensions.** Cursor is a core agent of Extensions → Connections (`cursor-agent mcp list`,
  with **Sign in in a terminal** for a server that needs a login) and of the Plugins group (a
  notice: its CLI lists no plugins or skills) — [plugins.md](plugins.md#antigravity-grok-build-and-cursor-cli).
- **Status.** Terminal use and detection/installation are verified; chat turns await a
  signed-in run of `CHIMAERA_TEST_CURSOR=… just chat-smoke-acp` (PROTOCOL.md, "Cursor CLI over
  ACP").

## Agent detection & catalog

- **What & when.** The launcher popover's truth: which agents this host has (installed? version?
  outdated? newer release upstream?), their curated model lists, install hints, and (for claude)
  resumable past conversations.
- **Where it lives.** `launcher.rs` (`list_agents`, `detect`, `resolve_bin`, `models`,
  `is_outdated`, `claude_resumables`); upstream latest-release probes in `agent_updates.rs`.
  Routes `GET /api/v1/agents`, `GET /api/v1/agents/claude/sessions`.
- **Key behaviors.** Detection runs the registered agents' login shells + version probes concurrently (serial would
  visibly stall the popover), with a 6s timeout and a 64 KiB output cap. A runnable
  `agents.<id>.path` wins, followed by the interactive login shell's choice. Only a completed
  shell probe with no choice falls back to the daemon's inherited PATH (absolute directories,
  in order), well-known installer paths, then the managed install. This covers nvm and other
  version managers through PATH without guessing among their installed versions. Versions are
  never ranked: an explicitly selected older install remains authoritative. Invalid explicit
  paths retain the existing fallback behavior. A timeout, failed startup, incomplete answer, or
  vanished shell-selected executable returns an actionable error instead of silently selecting
  another installation. Timed-out probes kill their shell and startup children. Successful
  resolutions log the agent, source, and path; failures log their reason. A hit is cached for
  the daemon's lifetime (stat-validated, so an in-place update is noticed); a miss is stamped for 90 s
  and served to **the catalog read only** (`GET /agents` without `refresh`), so a launcher paint or a
  Settings poll doesn't re-run a login shell per uninstalled agent per call. Everything else probes:
  a spawn, a refresh (the launcher's own open, Settings' load), Extensions' Check again (which also
  forgets the stamps), and a changed `agents.<id>.path` (which forgets them too). A refresh whose
  probe fails keeps a hit that still validates and logs that decision — a slow login shell never
  replaces a known executable with a fallback. Without a prior hit, the failure is served to catalog
  reads for the same 90 s window, while the next spawn or explicit refresh retries immediately. `is_outdated` flags
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
  finished="done", errored="err", rate_limited="rate". Hook-less agent TUIs (Codex, Antigravity, Grok and Cursor; legacy Gemini records)
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
  (needs_permission only: a permission, plan approval, or question blocking the agent), alive-gated
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
  explicit `Truncated` marker; only user/assistant records count toward that tail, so the per-request
  `api-request*` records Claude 2.1.300 writes there never crowd out the conversation). A saved Claude chat whose history cannot be reconstructed reports
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

## Where each agent keeps a conversation (resume from any folder)

- **What & when.** Chimaera resumes a native conversation after a restart, from Recents and from
  history. An agent's folder can change under it — a session started in a worktree, a Claude agent
  that calls `EnterWorktree` mid-conversation, a worktree removed since — so each runtime's lookup
  has to match how that runtime files conversations.
- **Where it lives.** `claude_store.rs` (the one Claude lookup: `find`/`find_blocking`,
  `find_known` + `worktree_index` for history's per-row check, `claude_worktree_dirs`), used by
  `chat.rs` (`resurrect_chat`, `switch_view`, `seed_resumed_journal`), `ledger.rs`
  (`resolve_resume`), `recents.rs` (`retire_inner`, the Recents scan), `history/routes.rs`
  (`reopen`) and `launcher.rs` (`claude_resumables`). Codex: `chimaera-agent/src/codex.rs`
  (`thread/resume`). ACP: `chimaera-agent/src/acp.rs` (`session/resume` / `session/load`).
- **Key behaviors.**
  - **Claude** files a conversation under the folder it is working in:
    `~/.claude/projects/<encoded cwd>/<id>.jsonl`, the encoding folding every non-alphanumeric
    character into `-`. `EnterWorktree` moves the file to the worktree's directory
    (`<root>--claude-worktrees-<name>`). Chimaera looks, in order: the transcript path Claude's
    hooks reported (`transcript_path`, now recorded for chats as well as terminals in the ledger,
    Recents and history), the session's own cwd, the workspace root, then the root's own Claude
    worktree directories (one bounded `read_dir` of the store, exact `<id>.jsonl` only; the
    `--claude-worktrees-` part keeps a sibling such as `proj-pro` out). Claude itself resumes by id
    from the repository root even after the worktree is gone (claude 2.1.300, `--resume <id>`, no
    fork), so a chat that entered a worktree and a chat whose managed worktree was removed both come
    back with their memory.
  - **Codex** keeps rollouts by date and thread id (`~/.codex/sessions/YYYY/MM/DD/rollout-…-<thread
    id>.jsonl`), independent of the folder, and resumes in-protocol (`thread/resume`; `codex resume
    <id>` in the TUI). A thread started in one folder resumes from another, including after the
    first folder is deleted (codex 0.162.1). Chimaera passes the id straight through.
  - **Antigravity, Grok and Cursor (ACP)** resume by session id through the adapter
    (`session/resume` when the agent advertises it, else `session/load`, both with the cwd). Antigravity keeps conversations
    by id with the cwd as metadata (`~/.gemini/antigravity-acp/conversations/<id>.*`). Grok files
    sessions under a directory per cwd (`~/.grok/sessions/<url-encoded cwd>/<id>`), so a Grok chat
    resumes in the folder it ran in: a worktree session comes back while its worktree exists;
    after the worktree is removed it starts at the root, where Grok may not find it.
  - A resurrected chat runs in the folder it ran in while that still exists, else the workspace
    root; a Recents row keeps that folder too and reopens there.

## Documents: the portable dialect, `check_document` and the issues chip

- **What & when.** Agents write reports, READMEs and notes the user reads in the markdown
  preview. Every session is taught a portable dialect (GFM, GitHub alerts, `$…$` math, mermaid,
  YAML frontmatter, relative links and embeds with fragments; it also renders on GitHub and in
  Obsidian) and can check a document before handing it over. The user sees the same findings as
  a chip on the markdown toolbar.
- **How it's used.** For sessions with Chimaera MCP integration (all five chat providers, Claude terminals
  and eligible Codex terminals), the chimaera MCP server's
  `initialize` instructions carry a six-line documents paragraph, `document_guide` (no args)
  returns the full guide, and `check_document {path}` returns a readable report (a relative path
  resolves against the session's cwd, then its workspace root). Both are read-only and
  pre-approved for native Claude/Codex sessions (`mcp::ALWAYS_ALLOWED_TOOLS`, beside `notify`,
  `open_browser` and `update_plan`); ACP providers retain their native approval behavior. In the preview, a quiet
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

- Cursor CLI chat is implemented from the signed-out handshake and Cursor's ACP docs; its turn
  wire awaits a signed-in `just chat-smoke-acp` run.
- Antigravity, Grok Build and Cursor CLI share ACP chat; their same-session terminal/chat switch and native
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

### Why the rail has one new-agent row and the menu holds the rest — _Intent pending_

- **Problem it solves (verbatim, issue #283):** “I think new agents in left pane, Claude Code + Codex default, then others ? takes you to settings agent view maybe.”

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
