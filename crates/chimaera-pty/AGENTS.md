# chimaera-pty — the persistent terminal engine

Orientation for coding agents. This crate owns long-lived PTY sessions whose full
screen state lives **server-side** in a headless `alacritty_terminal::Term`, so it
survives with zero attached clients. Clients are ephemeral: attach returns a
self-contained escape-sequence snapshot that rebuilds an xterm.js of the same size.
Parent map: repo-root [AGENTS.md](../../AGENTS.md). Deep rationale + the resize
invariants: the [architecture guide](../../docs/agent-guides/architecture.md)
(`### In-window layout` → resize repaint refinement).

## The rule that governs everything here

**Terminal state is server-side; never serialize the `Term` grid across the wire.**
The client is xterm.js (JS), not Rust. On attach/resize you **re-emit an
escape-sequence snapshot** (`snapshot.rs`), never a serialized grid. Resize/resync
has subtle repaint invariants — read the architecture guide before touching them.

## File map

| File | What it owns |
|---|---|
| `lib.rs` | `SessionManager`: the session registry + `attach` (snapshot stream + live receivers), `resize`, `kill_all`. The crate's public surface for `chimaera-server`. |
| `session.rs` | `Session`: PTY spawn, the output→`Term` mirror, `resize`, the wait/last-words reaper, the bounded output broadcast (`OUTPUT_CHANNEL_CAPACITY`). |
| `snapshot.rs` | `render_snapshot` — rebuilds the terminal as an escape stream (SGR minimization + private-mode/cursor/title restoration). `screen_text` for text scrapes. |
| `marks.rs` | The OSC 133/633/7 marks scanner → shell phase + a bounded command journal. Most methods are exec-internal correlation; the server uses `phase()`/`journal()`. |
| `exec.rs` | The exec engine that types agent commands into a live shell (integrated or sentinel mode) and correlates completion via marks. |
| `tests.rs` | PTY + snapshot-replay tests (feed a synthetic `Term` via the vte processor). Extend these when you touch screen state. |

## Invariants (breaking these is a review failure)

- **Bounded allocations.** The daemon runs on shared HPC login nodes (~150 MB RSS).
  The per-session output broadcast is capped; a lagging client is `Lagged`→resynced,
  not buffered without bound.
- **Snapshot must round-trip.** `snapshot_replay_matches_live_grid` is the contract:
  a snapshot fed back through a fresh `Term` reproduces the live grid. Keep it green.
- **Known accepted gap:** a resync while on the alternate screen can't restore the
  primary screen's scrollback.
- Leaf crate: depends on nothing else in the workspace. `serde::Serialize` on the
  metadata types (`SessionInfo`, `SessionEvent`, `ExecOutcome`, …) is a benign wire
  convenience — but note those types ARE the daemon↔UI contract (see the server map).
