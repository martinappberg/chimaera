# Performance audit & fix plan — 2026-08-31

Trigger: switching between a bash terminal tab and a chat tab hangs/lags badly;
general battery-drain suspicion. This document records what the audit found
(static sweep of the UI + daemon by four parallel reviewers, plus live
measurement against an isolated daemon) and the fix plan. Findings carry
file:line evidence and, where reproduced, numbers.

**Verdict up front: no heavy refactor.** The architecture — daemon-owned
sessions, warm socket pools, windowed transcripts, event-driven `/ws/events`,
bounded everything on the daemon — measured *sound* (idle daemon with 15 live
sessions: 0–1.3% CPU on a debug build; small-transcript tab switches: zero
long tasks). The lag and the battery cost come from ~10 specific, localized
mechanisms. Four bounded PRs fix them without touching the wire contract.

## Live measurements (isolated daemon, debug build, 1440×900)

| Scenario | Result |
|---|---|
| 6× dashboard↔terminal switches, 50k-line scrollback, pooled | 0 long tasks, 0 dropped frames |
| 6× chat↔terminal switches, short transcript | 0 long tasks |
| Busy terminal visible (unthrottled `seq` loop) | 0 long tasks (WebGL renderer copes) |
| Busy terminal **hidden** behind chat tab | daemon 11–18% CPU + renderer 10–17% CPU, continuously |
| Switch back to busy pooled terminal | 0 long tasks (warm pool — by design, good) |
| Switch to a terminal **evicted** from the 12-slot pool (10k scrollback) | one 95 ms long task (fresh attach + full snapshot); scales ~linearly with scrollback (200k max ⇒ ~2 s) |
| 6× chat↔terminal switches, transcript **at the 2000-block cap** (750 fake turns, tiny messages) | 249 ms + 116 ms freezes on first re-activations, ~50–60 ms steady tax after |
| Idle daemon, 15 sessions | 0–1.3% CPU |

The capped-chat number is the smoking gun for the reported hang: with tiny
filler messages it is already 250 ms; real transcripts (large markdown, KB-size
tool outputs) multiply the per-row cost — the 1–5 s "just hangs" range. The
cost also scales with how much the agent did while the tab was hidden, which
matches the "switch back and forth while an agent works" experience exactly.

Repro harness: `scripts/perf/pump-turns.mjs` drives a `fake-claude` chat
session to any transcript size in seconds, free of billing. Keep using it as
the before/after gauge for PR A/D.

## Findings — tab-switch jank (ranked)

1. **Index-keyed transcript rows + at-cap compaction ⇒ mass remount**
   (HIGH, reproduced). Rows are keyed `b-${originalIndex}`
   (`web-ui/src/lib/chat/ChatView.svelte:1323`). Past `BLOCK_CAP = 2000`,
   every appended event front-splices `blocks`
   (`web-ui/src/lib/chat/store.svelte.ts:1185-1197`), shifting every index;
   the activation `setRange` (`ChatView.svelte:320-335`) then sees ~192
   changed keys ⇒ Svelte destroys/remounts the whole window, each row paying
   marked + DOMPurify + KaTeX + `wrapWords`/`stampPaths` in one synchronous
   flush. **Bonus correctness bug**: at cap, `total === renderedTotal` makes
   the tail effect treat appends as in-place (`ChatView.svelte:339-342`), so
   new blocks silently stop rendering until the next activation.
2. **Unconditional `setRange`/`renderItems` rebuild per activation + freeze
   snapshots per hide** (MED, measured ~50–60 ms steady tax)
   (`ChatView.svelte:320-335, 179-198, 207-215, 1216-1234`).
3. **Evicted-terminal re-attach: full-scrollback snapshot rendered
   synchronously on the daemon reactor under the term lock, shipped as one
   multi-MB frame** (MED, measured 95 ms client-side @10k lines)
   (`crates/chimaera-server/src/ws.rs:105,296`,
   `crates/chimaera-pty/src/snapshot.rs:241-397`). Also stalls the PTY reader
   (`session.rs:430-438`) for the render duration.
4. **Catch-up burst proportional to hidden-time activity**; client-side
   `tool_output_delta` accumulation is unbounded
   (`store.svelte.ts:791-808`) (MED).
5. **WebGL context loss silently downgrades a pooled terminal to the DOM
   renderer forever** (`web-ui/src/lib/terminal/termPoolRuntime.ts:242`)
   (LOW-MED; when it fires, a busy terminal feels permanently sluggish).
6. **Up to 8 retained hidden view trees stay in layout** (`visibility:hidden`,
   not `content-visibility`) — global relayouts (resize, font setting, theme)
   pay for all of them (`web-ui/src/lib/layout/Pane.svelte:537-544`) (LOW-MED).

## Findings — battery / idle CPU (ranked)

1. **Hidden/parked terminals parse their full output stream** — pooled
   sockets stay open and `term.write` runs unconditionally
   (`termPoolRuntime.ts:281-283`); no visibility signal exists in the session
   WS protocol (`ws.rs:40-71`). Measured: 11–18% daemon + 10–17% renderer for
   one busy hidden terminal.
2. **`pollHealth`: 5 s HTTP fetch, forever, per window, never
   visibility-gated, redundant while `/ws/events` is up**
   (`web-ui/src/lib/net/api.ts:264-284`, `web-ui/src/App.svelte:1021-1039`).
3. **Daemon-unreachable reconnect storm**: health 5 s + sessions 5 s + events
   ≤10 s + up to 12 terminal + 8 chat reconnectors ≤10 s each — ~150
   attempts/min with no jitter, no hidden-window tier, indefinitely
   (`web-ui/src/lib/net/reconnect.ts:47-57`, `events.ts:215-221`).
4. **Streaming markdown is O(n²) per message**: every ~100 ms chunk re-parses,
   re-sanitizes (marked+DOMPurify+KaTeX), rebuilds the whole accumulated
   message, then re-runs `wrapWords` (span per word) + `stampPaths`
   (`web-ui/src/lib/chat/Markdown.svelte:271-276, 395-433`).
5. **Box-shadow pulse animations paint every frame for hours** while an agent
   runs: `agent-exec-pulse` on the whole pane border
   (`Pane.svelte:504-516`), composer `stop-breathe`
   (`Composer.svelte:909-925`); 4–6 infinite animations run concurrently
   during a turn, and none pause on `document.hidden`.
6. Minor: browser pane 1 Hz `syncLocation` (`BrowserView.svelte:330-334`);
   ComputeStrip/Banner/dashboard-chip 1 Hz tickers without the doc-visibility
   gate the rest of the app uses; `liveActiveAgents` filters all ≤2000 blocks
   per event (`ChatView.svelte:1202-1209`); visible-terminal link prefetch
   scans ~4×/s during heavy output (`web-ui/src/lib/terminal/links.ts:412-448`).
7. Daemon (all bounded, none idle-hot, worth fixing opportunistically):
   per-client `/ws/events` snapshot rebuild ≤1 Hz (4 Hz during turns)
   (`ws.rs:659-722`); `git status` per watching window with no single-flight
   (`crates/chimaera-server/src/git/http.rs:29-104`); one WS frame per ≤8 KiB
   PTY read, no coalescing (`ws.rs:202-207`).

Explicitly cleared (don't spend time here): termPool re-parenting design,
resize/fit paths (no storms — drag-suppressed, debounced, degenerate-dims
guarded), chat store granularity, hidden-chat freeze discipline, gap-only chat
replay, journal write path (no fsync amplification), fs_watch bounded stat
model, git fencing, PTY bounds/backpressure. The #105 windowing work is good;
these findings are the gaps it left, not a refutation of it.

## The plan — four bounded PRs

**PR A — kill the switch hang (chat)** *(small-medium; do first)*
- Key rows by a stable per-block monotonic id assigned in the reducer, not
  `b-${index}` (tool groups already use stable `g-${block.id}`).
- Fix the at-cap tail-advance (track a trim/compaction epoch so windowing
  advances and new blocks render — fixes the correctness bug too).
- Early-out the activation `setRange` when range and content are unchanged.
- Amortize trims with hysteresis (trim a page at a time, not per event), so
  `rebuildIndexes` stops running O(2000) per event at cap.
- Gauge: pump harness — 249 ms ⇒ target <50 ms; steady tax ⇒ ~0. Reducer
  Vitest suite extended for trim/key behavior.

**PR B — terminals: hidden-stream buffering + snapshot cost** *(medium)*
- Client: parked terminals stop feeding `term.write` per chunk — buffer
  (bounded, e.g. 512 KiB) and flush on re-show; on overflow drop + request a
  resync on re-show (server path exists: lag ⇒ resync).
- Daemon: coalesce PTY output frames (~8 ms tick or size threshold — keeps
  typing echo latency, cuts frame/syscall count).
- Daemon: render attach/resync snapshots off the reactor (`spawn_blocking`)
  and outside the term lock (internal grid copy is fine — the "never
  serialize the Term grid" rule is about the wire).
- Retry WebGL after context loss on re-show.
- Gauge: hidden-busy CPU ⇒ near-zero; `snapshot_replay_matches_live_grid`
  stays green; typing latency unchanged; evicted re-attach long task shrinks.

**PR C — idle battery sweep** *(small)*
- `pollHealth`: stretch to 60 s while `/ws/events` is healthy (the socket is
  the liveness signal); gate on document visibility with a catch-up fetch on
  `visibilitychange`.
- Reconnectors: add jitter + a slow tier while `document.hidden`; let the
  events socket's recovery trigger the per-session sockets instead of 20
  independent probes.
- Animations: box-shadow pulses ⇒ compositor-friendly opacity/transform on a
  pseudo-element; pause infinite animations under a root `document.hidden`
  class.
- Browser pane `syncLocation` ⇒ event-driven (or 5 s); visibility-gate the
  remaining 1 Hz tickers (copy the BackgroundTray idiom);
  `liveActiveAgents` ⇒ reducer-maintained set.

**PR D — streaming render cost** *(medium-large; last — most surgery)*
- Incremental streaming markdown: segment rendered HTML at top-level block
  boundaries; per chunk re-parse only the trailing incomplete block;
  `wrapWords` only the unrevealed tail; defer `stampPaths` to idle.
  (Must preserve the #122 text-block paragraph-break behavior — regression
  test that.)
- Mirror the server's tool-output truncation client-side so
  `tool_output_delta` accumulation is bounded.
- Gauge: long-reply streaming long-task profile; reveal animation unchanged.

Daemon extras (fold into PR B or a fifth small PR): compute the `/ws/events`
sessions snapshot once and fan out; single-flight `git status` per workspace;
move the settings `stat` off the reactor.

**Deferred, deliberately**: a visibility hint in the session WS protocol
(client tells the daemon "this view is hidden — downsample"). It's the *right*
long-term shape for remote/HPC links, but PR B's client-side buffering captures
most of the win with zero wire-contract risk. Revisit if remote-tunnel
bandwidth shows up as a cost later.

## Doc-drift checklist (per PR, in the same change)

- PR A/D: `web-ui/src/lib/chat/AGENTS.md` — row-key scheme, cap/trim
  semantics, streaming render pipeline.
- PR B: `crates/chimaera-pty/AGENTS.md` + `.claude/rules/pty.md` — frame
  coalescing, off-reactor snapshot render; `web-ui` terminal notes for the
  parked-buffer rule ("parked terminals buffer, not parse").
- PR C: `.claude/rules/web-ui.md` — add the invariant this audit kept finding
  violated: *recurring work (timers, polls, animations) must be gated on
  document visibility or an equivalent liveness signal*.
- Each PR: a dated entry in `docs/history/field-notes.md`.
- No `docs/features/` changes — none of this alters user-facing behavior
  (`fix:`/`perf:` prefixes, no release-notes surprises).

This file is the audit record; prune it (or fold the survivors into
field-notes) once the four PRs land.
