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
listed as the expected failure mode, not a measured one.)

## Fix plan

Ordered by leverage. Each lands with a before/after run of the tunnel gauge
(`scripts/perf/tunnel-gauge.mjs` — the harness that produced the numbers above;
`PARK=1` exercises the R1 protocol).

**R1 — park/unpark on `/ws/sessions/{id}` (daemon + client pool). SHIPPED.**
The pool sends `{"type":"park"}` when it stashes an entry and
`{"type":"unpark"}` on adopt. Parked, the server disables the output select
arm — the session's bounded broadcast ring (shared by every attachment, so
parking costs no extra daemon memory) becomes the catch-up buffer; events
still flow, an exit drains the withheld tail first (bounded slices), and a
foreign resize defers its repaint to unpark. Unpark re-enables the arm: the
ring replays contiguously, and an overflow surfaces as `Lagged` and repaints
through the existing path. Wire-compat is graceful both ways: old servers
ignore unknown client text frames (the client-side ParkedBuffer still absorbs
their stream); old clients never send park and get today's behavior.

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
if a fast-LAN workflow ever regresses, this is one line to revert.

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
