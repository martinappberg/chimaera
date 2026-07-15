---
description: Hard constraints for the PTY terminal engine — server-side terminal state, snapshot-on-attach, resize invariants, bounded buffers.
paths: ["crates/chimaera-pty/**"]
---

# PTY-engine rules

Depth + the resize invariants: [chimaera-pty/AGENTS.md](../../crates/chimaera-pty/AGENTS.md)
and the [architecture guide](../../docs/agent-guides/architecture.md) (resize repaint
refinement).

- **Terminal state is server-side.** Never serialize the `alacritty` `Term` grid
  across the wire — the client is xterm.js. On attach/resize, re-emit an
  escape-sequence snapshot (`snapshot.rs`).
- **The snapshot must round-trip.** `snapshot_replay_matches_live_grid` is the
  contract; keep it green when you touch screen state.
- **Resize/resync has subtle invariants** — read the architecture guide before
  touching that path. (Accepted gap: a resync on the alternate screen can't restore
  the primary screen's scrollback.)
- **Bounded buffers.** The per-session output broadcast is capped; a lagging client
  is `Lagged`→resynced, not buffered without bound (login-node RSS discipline).
- Extend `tests.rs` (real PTY + snapshot-replay) when you change terminal state.
