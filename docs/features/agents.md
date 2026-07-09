# Agents — launch, lifecycle & runtimes

Launching and managing coding agents (`claude`, `codex`; `gemini`/`antigravity` are
detected but not first-class yet). An agent runs either as its **real interactive TUI** in
a daemon-owned PTY (Tier A — looks, behaves, and *bills* like any terminal) or as a
**structured chat session** (Tier B — see [chat-mode.md](chat-mode.md)) on the same session
identity. This page covers getting an agent running, the session rail that tracks it,
renaming/killing, managed installs, and resuming ended conversations.

**Where it lives (shared):** UI `web-ui/src/lib/workspace/{Launcher.svelte,launcher.ts,
sessions.ts}` + the rail/split-button in `web-ui/src/App.svelte`. Daemon:
`crates/chimaera-server/src/{api/sessions.rs,launcher.rs,agents.rs,runtimes.rs,agent_state.rs,
spawn.rs,recents.rs}`. Wire: `POST/GET/DELETE/PATCH /api/v1/sessions*`, `GET /api/v1/agents`,
`POST/DELETE /api/v1/agents/{id}/install`, `GET /api/v1/agents/claude/sessions`,
`GET /api/v1/recents`, `POST /agent-events/{id}?key=`, and `/ws/events` for the roster.

## Launching an agent

- **What & when.** Start a coding agent in the focused pane, as a TUI or a chat session.
- **How it's used.** `POST /api/v1/sessions` with `kind:"agent"`, `agent:"claude"|"codex"|…`,
  optional `model` (from the curated list), `resume` (claude only), `theme`, `cols`/`rows`, and
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
  a nested child. `resume` is claude-only (400 otherwise). Chat is gated to `chat_capable()`
  agents (claude / codex); gemini/agy are refused chat.

## The launcher — split button & popover

- **What & when.** The primary way to start an agent: one click spawns your persisted default;
  the popover answers "*which* agent" with provenance and install state.
- **How it's used.** In the rail's "agents" section, click `+ new agent <default>` (or `Mod2+E`)
  to spawn the default instantly. Hover/click the chevron for the popover: one row per known CLI
  with provenance, version, a `docs ↗` link, and install/update chips.
- **Where it lives.** `App.svelte` (`.new-split`, `spawnDefaultAgent`), `Launcher.svelte`,
  `launcher.ts` (`listAgents` → `GET /api/v1/agents`; `?refresh=true` bypasses the detection cache).
- **Key behaviors.** Default persists in localStorage (`chimaera.agentDefault`, falls back to
  `claude`). If the default is missing, the main surface doesn't spawn a doomed pane — it installs
  in place (if managed) or opens the popover. Provenance is stated in words: **"yours"** (your
  binary on PATH) vs **"chimaera"** (a build under `~/.chimaera/agents`), with the resolved path in
  the tooltip. The popover re-detects on open (renders cached rows, then swaps in fresh truth).
  Agents with no curated managed install (e.g. gemini) get no chip, only the docs link.

## Managed runtimes — install / update / theming shims

- **What & when.** Chimaera installs and updates the agent CLIs itself (curated scripts, official
  sources, checksum-verified, never sudo), streaming the installer into a visible terminal pane —
  and writes tiny theming "shims" that inject a scheme-matched theme into agent spawns.
- **How it's used.** Click an install/update chip → `POST /api/v1/agents/{id}/install {workspace_id}`
  spawns the curated command as an ordinary shell session you watch. `DELETE /api/v1/agents/{id}/install`
  uninstalls the managed copy (driven from the Agents settings panel).
- **Where it lives.** `runtimes.rs` (`install_agent`, `start_install`, `install_script`,
  `write_shims`, `regenerate_shims`).
- **Key behaviors.** Scripts are composed by the daemon (never the client), `set -euo pipefail`,
  HTTPS-only, no sudo, version charset-whitelisted, downloads in a `mktemp` dir. Layout
  `~/.chimaera/agents/<agent>/<version>/bin/` with an atomic per-agent symlink swap (running
  sessions keep their exec'd inode). One install per agent (409 while running). Gemini has no
  managed install (needs a node runtime — phase 2; POST → honest 400). Shims are written **only**
  when chimaera owns the binary (never shadow your own install) and theme injection is skipped when
  your own config already sets a theme (fill the gap, never fight a choice).

## Agent detection & catalog

- **What & when.** The launcher popover's truth: which agents this host has (installed? version?
  outdated?), their curated model lists, install hints, and (for claude) resumable past
  conversations.
- **Where it lives.** `launcher.rs` (`list_agents`, `detect`, `resolve_bin`, `models`,
  `is_outdated`, `claude_resumables`). Routes `GET /api/v1/agents`, `GET /api/v1/agents/claude/sessions`.
- **Key behaviors.** Detection runs three login shells + version probes concurrently (serial would
  visibly stall the popover), with a 6s timeout backstopped by well-known paths. `is_outdated` flags
  npm-era codex (0.1.x). The Antigravity IDE's `agy` symlink (which just opens the GUI) is detected
  and refused. Resumables are scanned off the reactor, exclude transcripts already open in a live
  session, and are titled by the same custom > ai > first-prompt chain as live naming.

## The session rail — state, rename, kill

- **What & when.** The left rail lists every live session in the active workspace (terminals above
  agents) with at-a-glance state; it's where you focus, rename, and end sessions.
- **How it's used.** Click a row (or Enter/Space) to focus it in the current pane; holding the
  modifier fades in `⌘1–9` badges for direct switching. Double-click a label or `F2` to rename
  (an inline pin). Click `×` to end a session (a live one asks an inline confirm first).
- **Where it lives.** `App.svelte` (the `sessionRow` snippet, `startRename`/`requestKill`); session
  model + state helpers in `sessions.ts` (`dotState`, `displayName`, `needsAttention`). Roster:
  `GET /api/v1/sessions` polled every 5s + `/ws/events` snapshots. Rename `PATCH /api/v1/sessions/{id}`;
  kill `DELETE /api/v1/sessions/{id}`.
- **Key behaviors.** State dot: running=accent "alive", needs_permission/idle_prompt=amber "attn",
  finished="done", errored="err", rate_limited="rate"; hook-less agents (codex/gemini TUIs) read a
  muted "unk" (honestly unknown). `needsAttention` = needs_permission | idle_prompt | errored feeds
  the home-screen amber rollup. Chimaera owns renaming for **all** session kinds (only claude has an
  in-TUI `/rename`); the pin outranks every derived name on every surface. Kill drops the row locally
  even if the DELETE fails (already-gone/unreachable). Rail rows are drag sources.

## Attention hooks (claude TUI)

- **What & when.** Claude Code TUI sessions POST their lifecycle hooks back to the daemon, which
  folds them into the rail's attention state and tail-polls the transcript for a title.
- **Where it lives.** `agents.rs` (`ingest`, `write_settings`, `write_mcp_config`). Route
  `POST /api/v1/agent-events/{id}?key=` (registered *after* the bearer layer; the per-session key
  in the URL authorizes it — claude's hooks can't know the daemon token).
- **Key behaviors.** Settings/mcp files are written 0600 (they embed the secret). Attention state
  is **claude-only for TUIs** (codex/gemini integrations haven't landed). In chat mode this path is
  bypassed — the protocol drives state instead (hooks are unreliable under `-p stream-json`).

## Recents — resume ended conversations

- **What & when.** Below the rail: the workspace's *ended* agent conversations (any agent, newest
  first), remembered by the daemon across restarts. Pick a finished thread back up.
- **How it's used.** Click a `recent` row to reopen it (shows agent glyph, title, relative age).
  Top 3 by default; "all N" expands.
- **Where it lives.** `App.svelte` (`refreshRecents`/`openRecent`), `launcher.ts` (`listRecents`).
  Route `GET /api/v1/recents?workspace_id=` (server `recents.rs`). Refetch driven by a `recents`
  epoch on `/ws/events`.
- **Key behaviors.** Resume is **honest per agent**: when the CLI supports it (claude) it spawns
  `claude --resume <id>` and says so; otherwise it starts a fresh session of the same agent and the
  tooltip says so. Only promises resumption a transcript can actually deliver — claude 2.1.204
  interactive sessions persist *no* transcript, so an unverified id is refused rather than minting a
  row that dies with "No conversation found". Cap 20/workspace; live conversations are hidden at
  read time (they return when the session ends).

## Status: partial

- **Gemini / Antigravity** are detected but not first-class: gemini has no managed install (400),
  antigravity's `agy` is refused, neither has hook-driven attention state.
- **Attention state** is claude-only for TUIs.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

_No intent captured yet — pending the next `feat:` in this area._ Open questions a future
capture should answer: which agents are first-class vs "phase 2" and why; how/when the
curated per-agent model lists are refreshed; the scope of what chimaera will manage-install.
