---
description: Hard constraints for the structured-agent drivers and the chat-mode glue — the pinned, unversioned wire protocols and the live smoke gate.
paths: ["crates/chimaera-agent/**", "crates/chimaera-server/src/chat.rs"]
---

# Agent-protocol rules

The claude `stream-json` and codex `app-server` wire formats are **unversioned**;
Antigravity and Grok Build use the negotiated ACP v1 adapter.
Depth: [chimaera-agent/AGENTS.md](../../crates/chimaera-agent/AGENTS.md) and the
[PROTOCOL.md](../../crates/chimaera-agent/PROTOCOL.md) index with its topic files
(`protocol/claude-code.md`, `codex.md`, `acp.md`, `cross-agent.md`).

- **Pinned, not trusted.** Each driver is verified against a pinned CLI version
  (`TESTED_*_VERSION`). Touching a driver — or bumping an agent CLI — **requires
  `just chat-smoke` for Claude/Codex or `just chat-smoke-acp` for ACP**
  (live, billed). Hermetic tests cannot catch upstream
  drift. Record each new wire fact in its `protocol/` topic file the moment you learn it, then add a dated entry to `protocol/passes.md`.
- **The seq number is the contract.** Assigned once in `Journal::append`; monotonic,
  gap-free per session. Don't reset/skip/reorder it. Keep `seq` the first serialized
  key of `SeqEvent` (the write-path scan depends on it).
- **The protocol is authoritative in chat mode** — derive session state from events,
  not hooks (hooks are unreliable under `-p stream-json`).
- **Keep the adapters consistent.** Same trait, same normalized model. If one
  handles a cap / turn-end reset / unhandled-frame arm, the other should too —
  asymmetries have been real bugs.
- **Bounded allocations at event construction** (`model.rs` caps) so a giant tool
  input never reaches the journal, ring, or a client.
- **argv is assembled in `launcher.rs`, never in `chat.rs`.** A driver contributes only its protocol flags (`claude::chat_args`); the binary, model, resume, settings and MCP arguments are the launcher's.
