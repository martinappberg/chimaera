# Terminals

Persistent, daemon-owned terminal sessions — plain shells and agent TUIs alike. The
terminal is the product's core primitive: the child process lives in the daemon, not the
window, so closing the laptop or dropping the socket doesn't kill it. The client is an
ephemeral xterm.js that renders bytes; **all terminal state is server-side**.

**Where it lives (shared):** the engine is `crates/chimaera-pty/src/`
(`lib.rs` `SessionManager`, `session.rs`, `snapshot.rs`, `marks.rs`, `exec.rs`, `tests.rs`);
the daemon bridge is `crates/chimaera-server/src/{ws.rs,spawn.rs,exec.rs,api/exec.rs}`; the
client is `web-ui/src/lib/terminal/` (`Terminal.svelte`, `termPool.ts`, `ws.ts`, `links.ts`)
with reconnect in `web-ui/src/lib/net/reconnect.ts`. Wire: `GET /ws/sessions/{id}` (byte
pipe), `POST /api/v1/sessions` (spawn), `POST /api/v1/sessions/{id}/exec`,
`GET /api/v1/sessions/{id}/journal`. Rules: [rules/pty.md](../../.claude/rules/pty.md).

## Persistent sessions & server-side state

- **What & when.** Every terminal is a long-lived daemon child. Reopen a window/tab and the
  terminal is exactly where it was; run with zero attached clients and it keeps going.
- **How it's used.** The server calls `SessionManager::spawn(SpawnOpts{cwd,name,cols,rows,
  command,id,env,…})` (`command:None` = the user's interactive shell). The browser opens
  `GET /ws/sessions/{id}`: first frame `{type:auth,token,cols,rows}` → `ready` → a binary
  **snapshot** → the live byte stream.
- **Where it lives.** `chimaera-pty/src/lib.rs` (`SessionManager`, `SpawnOpts`, `SessionInfo`),
  `session.rs` (`Session::spawn` — reader/writer/reaper threads), `snapshot.rs`
  (`render_snapshot`). Daemon: `spawn.rs`, `ws.rs::session_ws`.
- **Key behaviors.** The full screen (scrollback, colors, cursor, title, private modes) lives
  in a headless `alacritty_terminal::Term`. On attach the server re-emits a self-contained
  **ANSI escape snapshot** that rebuilds the exact grid — **never serialize the `Term` grid
  across the wire.** The snapshot must round-trip (`snapshot_replay_matches_live_grid` in
  `tests.rs` is the contract). Accepted gap: a resync while on the *alternate* screen can't
  restore the *primary* screen's scrollback. Default scrollback 10,000 lines
  (`daemon.scrollbackLines`). Exited children unregister themselves (tmux semantics).
  `SessionInfo` **is** the daemon↔UI wire contract — don't drift its shape.

## Reconnect, resize & resync

- **What & when.** The terminal reconnects forever after an unclean close (sleep, network blip,
  daemon restart) and rebuilds the exact screen; resizing reflows the PTY and the headless Term
  together.
- **How it's used.** Invisible in the happy path. On reconnect the screen is wiped and rebuilt
  from a fresh snapshot; the client sends its current grid in the `auth` frame so the server
  adopts client dims *before* rendering.
- **Where it lives.** `web-ui/src/lib/terminal/ws.ts` (`SessionSocket`), `net/reconnect.ts`
  (`Reconnector`), server `ws.rs` (`resync`, `authenticate`), `session.rs::resize`.
- **Key behaviors.** The snapshot must render at the grid it was captured at — resize *before*
  `term.reset()` or every soft-wrapped row re-wraps wrong. `resize` rejects 0×0, no-ops on
  unchanged dims, and resizes the PTY first (the only fallible step). A *foreign* resize (another
  window on the same session) repaints after a `RESYNC_DEBOUNCE = 120ms` window; the resize
  *initiator* is skipped (its xterm already reflowed — resyncing it is the "terminal resets when
  I change font size" bug). `unknown_session` is retried a bounded number of times (mid
  view-switch race) before going fatal. A broadcast-lagged client is `Lagged` → resynced, never
  buffered without bound (login-node RSS discipline; `OUTPUT_CHANNEL_CAPACITY = 4096`).

## Warm terminal pool (client)

- **What & when.** Each session keeps one long-lived xterm instance + open socket that survive
  tab switches and pane moves, so switching away and back is instant with nothing reflowed.
- **Where it lives.** `web-ui/src/lib/terminal/termPool.ts` (`pool`, `attach`/`show`/`release`,
  hidden `stash`, `POOL_CAP = 12`, `evictLru`), `Terminal.svelte`.
- **Key behaviors.** Up to 12 terminals stay warm; the LRU *parked* one is disposed past the cap.
  A visible terminal outlives its dead session on purpose (an agent's last words stay on screen
  until you close the tab). WebGL renderer with DOM fallback on context loss; fonts are awaited
  before first open so glyph metrics measure correctly. Refits are debounced 80ms and suppressed
  during divider/sidebar drags. New sessions spawn at the pane's estimated grid so a TUI never
  boots at 80×24 then visibly reshuffles.

## Clickable path links

- **What & when.** File/dir paths printed by any process become underlined links: click a file
  to open it in a pane, a directory to reveal it in the Finder + file tree.
- **How it's used.** Hover a path; if the daemon confirms it exists, it underlines. A file opens
  in the **active pane** (reusing an already-open tab); a directory opens/reuses the Finder beside
  the terminal. Cmd/Ctrl+click forces a new split. A trailing `:42` line suffix is carried.
- **Where it lives.** `web-ui/src/lib/terminal/links.ts` (`PathLinkProvider`), validation via
  `POST /api/v1/fs/validate` (`fsValidate` in `web-ui/src/lib/previews/files.ts`).
- **Key behaviors.** **URLs are deliberately *not* linkified** (no `web-links` addon loaded).
  Bare single-segment names (`crates`, `justfile`) link only on hover and only on a line shape
  prose never has (a full `ls`/`ls -l` line) — because the daemon runs on shared login nodes and
  prose must never be mass-validated. Verdicts cached 15s; requests batched + deduped; a
  whole-viewport prefetch keeps hovers instant. Relative paths resolve against the session's live
  cwd first, then the workspace root; a bare `name.ext` that misses both also links when exactly
  one file in the workspace bears that name (an agent saying `FIGURE_PLAN.md` about
  `paper/FIGURE_PLAN.md`) — the daemon answers from the bounded quickopen index and refuses on
  ambiguity, so links stay existence-verified. Chat prose paths share the same endpoint and
  fallback.

## Clipboard, selection & copy provenance

- **What & when.** Select and copy terminal text; optionally copy-on-select; agent "copy"
  commands reach the system clipboard; pastes are surfaced so an agent composer can source-tag them.
- **How it's used.** Drag to select → Cmd+C (macOS) / Ctrl+Shift+C (else). With
  `terminal.copyOnSelect`, selecting copies immediately. When a terminal owns a selection the pane
  bar grows a quiet "@ reference" action (`⇧⌘R` / `Ctrl+Shift+R`) that sends the selection into a
  target agent.
- **Where it lives.** `termPool.ts` (`registerTerminalClipboard`), `web-ui/src/lib/shared/reference.ts`,
  `PaneTabs.svelte`.
- **Key behaviors.** Bare Ctrl is never intercepted — it stays SIGINT/tmux/EOF for the PTY. OSC 52
  *writes* are honored (a remote agent's only path back to the Mac clipboard), but rejected before
  decode above 1.4M base64 characters (about 1 MiB decoded); OSC 52 *reads* are silently swallowed
  so a process can't exfiltrate the clipboard over the PTY.

## Live theming

- **What & when.** Terminal font, size, line height, cursor, scrollback, and a per-theme 16-color
  ANSI palette apply live to every warm terminal on a settings or system-theme change.
- **Where it lives.** `termPool.ts` (`applySettingsToPool`, `themeFromTokens`), palettes in
  `web-ui/src/lib/settings/themes.ts`.
- **Key behaviors.** Default 13.5px JetBrains Mono; min-contrast-ratio 3.0 lifts illegible
  256-color grays without recoloring intended secondary text. Each theme carries its own ANSI
  palette — a UI theme without a terminal palette is "half a theme". Metrics-affecting changes
  trigger a refit.

## The exec engine and command journal

- **What & when.** Type a command into a live shell and wait for its outcome (exit, output, cwd) —
  the mechanic behind an agent's `run_in_terminal` and the daemon's REST exec. A bounded **command
  journal** records what ran; agents read the journal instead of raw scrollback.
- **How it's used.** `POST /api/v1/sessions/{id}/exec {command, timeout_ms?, queue_timeout_ms?}`;
  `GET /api/v1/sessions/{id}/journal?limit=` reads the journal. Same core drives MCP `run_in_terminal`
  / `read_terminal` (see [linked-terminals.md](linked-terminals.md)).
- **Where it lives.** `chimaera-pty/src/exec.rs` (`ExecOptions`, `ExecOutcome`, integrated vs
  sentinel mode), `marks.rs` (`Marks`, `ShellPhase`, `CommandView` from OSC 133/633;E/7), server
  `exec.rs` (`run_exec`, transport-neutral), `api/exec.rs`.
- **Key behaviors.** Two modes: **integrated** (shell integration active → OSC 133 marks delimit
  output + carry exit code) and **sentinel** (no integration → wrap in printf-emitted marks, zero
  remote install). A busy integrated shell **queues** the exec (bounded by `queue_timeout`) rather
  than typing over a running command. A **half-broken integration** (prompt marks fire but a typed
  command never produces 133;C — e.g. an rc chain that kills bash's DEBUG trap) fails that exec
  once (504 never-started), and the session degrades to sentinel mode for later execs; any
  shell-emitted 133;C restores integrated mode. The bash integration itself re-arms its DEBUG
  trap from PROMPT_COMMAND at every prompt, so audit-shell rc chains (Sherlock's user-audit trap
  on bash 4.2) can't silently revert the hook. Exec refuses agent sessions (409 — typing into a
  claude TUI is chaos; exec is terminals-only). `ExecOutcome` is a wire type. Journal caps:
  `MAX_RECORDS 500`, `TOTAL_OUTPUT_BUDGET 8 MiB`, the scanner runs independent of the alacritty
  grid so journal reads never need the term lock.

## Kill & last words

- **What & when.** Closing a session politely SIGHUPs the child (so a shell runs exit traps and
  vanishes tmux-style), escalating to SIGKILL after a grace if it's ignored. A fast-failing process
  still shows its final screen instead of a blank pane.
- **Where it lives.** `session.rs::kill` (`KILL_ESCALATION_GRACE = 2s`), `lib.rs::kill_all`,
  `lib.rs` (`LastWords`, `LAST_WORDS_MAX_BYTES = 2 MiB`). CLI daemon-stop is a different path
  (`crates/chimaera/src/kill.rs`).
- **Key behaviors.** Escalation is a detached thread (non-blocking) — a caller about to `exit()`
  must wait the grace after `kill_all` for the force-kill to land. The reaper snapshots the final
  screen *before* unregistering (a 60ms drain lets the reader catch the last bytes).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why terminals work this way
_Captured 2026-07-09 — drafted from DESIGN.md + code, confirmed live with the maintainer._

- **Problem it solves.** Replace tmux/zellij nested inside code-server. Server-side terminal state
  with instant lossless reattach is *the* fix for code-server's terminals-die-on-reload; plain
  persistent shells are first-class workspace sessions, so this replaces tmux itself, not just the
  agent chats.
- **Core.** This is core — server-side terminal state (never serialize the grid), daemon-owned
  persistence, and exited-shells-vanish (tmux semantics: no gray corpse rows) are load-bearing bets,
  not conveniences.
- **Do not change:** server-side terminal state and the vanish-on-exit semantic.
- **Incidental (not intent).** The bell being ignored is an implementation detail, not a decision —
  don't treat it as a promise either way.
