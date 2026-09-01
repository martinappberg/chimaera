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
(`scripts/perf/tunnel-gauge.mjs` — the harness that produced the numbers above).

**R1 — park/unpark on `/ws/sessions/{id}` (daemon + client pool).** The pool
sends `{"type":"park"}` when it stashes an entry and `{"type":"unpark"}` on
adopt. Parked, the server stops forwarding output frames (events still flow —
exit/title are cheap JSON) and remembers whether output occurred. Unpark
answers with the existing repaint idiom (`resync` + fresh snapshot) — and skips
it entirely when nothing happened while parked. Wire-compat is graceful both
ways: old servers ignore unknown client text frames; old clients never send
park and get today's behavior. Acceptance: gauge shows parked-socket bytes ≈ 0
during flood; unpark burst bounded by one snapshot; F4's per-window
multiplication applies only to *visible* terminals.

**R2 — attach-parked (no initial snapshot for hidden terminals).** `auth` gains
`parked: true`; the server skips the attach snapshot until first unpark. Window
open/restore then transfers one visible snapshot instead of 12. Stacks on R1
(same protocol surface, same PR or adjacent).

**R3 — predictive local echo for remote shells (client-only).** Nothing
server-side beats 1×RTT (F1), so mask it the mosh way: render predicted
keystrokes immediately (subtly styled), reconcile when the real echo arrives.
VS Code ships exactly this on xterm.js (`localEchoLatencyThreshold`), so it is
proven feasible on our stack. Guardrails: arm only when measured RTT exceeds a
threshold (~50 ms), and only in cooked-mode shell prompts — never inside
agent TUIs / alt-screen apps (use the existing OSC 133 shell-phase state).

**R4 — RTT-aware UI plumbing.** (a) Measure and surface link RTT in the host
chip — an honest "remote · 170 ms" beats a mystery. (b) Audit interaction
waterfalls so one user action costs ≤1 round trip (no sequential fetch chains
on file open / git view / quickopen); F2 makes every avoidable round trip cost
⅓ s. (c) Where a small pull rides an epoch-invalidate, prefer carrying it on
the already-open events WS over a fresh HTTP fetch.

**R5 — optional: `Compression=yes` on the chimaera ControlMaster.** Terminal
text compresses enormously and CPU headroom exists (F5). With R1 the bulk case
mostly disappears; compression still helps snapshots and file previews on slow
links. Benchmark first (zlib latency on large bursts) — adopt only if it
measures well.

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
