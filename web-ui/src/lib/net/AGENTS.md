# web-ui/src/lib/net — transport to the daemon

Orientation for coding agents. This area owns the client side of every daemon connection: authenticated REST calls, the `/ws/events` socket, the shared reconnect policy, pollers, uploads, and the bridge to the native shell. It does not own the per-session sockets themselves: the PTY socket is `terminal/ws.ts` and the chat socket is `chat/chatWs.ts`. Both use `reconnect.ts` from here. Parent map: repo-root [AGENTS.md](../../../../AGENTS.md). Rules: [.claude/rules/web-ui.md](../../../../.claude/rules/web-ui.md).

## File map

| File | What it owns |
|---|---|
| `api.ts` | Window identity from the URL fragment (`#token=…&ws=…&host=…&win=…`), persisted to sessionStorage, then stripped from the address bar. `api()` fetch with bearer auth. The `unauthorized` latch. The `/health` poll. |
| `events.ts` | `EventsSocket`: the daemon-wide `/ws/events` socket. Auth frame, then full `sessions` snapshots and the other pushes (`EventsSocketHandlers`: settings, git epochs, notices, `browser_open`, update status, plugin frames). Reconnects forever on unclean closes. |
| `reconnect.ts` | The backoff curve, jitter, hidden-document slow tier, `nudgeReconnectors`, the `reconnectingSockets` count, and `UNKNOWN_SESSION_RETRIES`. Shared by `terminal/ws.ts` and `chat/chatWs.ts` so the two cannot drift. |
| `poll.ts` | `POLL_FAST_MS` / `POLL_SLOW_MS` tiers, `healthPollDelayMs`, and `startVisibilityPoll` with its catch-up fetch on visibility return. |
| `rtt.ts` | Rolling minimum of `/health` fetch timings = the link RTT estimate. `LINK_RTT_BADGE_MS` sets when the host chip shows it. |
| `native.ts` | Bridge to the Tauri shell (`window.__TAURI__`). `isNativeShell()` gates shell-only features; every helper has a browser fallback. |
| `remoteReconnect.ts` | Native SSH reconnect view state, and `createReconnectListenerGate`: the reconnect waits until the `host-status` listener is attached. |
| `uploads.ts` | `POST /sessions/{id}/upload` and upload-to-directory, the `uploadJobs` chip store, `trackFileOp`, and the path-inserter hook that App registers (`setUploadPathInserter`). |

Tests: `*.test.ts` beside each module; `uploadNames.fixture.json` feeds `uploadNames.test.ts`.

**`fixtures/wire/*.json` are written by the daemon's tests, never by hand.** One file per shape family — `chat-events` (every `AgentEvent` as a `SeqEvent`), `chat-socket`, `pty-socket`, `pty-types`, `events-bus`, `session-rows` — each case `{name, note, frame}`. `wireFixtures.test.ts` feeds every frame through the real decoders with a fake `WebSocket` (`EventsSocket` here, `chat/chatWs.ts` + the reducer, `terminal/ws.ts`, the `workspace/sessions.ts` row helpers), once as sent and once with an unknown field spliced in, and asserts each lands in a known handler. A Rust change that moves a shape rewrites the fixture (`cargo test`); `just check` and CI refuse a stale one (`crates/chimaera-wire-fixtures`). If this test fails after a Rust change, the client no longer accepts what the daemon sends — fix the client in the same PR, or revert the shape.

## State owners

| State | Owner | Who mutates |
|---|---|---|
| `unauthorized` (writable) | `api.ts` | `notifyUnauthorized` / `clearUnauthorized` only. Any REST call or socket that sees a 401 calls `notifyUnauthorized`. |
| `reconnectingSockets` (writable) | `reconnect.ts` | The `Reconnector` class. Each down socket adds at most one. |
| `uploadJobs` (writable) | `uploads.ts` | This module's functions; `reportUploadError` for an external failure. |
| `askpassActive` (writable) | `native.ts` | `native.ts`. |
| RTT estimate (readable) | `rtt.ts` | `recordLinkRtt`, called by the `/health` poll. |

## Invariants / gotchas

- **The socket and poll rules are in [web-ui.md](../../../../.claude/rules/web-ui.md).** Read "Never lose a user action to a closed socket" and "Recurring work is gated on document visibility". `poll.ts` and `reconnect.ts` are the reference idioms that rule names. Do not hand-roll a new backoff or poll loop.
- **Every poll fetch has an abort timeout.** A hung fetch stalls the chain (rule bullet, `poll.ts`).
- **A hidden window takes the slow tier only from the third consecutive failure.** `HIDDEN_GRACE_ATTEMPTS = 2` keeps a transient drop fast (`reconnect.ts`).
- **Nudges are damped and spread.** `NUDGE_DAMP_MS` and `NUDGE_SPREAD_MS` stop a crash-looping daemon from causing a synchronized reconnect herd (`reconnect.ts` header).
- **`native.ts` command and event names are a contract with `crates/chimaera-app`.** Change them in the same PR on both sides (`native.ts` header).
- **The hash is the bootstrap channel.** `api.ts` reads `win` from it so a reopened window restores the same layout. Do not write app state into `location.hash`.
- **A failed upload types nothing.** The error shows on the job chip (`uploadAndInsert` doc comment). Only App knows how to insert a path into a chat composer or a PTY.
- **The `/ws/events` frame shapes are the daemon↔UI wire.** Do not change a handler's frame shape as a side effect of a refactor (root AGENTS.md). The daemon builds them in `crates/chimaera-server/src/ws/frames.rs`; `fixtures/wire/events-bus.json` is their current form.

## Checks

```sh
npm --prefix web-ui run check
npm --prefix web-ui run test -- src/lib/net
```

A change to reconnect or auth needs a live check: stop and restart the isolated daemon, and watch the window recover (the **verify-app** skill).

## Neighbours

[terminal](../terminal/AGENTS.md) · [chat](../chat/AGENTS.md) · [layout](../layout/AGENTS.md) · [workspace](../workspace/AGENTS.md) · [shared](../shared/AGENTS.md). Feature pages: [remote-connect](../../../../docs/features/remote-connect.md), [drag-drop-and-uploads](../../../../docs/features/drag-drop-and-uploads.md).
