---
name: chat-mode
description: Work on Chimaera's structured chat mode (Tier B) — the stream-json/app-server drivers, the seq-numbered journal + gap-replay, the server glue, and the chat web UI. Use when adding or changing a chat feature, a driver, the event model, the journal/replay, the view-switch/rewind flow, or the chat UI; when a chat change needs live verification; or when deciding whether a change requires the (billed) chat-smoke suite.
---

# Working on chat mode (Tier B structured agents)

Chat mode drives coding-agent CLIs through their **structured** protocols instead
of a PTY, and renders the result in a rich UI. It spans three places — read the
CLAUDE.md that governs the part you're touching first, they are the fast map:

- Engine: [`crates/chimaera-agent/CLAUDE.md`](../../../crates/chimaera-agent/CLAUDE.md)
  — drivers, journal, registry, event model. Wire facts:
  [`PROTOCOL.md`](../../../crates/chimaera-agent/PROTOCOL.md).
- Server glue: [`crates/chimaera-server/CLAUDE.md`](../../../crates/chimaera-server/CLAUDE.md)
  — `chat.rs`, the WS/REST seams, the lifecycle locks.
- UI: [`web-ui/src/lib/chat/CLAUDE.md`](../../../web-ui/src/lib/chat/CLAUDE.md)
  — the store reducer, the socket, the components.

## The end-to-end path (know it before you change it)

```
child stdio ─▶ driver (claude.rs/codex.rs) ─▶ AgentEvent ─▶ ChatManager pump
                                                               │ seq + journal + broadcast
   ws.rs /ws/chat/{id} ◀── ChatManager::attach (replay+live) ──┤
        │  ready(head) → batch replay → live ev                │
        ▼                                                       │
   chatWs.ts ─▶ store.apply (reducer) ─▶ ChatView              │
        ▲                                                       │
   socket.send(AgentCommand) ─▶ ws.rs ─▶ ChatManager::command ─┘─▶ driver ─▶ child stdin
```

The **seq number is the contract** end to end: assigned once in
`Journal::append`, replayed on reconnect from the client's `last_seq`. Don't
break, reset, or renumber it anywhere.

## Verifying a change — pick the right level

1. **Hermetic (always, free).** `cargo test -p chimaera-server` and the driver
   crate's `tests/manager.rs` (via `bin/fake-claude`) exercise the whole
   pipeline — spawn → handshake → mapping → journal → broadcast — with no
   network or billing. Add/extend a hermetic test for any logic change.
2. **Live app (for anything user-visible).** Bring up the isolated dev daemon +
   Vite (see the **develop** skill; use `chimaerad-isolated` in a worktree),
   open a chat session, and drive the actual flow — send a turn, hit a
   permission, toggle chat↔terminal, kill the socket and watch it reconnect.
   Don't claim a UI/lifecycle change works from unit tests alone.
3. **`just chat-smoke` (REQUIRED when a driver or an agent CLI changes; bills a
   few cents).** The wire formats are unversioned and pinned to
   `TESTED_*_VERSION`; only the live suite catches upstream drift. This is
   non-negotiable for driver/protocol edits — hermetic tests cannot see it.
   Record any new wire fact in `PROTOCOL.md` the moment you learn it.

## Rules that bite (all enforced in review)

- **Bounded everything** — the daemon lives on shared HPC login nodes (~150 MB
  RSS). Bounded channels, capped journal/ring, capped per-line reads, caps at
  *event construction*. No blocking fs on the async pump or the reactor
  (`spawn_blocking` journal reads/copies).
- **Agent output is untrusted** — sanitize it in the UI (`Markdown.svelte`);
  cap it before it enters an `AgentEvent`.
- **Protocol is authoritative** in chat mode — derive session state from events,
  not hooks (hooks are unreliable under `-p stream-json`).
- **Kill-then-respawn isn't atomic** — resolve respawn preconditions before
  killing; serialize concurrent view switches.
- **Keep the two drivers symmetric** — a fix to one (a cap, a turn-end reset, an
  unhandled-frame arm) usually belongs in the other.

## When you're done

- `just check` green (fmt + clippy + workspace tests); if `web-ui/**` changed,
  `npm --prefix web-ui run check` too (Node 22).
- Say in the PR what you ran and observed (hermetic + live; chat-smoke if a
  driver changed), per the repo's verify-live rule.
