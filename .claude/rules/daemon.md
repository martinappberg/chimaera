---
description: Hard constraints for the daemon (chimaera-server + the chimaera binary) — resource discipline, storage, auth, async hygiene.
paths: ["crates/chimaera-server/**", "crates/chimaera/**"]
---

# Daemon rules (server + binary)

The daemon lives on shared HPC **login nodes**. These are review criteria, not
nice-to-haves. Depth + module map: [chimaera-server/CLAUDE.md](../../crates/chimaera-server/CLAUDE.md).

- **Bounded resources.** Target ~150 MB RSS, <1 core steady-state. No unbounded
  buffers, no busy loops, hard ceilings on preview/extraction. A change that works
  but leaks or busy-loops is not done.
- **No SQLite anywhere near NFS/Lustre.** Durable state is append-only, size-capped
  JSONL under `~/.chimaera`. Hot state (`$XDG_RUNTIME_DIR`/`/tmp`) is
  reconstructible — it gets night-scrubbed; never assume it survives.
- **No blocking fs on the async reactor.** Wrap journal reads/copies/canonicalize in
  `spawn_blocking`. Never hold a `std::sync::Mutex` guard across an `.await`.
- **Auth every new route.** REST goes behind the bearer middleware; WS authenticates
  on its first frame; `/raw` uses a short-lived ticket. The only unauthed routes are
  the key-in-URL hook/MCP endpoints (`/agent-events/{id}`, `/mcp/{id}`) — claude's
  hooks/MCP can't know the daemon token. Don't add an unauthenticated endpoint.
- **The daemon↔UI wire is a stable public interface.** Core structs (`SessionInfo`,
  `SessionEvent`, `ExecOutcome`, chat `SeqEvent`/`AgentCommand`) serialize straight to
  the wire — changing a field changes the contract. Treat it deliberately; don't let
  its shape drift as a side effect of a refactor.
- **`chimaera serve` is a load-bearing CLI string** (chimaera-remote runs it over ssh).
- **Format with `cargo +1.96.0 fmt`** — the pinned toolchain, matching CI's fmt check.
