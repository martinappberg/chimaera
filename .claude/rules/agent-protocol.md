---
description: Hard constraints for the structured-agent drivers and the chat-mode glue — the pinned, unversioned wire protocols and the live smoke gate.
paths: ["crates/chimaera-agent/**", "crates/chimaera-server/src/chat.rs"]
---

# Agent-protocol rules

The claude `stream-json` and codex `app-server` wire formats are **unversioned**.
Depth: [chimaera-agent/CLAUDE.md](../../crates/chimaera-agent/CLAUDE.md) and
[PROTOCOL.md](../../crates/chimaera-agent/PROTOCOL.md).

- **Pinned, not trusted.** Each driver is verified against a pinned CLI version
  (`TESTED_*_VERSION`). Touching a driver — or bumping an agent CLI — **requires
  `just chat-smoke`** (live, bills a few cents). Hermetic tests cannot catch upstream
  drift. Record new wire facts in `PROTOCOL.md` the moment you learn them.
- **The seq number is the contract.** Assigned once in `Journal::append`; monotonic,
  gap-free per session. Don't reset/skip/reorder it. Keep `seq` the first serialized
  key of `SeqEvent` (the write-path scan depends on it).
- **The protocol is authoritative in chat mode** — derive session state from events,
  not hooks (hooks are unreliable under `-p stream-json`).
- **Keep the two drivers symmetric.** Same trait, same normalized model. If one
  handles a cap / turn-end reset / unhandled-frame arm, the other should too —
  asymmetries have been real bugs.
- **Bounded allocations at event construction** (`model.rs` caps) so a giant tool
  input never reaches the journal, ring, or a client.
- **argv lives in `launcher.rs`, not in drivers or `chat.rs`.**
