# Remote performance: measured findings + fix plan

Dated 2026-09-01. Companion to the (retired) local audit `docs/perf-plan.md`
(PRs #124–#131): that pass fixed local CPU and it held — this pass measured the
**remote** path, which those PRs never touched, against a real HPC login node.

## Topology measured

- Daemon: release v0.40.8 (includes the full #125–#130 perf series), run as an
  isolated instance (`CHIMAERA_HOME` under `~/chimaera-perftest`) on
  `sh04-ln03.sherlock.stanford.edu`.
- Tunnel: `ssh -L` forward delegated to the **same live ControlMaster the app
  uses** (`~/.chimaera/cm/…`) — the production topology, one TCP connection
  multiplexing every WS + HTTP request.
- Client: Node 22 native WebSocket harness simulating app windows exactly
  (12 session sockets per window = `POOL_CAP`, one "visible", others "parked"
  — the pool keeps parked sockets open and receiving).
- Link at measurement time: ~172–186 ms ping RTT (client far from the host).
  A fast link — bandwidth was never the bottleneck *in this measurement*.

## Findings

**F1 — typing echo is exactly 1×RTT; the daemon adds ~nothing.** WS keystroke
echo p50 165 ms / p95 249 ms vs ping avg 186 ms. The daemon+PTY+coalescing
path (leading-edge send) sits at the physical floor. "SSH terminals feel slow"
on a distant host is RTT physics — no server-side fix can lower it.

**F2 — a cold HTTP request costs ~2×RTT (~335 ms measured).** TCP to the local
forward is free, but the ssh mux **channel open costs one full RTT** before the
request even starts. Every UI fetch on a fresh connection pays ~⅓ s; a burst of
parallel fetches opens parallel connections, each paying channel setup.

**F3 — parked busy terminals ship their full stream over the tunnel.** #125's
"parked terminals buffer output" is client-CPU only: parking is not in the wire
protocol (`ws.ts` sends only `auth`/`resize`/input/`resync`), so the daemon
keeps streaming to sockets nobody renders. Measured: 4 busy hidden terminals =
27.5 MB each per 15 s window ≈ **6 MB/s of invisible tunnel traffic**.

**F4 — every window duplicates the whole stream.** Two windows attached to the
same sessions: 70.7 + 69.2 = **~140 MB in 15 s (~75 Mbit/s)** for the same 4
busy terminals, 0 bytes of it visible. Detached pane windows (#121) are full
windows with their own pools — each one multiplies again.

**F5 — idle costs are already excellent.** Idle parked sockets received exactly
0 bytes (the snapshot dedupe works); daemon CPU ~0 %, local ssh master 2.3 %
CPU while encrypting 75 Mbit/s, RSS small. This is a **bandwidth × windows**
problem plus an **RTT** problem — not CPU anywhere.

**F6 — the local app is clean (re-verified).** 8 sessions / 2 windows, 3
terminals flooding: frame p50 8.3 ms, p99 9.4 ms, zero long tasks; daemon 0.4 %
CPU. The #124–#131 wins are real and holding.

### Why it can feel like the app "got slower" despite the perf PRs

The perf series optimized local CPU; remote feel is RTT- and bandwidth-bound,
which it never touched. Meanwhile recent features add remote traffic (more
surfaces fetching, multi-window/detached-window duplication), more agents mean
more busy terminals streaming invisibly, and on a slow uplink (hotel wifi, VPN,
LTE) F3+F4's tens of Mbit/s will saturate the tunnel — at which point
keystrokes and every UI fetch queue behind the flood and *everything* goes
clunky at once. (Saturation was not reproducible on the fast measured link;
listed as the expected failure mode, not a measured one. Post-review correction: the gauge's
original "under-flood" echo probe ran AFTER the flood — the exec route responds only on command
completion and the script awaited it — so "no HOL degradation observed" was measured on an idle
link and is retracted as unverified; the gauge now fires floods without awaiting.)

## Fix plan

Ordered by leverage. Each lands with a before/after run of the tunnel gauge
(`scripts/perf/tunnel-gauge.mjs` — the harness that produced the numbers above;
`PARK=1` exercises the R1 protocol).

**R1 — park/unpark on `/ws/sessions/{id}` (daemon + client pool). SHIPPED
(hardened in review).** The pool sends `{"type":"park"}` when it stashes an
entry and `{"type":"unpark"}` on adopt. Parked, the server disables the
output select arm — the session's bounded broadcast ring becomes the catch-up
buffer (shared by every attachment; a non-consuming receiver does pin the
ring's rolling window of recent chunks, the same bounded retention any slow
attachment already imposes). Events still flow; an exit while parked sends
`exited` WITHOUT draining the ring (the exit broadcast races the PTY
reader's final bytes, and a lagged ring can't produce a coherent stream —
the client latches desynced instead and its adopt resyncs into the
last-words replay, the authoritative final screen); a foreign resize defers
its repaint to unpark. Unpark re-enables the arm: small backlogs replay from
the ring, anything past `UNPARK_REPLAY_MAX_CHUNKS` (or a reflow, or a
never-sent snapshot) repaints — one snapshot instead of a megabyte replay.
Wire-compat is graceful both ways: old servers ignore unknown client text
frames (the client-side ParkedBuffer still absorbs their stream); old
clients never send park and get today's behavior. Pinned by two server-side
protocol tests in `tests/ws.rs` (parked attach withholds until the unpark
repaint; a live park stops output and a small backlog resumes from the
ring).

**R2 — attach-parked (no initial snapshot for hidden terminals). SHIPPED.**
`auth` gains `parked: true`: the server attaches via `attach_quiet` (subscribe
only — no ~95 ms render under the term lock, no snapshot bytes) and does not
adopt the hidden window's dims. The client desyncs its ParkedBuffer on a
parked ready (no snapshot is coming; pre-drop bytes predate an output gap), so
the first adopt resyncs into one fresh visible attach — a wake-from-sleep
reconnect of 12 parked terminals now transfers ~0 snapshots instead of 12.

**R3 — predictive local echo for remote shells (client-only). SHIPPED.**
Nothing server-side beats 1×RTT (F1), so mask it the mosh way:
`terminal/localEcho.ts` ghosts predicted keystrokes as a translucent DOM
overlay — never buffer writes, so a wrong prediction needs no rollback (worst
case is cosmetic). Armed only when: remote window, measured link RTT ≥ 45 ms,
the shell at its OSC 133 prompt (agent TUIs stay "unknown" and never arm),
primary screen, viewport at bottom, printable ASCII. Any control byte, escape
sequence, or over-long paste clears every ghost — over-clearing is always
safe, the truth is already painted.

**R4 — RTT-aware UI plumbing. (a)+(b) SHIPPED, (c) open.** (a) `net/rtt.ts`
estimates link RTT as the rolling minimum of /health fetch timings; a remote
window's host chip shows it once it crosses 30 ms. (b) Waterfall spot-check:
file open (fileStore single-flight, one fetch per surface), git view
(epoch-invalidate → one /git/status pull), quickopen (one fetch) — no
sequential chains found; the invalidate-and-pull architecture already keeps
interactions at ≤1 round trip. (c) carrying small pulls on the events WS
remains open — worth it only if a hot path shows up with a cold-fetch habit.

**R5 — `Compression=yes` on the chimaera ControlMaster. SHIPPED, with an
honest caveat.** Adopted in `ssh_opts` (chimaera's own masters only; the
user's ssh config is untouched). A clean A/B could NOT be measured in the
audit session: compression is negotiated at master creation, the live master
predated the option, and a fresh connection needs an interactive Duo auth.
Rationale stands on the traffic shape (terminal text and escape-sequence
snapshots compress enormously; zlib CPU measured negligible at these rates);
if a fast-LAN workflow ever regresses, this is one line to revert. Two
operational notes: an already-running master (ControlPersist keeps them for
10m past last use) ignores the option for its whole life — the win only
lands on freshly created masters — and the option also rides `scp` of the
release binary (mildly compressible; harmless).

**R6 — connect/reconnect round trips (chimaera-remote + the app shell).
Shipped; live-verified on Sherlock for connect/update timings (probe
1.85→1.37 s, stop 1.70→1.08 s, start 2.80→1.38 s); the wedge path and the
monitor backoff are unit-tested, not yet observed live.** Every separate
`ssh host cmd` through the ControlMaster is a
channel-open RTT plus a remote fork on a loaded login node (~300-500 ms at
~170 ms RTT), so the connect path folds its serial execs: the probe is ONE
exec (manifest + `kill -0` computed remotely — POSIX sh + `sed`, no `jq`),
the post-start wait is ONE exec running a remote ≤15 s loop (was up to 30
client-side execs), and stop is ONE exec (SIGTERM + a ≤10 s remote wait; the
exit code is the wire; still SIGTERM-only). The app's health monitor backs a
confirmed-down tunnel off (3 s doubling to a 30 s cap — a dead host was
costing ~12-20 failed 2 s authenticated probes a minute, forever), and a
reconnect after a confirmed down first clears a wedged ControlMaster (alive
process, dead TCP after laptop sleep: `-O check` → bounded `BatchMode`
session open → `-O exit`) so it dials fresh instead of queueing ~45 s behind
ssh's keepalive. hosts.json I/O moved off the tokio reactor
(`spawn_blocking`). The remote scripts are pure builders sent as `sh -c '…'`
— sshd hands the command to the LOGIN shell, and tcsh/fish would not read
them bare — pinned by tests that run the wrapped command through sh, dash,
bash, zsh, tcsh, and fish where installed. Follow-up: the pre-existing start
line is still sent bare and dies under a csh/fish login shell, as before.

**Non-goals:** transport replacement (QUIC/mosh-style datagrams) and HTTP/2 for
the tunnel (browsers require TLS for h2; a localhost-tunnel TLS story costs
more than the channel-open RTT it saves).

**Watch-item:** a local flood spiked daemon RSS 20 → 127 MB (recovering after)
— inside the ~150 MB login-node budget but close under 12 busy sessions worth
of scrollback + broadcast buffers. Re-check under R1 (parked sessions still
parse into the server grid; only forwarding stops).

## Reproducing

```
# on the host: run an isolated daemon (release binary is fine)
CHIMAERA_HOME=~/chimaera-perftest/state PORT=39717 ./chimaera serve

# locally: forward through the existing ControlMaster, then
node scripts/perf/tunnel-gauge.mjs http://127.0.0.1:39717 <token> <flood-count> <session-ids…>
node scripts/perf/tunnel-count.mjs http://127.0.0.1:39717 <token> <seconds> <label> <session-ids…>
```

`tunnel-gauge` connects a window's worth of session sockets, measures idle vs
under-flood keystroke echo RTT on the "visible" one, floods N "parked" ones via
`/exec`, and prints per-socket byte/frame accounting. `tunnel-count` is the
passive second-window counter used for the duplication measurement. Beware two
harness footguns hit during this audit: `pkill -f`/`pgrep -f` patterns that
match the invoking ssh wrapper's own command line (bracket-trick them), and
zsh's lack of word-splitting on unquoted variables.
