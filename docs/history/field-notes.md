# Chimaera — field notes & verified-component log

> A dated running log of live-verification findings and field deployments, moved
> out of DESIGN.md to keep it a lean spine. Historical record: where it conflicts
> with the current code, the code wins.

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

## Verified component notes (2026-07-07/08): child-marked claude persists no transcript — scrub launcher env

Found during live verification of session resurrection, initially misdiagnosed as a
claude 2.1.204 regression, then bisected to its true cause: **claude suppresses
interactive transcript persistence when its environment marks it as a child of another
Claude Code session** (`CLAUDE_CODE_SESSION_ID` / `CLAUDE_CODE_CHILD_SESSION`; both
bisected live against 2.1.204 — same shell, same cwd: markers present → no transcript
ever, markers removed → transcript within 2s of the first prompt). Hooks still *report*
a `transcript_path`; the file never materializes, and `claude --resume <id>` dies with
"No conversation found".

Why chimaera hits this: a daemon started **from inside a claude session** — dev loops
do this constantly ("restart the daemon" typed to an agent, agent-driven verification,
a linked terminal running `chimaera serve`) — inherits those markers and passes them to
every session it spawns, so every agent under it silently loses `--resume`, and even a
`claude` typed by hand into a chimaera shell goes transcript-silent. A daemon started
by the app or a plain terminal is unaffected (which is why normal usage never sees it).

Fixes, both kept even though the root cause is environmental:
- **Launcher-context scrub**: PTY spawn gained `env_remove`; the daemon strips the
  CLAUDE* marker family (`CLAUDECODE`, `CLAUDE_CODE_*`, `CLAUDE_AGENT_*`,
  `CLAUDE_EFFORT`, `AI_AGENT`) from every session it spawns — none of it can describe
  a chimaera session truthfully, and anything the user set in their own profile comes
  back through the login-shell wrap.
- **A resume id is a claim, not a promise.** Everything that mints one — the session
  ledger's boot resurrection, `retire()` into Recents — verifies the transcript exists
  on disk first (hook-recorded path, or the conventional store location for the cwd).
  No transcript → resurrection respawns a fresh TUI carrying the old display title,
  and Recents rows omit `resume` (the row honestly starts fresh). Defense in depth:
  transcripts also vanish via claude's own `cleanupPeriodDays`, old contaminated
  daemons, and whatever comes next.

## Decisions log addendum (2026-07-07): stateful restarts + update surface — SHIPPED

The update-safety chain, three layers, each independently useful:
- **Session ledger + resurrection** (`sessions.json`, reconciled from live state ≤2s
  behind truth, flushed on graceful shutdown): on boot, shells respawn at their last
  polled cwd and claude conversations respawn `--resume` (transcript-verified, above) —
  all **under their original session ids**, which is the single property that lets every
  persisted layout tab, linked-terminal edge, and window rebind with zero client
  migration. Non-resumable agents retire into Recents (also fixes: agents that died
  while the daemon was down used to vanish). `daemon.restoreSessions` opts out;
  opting out still retires conversations into Recents.
- **Restart handoff** (`handoff.json`, written by every graceful stop, consumed once
  within 120s): the successor daemon rebinds the same port with the same token, so ssh
  forwards stay valid and every client — app window or plain browser tab — heals with a
  plain WebSocket reconnect. No re-home, no re-auth. Crashes never leave one, so
  unplanned restarts keep fresh credentials.
- **Window registry** (`windows.json`, app shell): the open window set (host, workspace,
  logical geometry, stable per-window id) is persisted and reopened on launch; the
  stable id rides the window URL as `win=` and seeds the SPA's view-state key, so a
  reopened window IS its predecessor. Closing a window forgets it (macOS convention);
  quitting keeps the set.

Update awareness rides on top: the daemon checks GitHub a few times a day (bounded curl,
off for dev builds, `update.autoCheck` opt-out) and pushes an `update` frame on
`/ws/events`; the app shell separately watches the signed-app updater and exposes its
build id for skew detection against `/health`'s. One toast per window merges the three
signals into the single offer a click can act on there — full app+daemon chain (intent
file carries consent across the relaunch), daemon-only restart, or a "new release
exists" notice in browsers. The toast cannot over-promise resurrection by construction:
the UI is embedded in the daemon, so a daemon too old to have the ledger serves a UI too
old to have the toast. Home screen's version mark now says `daemon dev·<ref>` for dev
builds instead of posing as an ordinary `v0.0.1` (field confusion: an app reinstall
attached to a still-running dev daemon and nothing on screen said so).

## Field notes: dev binary stranded in the real home bricked release connect (2026-07-09)

The day after `connect --dev` shipped (dev-is-dev on both ends: a `0.0.1` build defaults its
state to `~/.chimaera-dev`), the RELEASE connect to the cluster broke: "daemon did not start
within 15s", with daemons piling up on the login node. Chain of causes, and the invariants
they forced:

- **The real home's binary resolution still trusted the `just dist` stash.** The release
  app's update path deployed `~/.chimaera/dist/chimaera-x86_64-linux-musl` — a `0.0.1` dev
  build — into `~/.chimaera/bin/`. Started there with no `CHIMAERA_HOME`, it relocated its
  state to `~/.chimaera-dev` and wrote its manifest THERE, while connect polled
  `~/.chimaera/manifest.json` forever. Invariant: **the real home runs release binaries
  only** — the stash feeds dev connects; `--binary` is the explicit override.
- **"Executable" was the only reuse check on a fresh start**, so once poisoned, the host
  stayed poisoned: every connect reused the stranded dev binary. Invariant: a fresh start
  **version-probes the installed binary** and replaces the `0.0.1` sentinel in the real home.
- **The daemon happily double-started over one state dir.** Each failed-connect retry ran
  another `chimaera serve`, and each one respawned the SAME ledger sessions — triplicate
  claude processes on a shared login node. Invariant: **one daemon per state dir** — `serve`
  refuses to start when the manifest's daemon is provably alive (live pid + an HTTP answer
  on its port; a crash leftover or recycled pid must not block startup).

## Field notes: laptop-sleep reconnect (2026-07-08)

The first real close-the-laptop-overnight cycle against the cluster broke reconnect in
three compounding ways. Findings and the invariants they forced:

- **A dead ssh forward keeps accepting.** After wake, the `-N -L` child's local listener
  still accepts TCP while the connection behind it is gone — so every `TcpStream::connect`
  liveness probe said "up", `connect_host` concluded "already connected" and healed
  nothing, while the window's WebSocket retried a black hole forever. Invariant: **tunnel
  liveness is an HTTP response end-to-end** (`http_alive`: any HTTP status on the loopback
  port within 2s — even a 401 proves the daemon answered). A bare TCP accept is never
  proof of anything. Corollary: chimaera ssh now carries `ServerAliveInterval=15`/
  `CountMax=3` + `ConnectTimeout=15`, so dead masters and forwards exit in ~45s instead
  of holding their listeners (and their lies) for hours.
- **Askpass prompts are state, not just an event.** Startup window restore begins
  connecting before any webview exists; a Duo prompt emitted then reached zero listeners
  and vanished — ssh waited out its 180s timeout with the host stuck "connecting" and
  nothing on screen to answer (the "blue bar, no prompts" bug). Invariant: pending
  prompts are held in the shell (`list_askpass`) and eligible windows fetch them on mount;
  the emit is just the fast path. Each prompt now carries the ssh child's host alias, and the
  shell targets events plus authorizes list/answer commands from its immutable window scope, so
  remote windows can reach only their own host while home remains the startup/first-connect
  fallback. Restore registers that home before starting remote ssh when the persisted local set
  contains only workspaces; otherwise the stricter scope would leave early password/2FA prompts
  with no eligible window. The shell stamps that fallback at window creation and does not derive it
  from later SPA workspace reports, so navigating Home into a workspace cannot hide an in-flight
  prompt. Answering in one matching window targets `ssh-askpass-done` to the same scope.
  Compute windows carry an explicit login-host askpass scope alongside their distinct per-job
  tunnel identity; parsing a `host#job…` prefix as authorization would let a colliding ordinary
  SSH alias observe the host's prompt.
- **Connects coalesce per alias.** A drop used to fan out: every window's reconnect plus
  the home screen plus startup restore each called `connect_host`, the first won and the
  rest bounced with "a connection attempt is already running" (or worse, queued more 2FA
  prompts). Now one flight owns the ssh per alias and every concurrent caller awaits its
  outcome — one Duo push per host, ever. The fresh-port retry after a reused-port failure
  is gated on a typed `TunnelPhaseError`, because re-running the whole connect on an auth
  cancel re-prompted 2FA. Every successful caller also receives a `connected` endpoint event,
  including live-tunnel reuse and flight joiners; otherwise a window with a stale token could join
  a tunnel another window had already healed and remain stuck on its old 401.
- **Remote windows come back with the first successful connect**, not just the next
  launch: reopening persisted windows rides `connect` itself (dedup'd on stable window
  id), so a host that was unreachable at launch restores its windows the moment a
  home-screen click or auto-reconnect lands.
- **Sessions snapshots wait for resurrection.** The daemon serves while the ledger is
  still respawning, and a window's first `/ws/events` snapshot taken mid-restore read as
  "those sessions died" — the client pruned their tabs out of the restored layout.
  `GET /sessions` fed the same half-truth to the remote update decision ("0 live
  sessions" → safe to replace the daemon → kills the sessions being resurrected). Both
  now gate on restore completion (bounded at 15s so a wedged respawn can't blank the UI).

## Field notes: first cluster deployment (2026-07-06)

M0 `connect` validated end-to-end against a production HPC cluster (CentOS 7.9 login
nodes, Duo 2FA, ControlMaster-only non-interactive ssh). Findings:

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
  one node (the ssh config pointed at a round-robin login alias, the master landed on one
  specific node, and every subsequent command hit that same node). Round-robin manifest
  discovery matters only *across* master restarts — less scary than the design feared, but
  still needed.
- **`claude` is not in the non-interactive ssh PATH** on the login node even for a user who
  runs it daily — M2's session spawning must resolve the agent binary via a login shell or
  explicit config, never PATH assumptions.
- The cluster's login nodes run Duo + `gssapi-with-mic,password` only (no pubkeys): riding the
  user's ControlMaster isn't just convenient, it's the *only* non-interactive path — the
  design's shell-out-to-system-ssh decision is load-bearing here.
- **On containers as a fallback:** Docker never exists on HPC (no root); Apptainer/Singularity
  does, but adds per-site bind-mount/startup variance. The static binary already solves the
  problem class containers address (old glibc, missing deps) — and the bugs we actually hit
  were ssh/shell semantics that would reproduce identically inside a container. Keep an
  Apptainer recipe as a documented fallback for pathological hosts, not as the plan.

## PR B — parked-terminal buffering + PTY frame coalescing + off-reactor snapshots (2026-08-31)

Follow-up to the live performance audit: one busy *hidden* terminal measured 11-18% daemon CPU + 10-17% renderer CPU, continuously, because the client pool parked detached xterms with sockets open and parsed every byte anyway — × up to POOL_CAP=12. Landed in two passes: the initial change, then a deep (5-finder) review round that hardened the interleavings.

- **Parked terminals buffer, don't parse** (`termPoolRuntime.ts` + the `parkedBuffer.ts` state machine). Parking is an explicit lifecycle flag (release() parks, adopt unparks) — never derived from DOM topology, which pane drag-out flows can rearrange without a release. While parked, output queues in a bounded buffer (512 KiB) and replays in order on adopt, ahead of live writes. The stream is *discarded* (desynced latch) on buffer overflow, on a foreign resize (old-width bytes are unreplayable in the reflowed grid), or on an exit that raced a snapshot: adopt then forces a clean socket re-attach — and the latch clears only when the resync was actually issued, so a fatal socket can't burn the recovery flag. A reset that arrives while parked applies resize-before-reset immediately (cheap) and the adjacent snapshot frame **writes through** — one bounded hidden parse beats discarding a >512 KiB snapshot and paying a second server render on adopt. An exit flushes the buffered tail (the last words) the same way. So two hidden-parse paths remain by design, and deferred client-parse side effects (OSC 52 clipboard writes) *usually* — not always — wait for adopt. Verified nothing else depends on parked parsing: titles, cwd, and busy/idle are daemon-derived (`EventProxy` OSC titles, the cwd poller, `last_output_at`→`output_active`); the link prefetch hooks `term.onRender`, inert while hidden. The interleavings are pinned in `parkedBuffer.test.ts`.
- **PTY output coalescing** (`ws.rs`). The bridge previously sent one binary WS frame per PTY read (≤8 KiB) — dozens of frames/sec per client under a repainting TUI. Now: leading edge (the first chunk after an idle gap sends immediately, so typing echo never waits), then an 8 ms window / 32 KiB ceiling per frame, with an opportunistic `try_recv` drain so already-queued chunks batch for free. Single-chunk frames still ride the refcounted broadcast `Bytes` (zero-copy per client). Lagged discards the pending batch and repaints; a Closed flushes the batched tail first — those are the session's last words. **The ordering rule that fell out of review:** the batch is flushed before *any* event JSON frame and before applying a client resize — an `exited` or `resized` frame must never overtake the bytes it postdates (the resize initiator is excluded from resync, so an inverted flush there would never self-repair). A failed re-attach mid-resync now closes the connection so the client's reconnect self-heals, instead of silently streaming onto a stale grid. Frame *boundaries* changed; frame semantics and the auth/ready/snapshot handshake did not — and one adjacency became an explicit contract: a reset-bearing frame (`ready`/`resync`) is always immediately followed by the complete snapshot binary.
- **Term-lock work off the reactor** (`ws.rs`). Attach/resync renders (full scrollback under the term std::Mutex — ~95 ms measured at the 10k default, cap 200k) *and* resizes (grid reflow under the same lock) run under `spawn_blocking`. The render still briefly stalls that session's PTY reader thread (term lock) — accepted; rendering from a grid copy was considered and deferred. Remaining known reactor-side lock taker: the MCP screen-text scrape. Invariant recorded in `.claude/rules/pty.md`.
- **WebGL retry on adopt** (`termPoolRuntime.ts`). A context loss used to downgrade a pooled terminal to the DOM renderer permanently; adopt now retries the addon, bounded at 2 losses before latching DOM for good (unbounded retries across a 12-entry pool can ping-pong context eviction). Construction failure stays permanent — WebGL is genuinely unavailable.
- **Graceful death of forgotten sessions** (`ws.ts`). A session that exited while parked and whose last words were evicted (2 MiB server-side bound) used to burn 12 unknown-session retries and end in a fatal error surface; after a witnessed exit, "unknown session" now resolves terminal-gracefully — keep the grid + `[exited]` marker, stop reconnecting. The reconnect `ready` frame's cols/rows are now adopted before the replay (they're the render dims — for last-words, the death-time grid).
- **Known limitation:** a client with `terminal.scrollback` set above the daemon's server-side scrollback loses the excess client-held history on any resync (the snapshot only carries the server's lines). Pre-existing on reconnect; the overflow-adopt path makes it reachable more often.
- **Deferred design notes:** coalescing at the broadcast *source* (one timer per session instead of per client) was considered and deferred — per-client windows keep slow clients from shaping fast ones. A wire-level hidden-mute (client tells the server "I'm parked, stop sending") would cut network too but needs an unmute-resync protocol — deferred. Disposing the WebGL addon on park (freeing GPU contexts for visible terminals) is a plausible follow-up — deferred pending context-loss telemetry.

## PR A — stable chat row keys + trim-aware windowing (2026-08-31)

- **The tab-switch hang:** transcript rows were keyed `b-${arrayIndex}`. Once `blocks` hit the 2000 cap, every append front-spliced the array, shifting all indices — the next activation saw ~192 changed keys and remounted the whole DOM window (marked+DOMPurify+KaTeX per row, 249ms measured). Rows now carry a monotonic per-store `uid` and are keyed by it.
- **New blocks silently stopped rendering at cap:** with length pinned at 2000, appends looked like in-place chunk growth and the tail window never advanced. The store now exposes `trimmedCount`; windowing compares the virtual total (`length + trimmedCount`), and view ranges/pool cursors shift with trims (cursors persist in virtual coordinates).
- **Per-switch tax:** re-activating an unchanged hidden tab now early-outs of the range rebuild; trims run with a 64-block hysteresis slack so the O(2000) index rebuild costs 1/64th as often at cap.
- **Review hardening (same day):** "did the row set change" now keys on an explicit `structuralVersion` (net lengths — even the virtual total — cancel out under a retract+re-append); a store `epoch` gates trim-delta shifts and saved cursors across journal resets; trim shifts drop the same rows from the rendered slice (tail fallback when the whole window is gone); scroll anchors resolve by block uid, not stale DOM index labels. Known accepted edge: a ToolGroup keyed by its first tool's id remounts if that exact row is trimmed away (only reachable parked ~2000 blocks behind the tail).

## PR E — daemon shared-work extras (2026-08-31, review-hardened 2026-09-01)

The audit's "bounded but duplicated" daemon costs (perf-plan item 7 / the "daemon extras" list), plus one harness bug. None were idle-hot; all scaled per connected window. Zero wire-shape changes — every frame's bytes are exactly what each client built for itself before. Landed in two passes: the initial change, then an xhigh review round that found both shares had real semantic gaps (below, marked *review*).

- **Shared `/ws/events` sessions snapshot** (`session_view::SnapshotCache` + `state::ChangeBus`). Each connected events client woke on the same change notify (up to 4 Hz during chat turns — every coalesced chat event fires `notify_waiters`) and rebuilt + serialized the full sessions snapshot itself: N windows, N identical builds. Now `state.changes` is a `ChangeBus` — the same `Notify`, plus a generation counter bumped on every wake (same method names, so all ~40 call sites compiled unchanged; bump-before-notify is a pinned test) — and the serialized frame is cached once per generation and fanned out as an `Arc<String>`. *Review:* the first draft held a `std::sync` mutex across the build, so waiting clients PARKED reactor workers — with clients ≥ workers one change could stall every PTY pump for the build duration. Builds now serialize on an async mutex (waiters yield; the build itself stays sync), and the store is generation-keyed so a straggler holding an old generation can't clobber a newer entry. The generation is read BEFORE the build (a change landing mid-build stamps a newer generation → next reader rebuilds; a spare build, never staleness), and the cache expires on `ws::EVENTS_THROTTLE` within a generation — `EVENTS_SNAPSHOT_REUSE` is defined AS that constant — because `stalled`/`output_active` are time-derived; their worst-case flip latency is tick + throttle + reuse ≈ 1.5s (vs ≈1.25s pre-cache). Per-client state (fs_watch, git-epoch/settings/update/recents sends, the last-sent compare) stays per-client; the compare got an `Arc::ptr_eq` fast path.
- **Single-flight `git status`** (`git/service.rs::StatusShare`). An epoch bump makes every watching window refetch `GET /git/status` at once — up to 4 concurrent identical porcelain-v2 runs on a big repo. *Review:* the first draft stamped freshness at run start and TTL-checked it at read, which self-defeated exactly on the motivating case — a run slower than the 1s window arrived pre-expired, every queued follower became a new serial leader, and N windows paid ~N×T wall (worse than no share). The join is now real single-flight: a caller that was already WAITING when a run completes takes that run's outcome unconditionally — success or failure (a wedged repo costs one 8s timeout, not one per window) — unless a flush invalidated it; the ~1s reuse window applies only to callers arriving after completion and never resurrects an error. Event-driven invalidations (`GitService::invalidate` — `mark_path_dirty` after saves/agent writes, and the worktree add/remove handlers, which previously bumped without flushing) drop the cached outcome (payload included — nothing stays pinned) and mark an in-flight run so its result is served but neither shared nor **published**: re-seeding a mid-flush run's pre-change data as the epoch baseline would have forced a second bump + second full fan-out per save. Slots are evicted on workspace delete (`forget_workspace`, also wired into the worktree-removal unregister loop). The HTTP handler and backstop ride the share; the MCP `git_facts` path uses `status_fresh` instead — serialized on the same slot lock but ALWAYS running, because an agent that just ran `git commit` in a terminal (no invalidation fires) must not be answered with the pre-commit dirty list. That same out-of-band case means the HTTP path accepts ≤1s staleness — deliberate. The raw runner is named `status_uncached` with a warning comment so future callers can't silently bypass the share. Semaphore, 8s timeout, and output caps untouched underneath.
- **Settings stat off the reactor** (`settings.rs::watch_external_edits`). `send_settings_snapshot` re-statted settings.json on the reactor under the settings mutex on every events wake, per client — the hot-path hazard on NFS. The events path now reads only the cached generation/map (`generation_cached`/`map_cached`, no stat); hand-edit detection moved to one daemon-wide 2s poll (stat + conditional reload in one `spawn_blocking` hop) that fires `notify_waiters` on a real content change. *Review:* a FAILED stat (NFS hiccup, distinct from the file being absent) is treated as no-change — reloading through a failing filesystem broadcast an empty map and flashed every window's theme back to defaults; and a new window's first settings frame does one fresh off-reactor read so a hand-edit inside the poll window can't greet it stale. Scope note, deliberately accepted: `current()` (GET /settings, daemon-key getters, `git::configured_git`) still re-stats inline and PUT still writes, on the reactor — rare, user-driven paths; the reactor-fs exposure is narrowed to those, not eliminated.
- **Harness fix** (`scripts/perf/pump-turns.mjs`). The pump deduped permission answers by `request_id` — but fake-claude reuses `req-1` for *every* turn's ask, so the pump answered turn 1 and stalled forever. The dedupe set is gone; the `e.seq > replayHead` guard alone prevents stale replay re-answers (a comment now warns the next person off reintroducing it). Known limitation, now in the header: a live ask parked from before the pump connected sits at seq ≤ the replay head and is never answered — use a fresh session.

## PR C — idle battery sweep (2026-08-31, hardened 2026-09-01)

The perf audit (docs/perf-plan.md) found idle/background cost everywhere the same shape: recurring work with no visibility gate. Landed in one sweep, then a deep review round hardened the recovery machinery:

- **Health poll**: `/health` every 5s per window forever (~17k req/day) → 60s safety net while `/ws/events` is up (the socket IS the liveness signal), 5s recovery probe only in a visible window, slow tier hidden, catch-up fetch on `visibilitychange` (`net/poll.ts`; the sessions fallback poll rides the same tiers). The fetch carries a 4s abort so a hanging TCP connect (dead tunnel) can't stretch the cadence; an events up/down transition kicks one prompt probe, damped so a crash-looping daemon can't turn flaps into a fetch per second.
- **Reconnect storm**: an unreachable daemon (resumed laptop, dead tunnel) ran ~150 conn attempts/min indefinitely. The shared `Reconnector` + events socket now jitter every delay ±20% and floor it at the hidden slow tier (~60s nominal, 48–72s with jitter) from the third consecutive failure — the first two retries keep the fast backoff even hidden, so a transient blip recovers in seconds. Recovery nudges are deferred out of the events message handler, spread over 0–2s per socket, and damped to one herd per 10s; a nudge landing while an attempt is in flight marks the socket to retry immediately on that attempt's failure. A successful health probe while events is down cross-nudges the events socket (and revives a fatal one exactly once), so the two safety nets cooperate instead of running independent 60s clocks.
- **Pulse animations**: the box-shadow keyframes (pane `agent-exec-pulse`, composer `stop-breathe`, tab `chip-pulse`) repainted every frame for whole agent turns — now static glows breathed via opacity/transform (composited); the pane ring is an unclipped overlay ring outside the border, so it occludes no content or scrollbars. The enumerated infinite "presence" animations pause under `html.app-hidden` (set by App while the document is hidden): the three pulses above plus SessionGlyph, BrandMark, WorkTray sparks, WorkTrayRow dots, the daemon dot, AgentCard's card pulses, ChatView's thinking shimmer trio, ToolCallCard's cursor + running dot, and HomeScreen's presence dots. `WorkTrayRow` dots also gained the `visible` gate their tray siblings had, threaded Dashboard→AgentCard→rows.
- **1 Hz tickers** (ComputeStrip/ComputeBanner/Dashboard compute chip/AgentCard work rows) and the browser pane 1s `syncLocation` poll (now 5s, load-event-driven for real navigations) gate on pane visibility && `$pageVisible` with catch-up on return; the browser pane's unreachable-retry probe gates the same way, while its 60s proxy keep-alive deliberately does NOT (a hidden window must keep its proxy session alive).
- **Accepted tradeoff (zombie events socket)**: a wedged-but-open `/ws/events` socket reads as `eventsUp`, so in a visible window a dead daemon behind it is only noticed by the 60s health safety net. Kept at 60s rather than 30s: the per-session sockets fail loudly within seconds of any real use, every send returns false (surfaced), and halving the tier doubles the steady-state tax on every healthy window to shave worst-case detection on a rare failure mode.

The invariant is now a web-ui rule (.claude/rules/web-ui.md): recurring work must be gated on document visibility or an equivalent liveness signal.

## PR D — incremental streaming markdown (2026-08-31)

Streaming prose was O(n²) per message: every coalesced chunk (2 KiB / 100 ms) re-ran marked + DOMPurify + KaTeX over the ENTIRE accumulated text, rebuilt the `{@html}` subtree, then re-ran copy decoration, `wrapWords` (a span per word over the whole message), and `stampPaths` (TreeWalker + path regex per word) — all synchronous. A multi-thousand-word reply burned tens of ms per chunk near its end.

- **Segment the source, cache the prefix** (`streamSegments.ts`, pure + tested). The stream splits at safe top-level blank-line boundaries; "safe" is conservative by rule — open fences and `$$`/`\[` block math never split, a segment ending in a list item / indented line / HTML-ish line refuses to close (those constructs continue across blank lines), and a reference-link definition anywhere bails segmentation for the whole message (its effect is document-global — per-segment parsing would break references silently). The partition is lossless: closed segments + open tail concatenate back to the source byte-for-byte, which is also what protects PR #122's materialized `\n\n` block separators (regression-tested). A text update that doesn't extend the prior source (retraction/reroute) invalidates the cached prefix wholesale.
- **Per-chunk work is proportional to the open tail.** During streaming, `Markdown.svelte` manages its own DOM: each closed segment parses/sanitizes/decorates/word-wraps once into a `display: contents` wrapper and is never touched again; only the tail wrapper re-renders per chunk. The reveal ticker drains a prefix-then-tail queue of ONLY not-yet-revealed word spans (the shown prefix is plain text), with the cursor carried across tail rebuilds and into closing segments so nothing re-hides or double-fades. `stampPaths` runs at idle on closed segments — never on the chunk hot path. Reduced motion skips spans and ticker entirely but keeps the incremental DOM.
- **Settle is ONE canonical full re-parse — the correctness anchor.** When `streaming` flips false the template swaps to the memoized whole-message `{@html}` parse (lazily computed, so live blocks never pay it per chunk) and full decorations run. The settled transcript is byte-identical to a never-streamed render *by construction*, making any conservative-segmentation artifact cosmetic and transient. The swap also lands span-free DOM, which is what keeps selection-copy clean (previously done by dissolving spans in place). Every fragment that touches `innerHTML`/`{@html}` — per segment, per tail render, canonical — passes DOMPurify first; unsanitized fragments are never concatenated.
- **Live tool output is bounded client-side** (`store.svelte.ts`). `tool_output_delta` used to accumulate unboundedly ahead of the authoritative result; it now mirrors the server's `cap_output` (12 KiB head + 4 KiB rolling tail behind the same "[N bytes omitted]" marker, 4 KiB re-slice hysteresis). The bookkeeping lives beside the block, never parsed back out of the rendered text (agent output could forge the marker), and dies with the authoritative result / reconciliation / trim / reset. Pure per event — replay rebuilds the identical capped text.
- **`liveActiveAgents` is reducer-maintained** (audit B2). ChatView re-filtered ALL blocks through their Svelte proxies on every structural/tool event to feed the (usually empty) subagents tray. The store now maintains `activeAgents` incrementally at the tool_call/update/reconcile sites (the `backgroundTasks` level-set pattern), holding the same proxies as `blocks` so in-place patches land in the tray for free.
- **Deferred:** stamping the open tail's paths during streaming (they become clickable when the segment closes or at settle); per-segment KaTeX caching (KaTeX only re-renders inside the open tail already).

### Review hardening (2026-09-01, xhigh round — one security blocker + a correctness cluster)

The A/B gauge passed (identical 10-part stream: 504→221 ms total main-thread drift, worst stall 37→20 ms, zero long tasks), but three deep finder passes — two with empirical repros against the repo's own marked+math — found real gaps. All applied on the branch:

- **THE BLOCKER — the scanner was blind to CommonMark HTML blocks.** `<pre>\nplain\n\n![x](https://evil/beacon.png)\n\n</pre>`: the whole-doc parse keeps the interior inert raw text, but the old segmenter closed after "plain" and parsed the image line alone — a live `<img>` fires a network beacon from agent output (DOMPurify allowlists img/a; the settle heal is too late — the request already left). The streaming render must NEVER be more permissive than the settled render — now the stated invariant of `streamSegments.ts`. Any line that can open an HTML block (types 1–7, conservatively `<` + tagname/`!`/`?`/`/`) bails segmentation for the whole message; hostile-input tests pin the beacon shape, comment-hidden content, script/style/textarea/PI/declaration/CDATA/close-tag lines. **The same invariant killed the review's own suggested mitigation** for sticky-cost messages: force-closing a stuck tail past a byte cap would split fence/math/HTML interiors — content that is inert whole-doc but ACTIVE split (a fence whose closing marker arrives later is exactly the beacon case again). So the size cap applies only to reference-definition stickiness (splitting refs costs resolution, cosmetic), and it re-uses the ordinary safe boundaries; fences/math/HTML get no relief by construction (the giant-single-fence tail stays O(open) per chunk — documented, accepted).
- **Streamed relative links could navigate the workbench away**: deferring ALL of stampPaths to idle deferred the `md-local` classification the click handler relies on to swallow schemeless hrefs. Classification (a class toggle) now runs synchronously per fragment; only daemon validation stays deferred. Relatedly, the resolve callback's re-stamp used to re-walk the WHOLE live tree synchronously mid-stream — it now re-queues only its own root through the idle path.
- **Math close-check drift**: the scanner accepted `$$ ` (trailing space) as a closer; math.ts's BLOCK_DOLLAR requires the exact `\n$$\n`, so a "closed" that wasn't let a later boundary split inside still-open math — prose the reader saw collapsing into a katex-error blob at settle. Open/close now mirror BLOCK_DOLLAR exactly (and `\[…\]` closes only on a line ENDING with `\]`); marked-equivalence tests pin the scanner to math.ts so they can't drift apart again. CRLF lines are compared `\r`-stripped, the way marked's lexer normalizes — CRLF streams previously never closed a fence.
- **Reveal-cursor desync** (two finders independently): after a close consumed the carry, a string-equal new tail (duplicate paragraph) hit the tail memo and skipped the rebuild — shown words re-hid and re-faded. The cursor arithmetic now lives in a pure tested module (`revealLedger.ts`) whose order contract (a close is ALWAYS followed by a rebuild; the memo is invalidated on closes) is pinned by the duplicate-tail test.
- **Hide/thaw tax**: `streaming` was gated on `visible`, so hiding a tab mid-stream swapped to the canonical `{@html}` parse — a synchronous O(message) parse AT TAB-SWITCH-AWAY (partially undoing PR A's win), and thaw re-animated the whole message. `streaming` is now pure turn state (the row is the streaming tail, compared by block uid — trim-proof, and a frozen hidden row keeps matching by identity), `visible` rides separately: hidden live rows freeze their segment DOM in place (no parse, no ticker; ledger/queues/segState intact) and thaw catches up incrementally with the reveal cursor preserved. The canonical swap happens only when the row genuinely stops streaming.
- **Mid-stream selection-copy poison**: closed-segment DOM now survives across chunks, so selecting during a stream became meaningful — and partially-revealed closed segments kept word-per-span DOM (hard newline at every wrap point on copy) until settle. A drained segment now dissolves its spans just after its last fade (the old `unwrapWords`, per segment, timer torn down with the stream). Strictly better than the old pipeline mid-stream (which nuked selections every 100 ms); the settle swap still drops a selection held across that one moment — accepted, equal to before.
- **No silent segment drops**: a parser throw inside a fragment now falls back to inert `textContent` (plain text can't be more permissive than settle) instead of leaving a consumed-but-unrendered segment until settle.
- **Tool-output cap in real bytes**: thresholds counted UTF-16 code units while claiming byte parity with the server. Budgets are now UTF-8 bytes (one TextEncoder pass per delta, incremental; slices land on code-point boundaries — no split surrogates), and re-capping text that already carries the server's elision marker ABSORBS it (counts merge) so markers can never nest.
- **Scan cost is delta-proportional**: the segmenter persists its cursor + construct state (fence/math/list/blank-run/refDef) and walks only newly arrived lines; a chunking-invariance test feeds the same stream at 1/3/7/64-char steps and demands the identical split. The lazy-continuation hole (a flush plain line under a list item let the segment close, rendering the item's continuation as a top-level paragraph until settle) is closed by tracking list state instead of judging only the last line; escaped-bracket reference definitions (`[foo\]bar]: url`) now match the sticky rule; and the honest limitation is now stated where it lives: a reference USED in a segment that closed before its definition streamed renders literally until settle.
- **Platform note**: WKWebView (the native app) has no `requestIdleCallback` — deferred path stamping there rides a short fixed setTimeout instead.

Verified clean by the finders, no action: idle-stamp teardown, turn-end/abort settle paths, split-pane instance isolation, activeAgents lifecycle balance, memory release on unmount/rebuild, no effect loops, reduced-motion follow via the ResizeObserver, the decorateCopyTargets contract.

### Remote-performance batch (2026-09-01, measured on Sherlock, R1–R5 in one pass)

The perf series (#124–#131) fixed local CPU and it held (re-gauged: 8.3 ms frames, zero long tasks under flood) — but the *remote* feel is RTT- and bandwidth-bound, which it never touched. Measured against a real login node through the app's own ControlMaster: typing echo = exactly 1×RTT (165 ms on a 186 ms-ping link; the daemon path adds ~nothing), a cold HTTP fetch ≈ 2×RTT (~335 ms — the ssh mux channel-open costs a full RTT before the request starts), four busy **parked** terminals shipped ~6 MB/s of invisible tunnel traffic, and every window duplicated the whole stream (2 windows = 140 MB/15 s of nothing anyone saw). Plan + numbers: `docs/perf-remote-plan.md`; the gauge that produced them: `scripts/perf/tunnel-gauge.mjs` (`PARK=1` exercises the new protocol).

- **Park is now a wire state, not just a client one.** `/ws/sessions/{id}` gains `park`/`unpark` client frames and `auth.parked`. Parked, the server disables the output select arm — the session's bounded broadcast ring (shared by every attachment; parking costs no extra daemon memory) becomes the catch-up buffer; events still flow; an exit drains the withheld tail in bounded slices ahead of the `exited` frame; a foreign resize defers its repaint to unpark (`parked_stale`). Unpark just re-enables the arm: the ring replays contiguously, and an overflow surfaces as the *existing* `Lagged`→repaint path. Gauged: parked+flooding sockets went 27.5 MB → **0 bytes**; unpark caught up with one 813 KB resync.
- **`auth.parked` attaches without a snapshot** (`attach_quiet`: subscribe-only, no ~95 ms render under the term lock). The client desyncs its ParkedBuffer on a parked ready — no snapshot is coming, and pre-drop bytes predate an output gap — so the first adopt resyncs into ONE fresh visible attach. A wake-from-sleep reconnect of 12 parked terminals now ships ~0 snapshots instead of 12. Compat is graceful both ways (old servers ignore unknown frames — verified live against v0.40.8 on Sherlock; old clients never send park).
- **Predictive local echo** (`terminal/localEcho.ts`): nothing server-side beats 1×RTT, so ghost predicted keystrokes as a translucent DOM overlay — never buffer writes, so a wrong prediction needs no rollback (worst case is cosmetic). Armed narrowly: remote window + measured RTT ≥ 45 ms + OSC 133 prompt phase (agent TUIs stay "unknown" and never arm) + primary screen + viewport at bottom + printable ASCII; any control byte/escape/paste-torrent clears every ghost (over-clearing is always safe — truth is already painted). Verified against real Sherlock RTT: ghost painted in the same frame, reconciled away when the echo landed ~340 ms later. The verification also proved ghosts ride REAL input — the test's "echo" landed in the shell line buffer.
- **Link RTT is measured and worn**: `net/rtt.ts` keeps the rolling minimum of /health fetch timings; a remote window's host chip shows e.g. "338ms" once past 30 ms (the first cold sample IS the honest 2×RTT fetch cost). The same estimate gates the local-echo arming. Waterfall spot-check (R4b): file open / git / quickopen are already single-flight, ≤1 round trip — invalidate-and-pull holds.
- **`Compression=yes` on chimaera's own masters** (user ssh config untouched). Honest caveat: a clean A/B was impossible in-session — compression is negotiated at master creation and a fresh connection needs interactive Duo — so this rides on the traffic shape (terminal text/snapshots compress enormously; zlib CPU measured negligible at these rates). One line to revert if a fast-LAN flow ever regresses.
- **Harness footguns worth remembering**: `pkill -f`/`pgrep -f` self-match the invoking ssh wrapper's command line (bracket-trick the pattern — this "killed" two earlier daemon-start attempts and faked a running daemon); zsh does not word-split unquoted vars; and `-o Compression`/`-o` options on a mux *client* are silently ignored — only the master's negotiation counts.

### Review hardening (2026-09-01, xhigh round on the remote-perf batch — 15 findings, 13 applied)

Ten finder passes + tokio-source verification against the park/unpark batch. The blocker cluster: the exited-while-parked ring drain was wrong three independent ways — `while let Ok(..) = try_recv()` terminates on `Err(Lagged)` BEFORE yielding anything (tokio repositions the cursor and returns the error first; an empirically-probed lagged receiver drained **zero** of 4096 retained chunks), the `Exited` broadcast races the reader thread's final bytes (the 60 ms sleep in session.rs sits AFTER the event send, and the two broadcast channels have no cross-ordering), and the drain was unbounded (up to the 32 MiB ring) against the client's 512 KiB ParkedBuffer cap — guaranteed discard. Fix was a redesign, not a patch: the drain is gone; a parked exit latches the client buffer desynced and adopt resyncs into the last-words replay (the authoritative final screen, correct for old and new servers alike). Companion server fixes: the last-words branch now skips the snapshot for parked auths (it was shipping a full render the client provably dropped), `snapshot_sent`+`parked_stale` collapsed into one `unpark_repaint` flag, and unpark repaints past `UNPARK_REPLAY_MAX_CHUNKS = 64` instead of replaying an arbitrarily long ring (`Receiver::len()` as the lag detector — it counts sent-minus-received, deliberately overcounting toward the safe direction). Two server-side protocol tests now pin the handshake (parked attach withholds until the unpark repaint; small backlogs resume from the ring).

Client/echo hardening: ghost styling moved off `cssText` (the free-text `terminal.fontFamily` setting could inject arbitrary declarations — individual property assignment + a `.term-ghost` app.css rule on theme tokens closes it and makes theme flips repaint live ghosts); the re-anchor moved from rAF to `term.write`'s completion callback (xterm parses asynchronously — a rAF can fire mid-parse and redraw ghosts over already-echoed text); `echo.clear()` wired into onResized/onExited/applySettingsToPool; a 1.2 s post-control cooldown covers the window where the OSC 133 phase gate is provably stale (the sudo-password case — the phase signal lags ≥1 RTT + the events cadence); the arming gate gained `eventsUp`, the `exec_stage` half via `isBusy`, a socket-open check (never ghost input a closed socket dropped), and an O(1) `$derived` ready-set instead of an O(n) proxy walk per keystroke; desync now latches at DROP time (`onDrop`), not a ready-frame RTT later, killing the adopt-before-ready race; unpark is only sent when the entry was actually parked (show() re-runs adopt on visible entries for font changes). `resetLinkRtt` got wired (events-socket recovery = possibly a new link), the badge threshold became `LINK_RTT_BADGE_MS` beside the estimator, and the detached-window strip footer wears the RTT chip too.

Honesty corrections: the gauge's "under-flood" RTT had been measured on an idle link — the exec route responds only on command completion and the script awaited the floods (fire-and-forget now; the plan doc retracts the "no HOL degradation" claim as unverified). `PARK=1` now also auths parked (R2 finally has a harness), tunnel-count too. The "parking costs no extra daemon memory" comment was wrong and now states the real bound (the ring's rolling window, the same retention any slow attachment imposes). Docs re-synced: features/terminals.md (park is a wire state), architecture.md (invariant 4 is visible-auth-only now), remote-connect.md (Compression + the ControlPersist caveat: pre-existing masters ignore the option for their whole life). Deliberately skipped: retitling `perf:`→`feat:` (the ghost echo + RTT chip are arguably new capability — flagged, maintainer's call), a time-based RTT sampling window, and folding the two tunnel scripts.

### Daemon hot paths on a real login node (2026-09-01, PR F)

Read Martin's live scg daemon non-invasively through the app's own ControlMaster (`ssh -o ControlPath=~/.chimaera/cm/<hash>`): 159 MB RSS (180 MB high-water) with six sessions, 10 % CPU averaged over nine hours, 87 threads on a 64-core node, and 389 `quickopen index hit the time guard` warnings in one day in bursts of five identical lines per millisecond. The bursts were the terminal/chat link validator: every repaint validates every path-like token on screen, a bare basename that misses falls back to the workspace index, and the index walker was (a) inline on the reactor for the GET handler, (b) not single-flighted — "two racing queries may both walk, which is fine at this scale" was five concurrent 3 s NFS crawls per burst — and (c) trusted for only 5 s, so a 3 s walk was stale two seconds after it finished. `find` on the same tree takes 0.85 s warm; the daemon's walks hit the 3 s guard under contention with each other. Fix: stale-while-revalidate (queries never wait once an index exists), a per-workspace walk lock, freshness = 10× walk cost (5–120 s), idle eviction, and `spawn_blocking` for the handler. Gauged locally on a 144k-file synthetic tree: a five-request cold burst went from five walks (five warn lines) to one; a query landing after the freshness window went from paying the re-walk (352 ms) to 64 ms.

Companions in the same batch, each a measured or rule-violating cost: the tokio runtime was default-sized (one worker per core — 64 workers + up to 512 blocking threads on a login node) and is now 4 workers / 128 blocking on both daemon entry points (the app's `--daemon` had its own default-shaped runtime; 10 threads total locally, was ~20+); the PTY output ring dropped from 4096 to 2048 chunks (32 → 16 MiB per session worst case; review stopped it at half rather than a quarter: a visible attachment on a slow link that trips `Lagged` pays an uncapped scrollback repaint down that same link, and chatty small-chunk producers fill slots far faster than bytes — coalescing lag repaints is the follow-up that would let the ring shrink further); the transcript tail reader that fed a resumed claude session's entire multi-MB JSONL through RSS twice now streams the delta through a fixed buffer on one blocking hop, keeps only the title records it acts on (a shared predicate beside `apply_title_line`, so the launcher's resume scan can't drift from it), and truncates a single line to a 256 KiB prefix — retained state is bounded by one line, never by the delta; the marks capture pushed bytes one at a time on the PTY reader thread and now moves slices; `view-state.json` grew a key per tab forever (60 KB locally, 24 KB on scg) and is capped at 128 keys by write recency with the write on the blocking pool; the shell-name `/proc` probe and MCP screen renders left the reactor. Not changed, by choice: the 10k-line scrollback grid (~29 MB per session at 120 cols, the dominant steady-state cost — a product setting, not a leak) and the uncapped attach snapshot (full-fidelity scrollback on re-attach is the "close the laptop, nothing dies" promise).

### Remote connect round trips + UI state audit (2026-09-01/02, PRs G and H of the same round)

Two companion batches, driven by the same question ("connections smooth, state not cluttered"). **Connect (chimaera-remote + the app shell):** on the measured link every separate `ssh host cmd` through the ControlMaster cost ~0.55 s (channel open + remote fork on a loaded node), and the connect path was built out of them — a two-exec probe, a daemon start that slept a whole second and then polled with two execs per iteration (up to 15), a stop that polled with one exec per half-second (up to 20). Folded each into ONE remote POSIX-sh exec (the scripts are pure builders, unit-tested under the host's real `sh` and `dash` against real pids and manifests). Measured against Sherlock's isolated dev home through the app's own live master, update path (stop + redeploy + start): the ssh-exec phases went 6.35 s → 3.83 s (13.2 s → 11.2 s end to end; the 17 MB `scp` is what remains). Two shell-side companions: the health monitor probed a confirmed-down tunnel every 3 s forever (now doubles to a 30 s cap, any success restores the cadence), and a reconnect after laptop sleep dialled through a master whose TCP was dead and hung until ServerAlive killed it (~45 s) — `clear_wedged_master` now runs `-O check` → a bounded `ssh true` → `-O exit` for an alias the monitor confirmed down, so the flight dials a fresh master at once. Harness lesson: a dev build's ControlPath is `$CHIMAERA_HOME/data/cm/%C`, not the app's `~/.chimaera/cm/%C` — a bare run opens a fresh connection and burns failed password attempts on the host; a `data/cm → ../cm` symlink lets the dev CLI ride the live master with no auth.

**State (web UI):** a sweep of every status indicator found six that pile up or never clear: a dead errored chat driver counted as "needs you" in the rail pill, focus strip, window title and home badge forever (the dashboard was alive-gated, the other four were not); a chat's fatal-error banner cleared only on a portable fork, so a recovered pane kept a minutes-old red notice; `unauthorized` was a write-once latch (an in-place heal left the "needs fresh credentials" chip beside a working window, and a second 401 could never auto-reconnect); the daemon's `stalled` field was shipped on every snapshot and read by nothing; `chimaera.rail.<uuid>` localStorage keys were minted per tab forever; and an undismissible asset-transition notice hid the whole SSH reconnect surface. Review caught the one real trap in the fixes: releasing the 401 latch inside the unconditional `finishReconnect()` fired BEFORE the port/token-moved check, so a rotated token behind a blocked navigation would have cleared the latch and wasted a reconnect round — the release now lives only in the no-move branch, damped to one release per 30 s and at most two automatic rounds per episode. Verified live: rail-key pruning (22 → 16 records after a real save), the alive-gated count with a degraded-but-alive errored session. Not driven live: the in-place 401 heal (needs the native app + a remote), the stalled dot (a three-minute wedge), the stacked notice.

### "Switching tabs takes seconds" — WebKit accessibility, not the app (2026-09-02, live on Martin's scg window)

Reproduced on the real window with `sample <WebContent pid>` (1 ms sampling of the renderer's main thread) while the daemon, the ssh master and the app's main process all sat at ~0 %. Two distinct accessibility costs showed up, and neither is our code path: (1) an external client crawling the page's accessibility tree — `AXUIElementCopyMultipleAttributeValues` → `NSAccessibilityChildren` → WebKit walking every unignored node (`nextSiblingIncludingIgnored`, ~1 s per crawl on a workspace whose Codex transcript alone is ~6,900 DOM nodes); Goldfish captures windows this way, and quitting it removed the crawls; (2) the one that made tab switches take seconds: with Goldfish already quit, 58 % of the main thread over 20 s was `FrameSelection::updateAppearanceAfterUpdatingRendering` → `AXObjectCache::postTextStateChangeNotification` → `characterOffsetFromVisiblePosition`, which computes the caret's character offset by walking `Position::next` from the start of the selection's node — proportional to that node's text, on the JS thread, once per selection set. WebKit only does this after an assistive client has connected to the process (`accessibilityEnabled` is one-way for the process's life), so quitting the client does nothing; a fresh window process (close + reopen) cleared it completely: 7,966 samples, zero accessibility calls, 22 samples of page work. Not a leak: the ten-hour renderer sat at 276 MB with a large transcript and up to twelve warm terminals, and the client caches (link validation, chat indexes, file cache) are capped. Our share of the bug: some view keeps the DOM selection inside a very large text node and re-sets it on every update; deterministic repro plan — isolated dev app, enable accessibility with one System Events query, pump a long transcript, sample per tab switch — then fix that view. Harness lesson: screen-control tooling is itself an accessibility client; sample the window hands-off.

### The tab-switch stall, reproduced and fixed without screen control (2026-09-02, Safari harness)

The accessibility finding above was one layer; the switch itself was slow with no assistive client at all. Martin's window sampled a 2.9 s `Position::next` loop under `WebPage::editorState → Editor::stringForCandidateRequest → charactersAroundPosition` on rendering commits, and the dev-app screen grant was not available, so the reproduction ran in Safari — the same system WebKit as the WKWebView — driven by a temporary in-app harness behind a hash flag (`scripts/perf/tab-switch/`): two shell terminals plus three parked rendered documents of ~235k inline elements, 24 switches, the engine's own caret walk (`Selection.modify`) timed from the active layer, a continuous `sample` of the renderer, and isolation probes for each suspect. Three causes, each confirmed by a probe or a sample and each fixed at its own altitude:

1. **Parked layers hid with `visibility:hidden`.** It inherits: a switch re-resolved the style of every node in both layers, and WebKit then rebuilt their inline layout, re-shaping every text run (`InlineItemsBuilder::build → CTFontShapeGlyphs`, ~0.9 s of every 1.5 s switch). `opacity` is not inherited and compositor-only; the active layer stacks on top with `z-index`; `inert` still covers focus, hit-testing, and AX. `content-visibility:hidden` was measured and rejected — the reveal relayout (519 vs 219 ms) and the activate-class flip (62 vs 1 ms) both cost more than it saved. Layers are also one per tab, not per mounted view: a switch must never insert a sibling, because WebKit invalidates the following siblings' whole subtrees for positional selectors (58 ms per parked document in the probe).
2. **xterm 6 keeps a `<style>` inside the terminal element** (its scrollbar theme sheet, in `.xterm-screen`). Re-parenting a terminal between a pane and the warm stash removed and re-inserted a stylesheet, and WebKit answers that with `Style::Scope::createDocumentResolver` — a resolver rebuild plus a re-resolve of every element in the document — twice per switch (the probe's stylesheet-insert reference: 727 ms). `termPoolRuntime` hoists those nodes into `<head>` at open and before every move; xterm keeps its reference, so theme updates still land.
3. **The QuickType candidate walk.** On every rendering commit with a caret in an editable, macOS WebKit walks from the caret in both directions to the nearest visible *selectable* position. Everything inside a terminal is `user-select:none` and every parked layer is inert (unselectable), so the walk crossed every parked node one at a time. A text control's caret is confined by its shadow root (the harness measured 0 ms from the xterm textarea), so the live 2.9 s walk came from a light-DOM editable — a CodeMirror caret. A clipped, zero-width, selectable `.sel-stop` before and after every layer ends the walk at the layer edge: the engine's forward walk from the active layer went 720–855 ms → 0–1 ms.

Before/after on the same harness: 1.3–2.0 s per switch → 23–37 ms (the no-documents control: 30–50 ms); scroll position and editor state intact. Two harness lessons: `sample` only when the switch is actually running (a `top`-triggered sampler kept missing a 1.3 s window, a continuous 45 s sample did not), and isolate suspects with in-page probes (`inert`, focus flips, class toggles, a body custom property, a stylesheet insert) before theorising — three plausible theories (inherited `inert`, `:focus-within` invalidation, sibling insertion) each probed at 0–2 ms.
