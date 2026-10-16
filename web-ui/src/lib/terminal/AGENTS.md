# web-ui/src/lib/terminal — the PTY client stack

Orientation for coding agents. This area owns the browser half of a terminal: the xterm pool, the per-session PTY socket, path and URL links, predictive local echo, and the terminal-only window. It does not own terminal state: the daemon holds the grid and sends snapshots (`crates/chimaera-pty`). It does not own structured chat: a session shown as chat lives in `chat/`. Parent map: repo-root [AGENTS.md](../../../../AGENTS.md). Rules: [.claude/rules/web-ui.md](../../../../.claude/rules/web-ui.md). Feature pages: [terminals](../../../../docs/features/terminals.md), [linked-terminals](../../../../docs/features/linked-terminals.md).

## File map

| File | What it owns |
|---|---|
| `termPool.ts` | The facade App imports on every route. Synchronous reads stay synchronous; mutations queue behind one cached dynamic import of the runtime. `PoolHandlers` is the callback contract with App. |
| `termPoolRuntime.ts` | The pool: one warm xterm `Terminal` + `SessionSocket` per session id, `show()` / `release()`, the hidden stash, the LRU cap (`POOL_CAP = 12`), per-container refits (80ms debounce, off during a divider drag), and `hoistStyles`. |
| `parkedBuffer.ts` | Pure state machine for a parked terminal's output: buffering, awaiting-snapshot, desynced. |
| `ws.ts` | `SessionSocket`: the per-session PTY WebSocket (auth with grid dims, binary output, resize, park/unpark, resync). Reconnect policy comes from `net/reconnect.ts`. |
| `Terminal.svelte` | The pane view: mounts the pooled terminal into its host element, plus find. |
| `TerminalWindow.svelte` | A terminal-only window (`term=` hash param) for a cluster login-node shell. Links are off. |
| `links.ts` | File-path links: parse with `shared/fileRef.ts`, validate in batches with `POST /fs/validate`, cache verdicts, open on click. |
| `urlLinks.ts` | Underlines only proxyable URLs (loopback, or a host with an explicit port) and opens them in a browser pane. |
| `localEcho.ts` | Predictive local echo as a ghost overlay on high-RTT remote links (`LOCAL_ECHO_MIN_RTT_MS = 45`). |
| `terminalKeys.ts` | macOS Option+Left/Right → readline Meta-b/f, outside full-screen TUIs. |
| `terminalMetrics.ts` | Font settings and cell measurement without loading xterm (`estimateSize`). |

Tests: `*.test.ts` beside the modules. `net/fixtures/wire/pty-socket.json` + `pty-types.json` (written by the daemon's tests) go through `SessionSocket` in `net/wireFixtures.test.ts`: the text-frame vocabulary of `/ws/sessions` as the daemon sends it.

## State owners

| State | Owner | Who mutates |
|---|---|---|
| The pool (`Map` of entries: xterm, socket, parked buffer) | `termPoolRuntime.ts` | Its functions only, through the `termPool.ts` facade. |
| Session title, exit, selection | App, through `PoolHandlers` callbacks | App. |
| Terminal grid, scrollback, cwd, busy state | The daemon | The daemon. The client never serializes the grid. |

## Invariants / gotchas

- **Terminal state is server-side.** Never serialize the xterm grid (root AGENTS.md). A reconnect or adopt gets a fresh snapshot from the daemon.
- **xterm instances are plain objects, never `$state`.** `previews/DiffView.svelte` and `previews/buffers.svelte.ts` cite this rule.
- **Parked terminals buffer and do not parse.** Parking is an explicit lifecycle flag (`release()` parks, adopt unparks). Do not derive it from DOM position: a drag-out can move DOM without a release (`termPoolRuntime.ts` header).
- **A desynced parked buffer forces a resync on adopt.** The latch clears only after a resync was issued (`parkedBuffer.ts` header).
- **A parked attach omits grid dims.** A hidden window's stale dims must never reflow the server grid (`ws.ts`, `SessionSocketHandlers.parked`).
- **Resize before reset.** When the server tags a resync with its grid, resize to that grid before the snapshot is written (`onReset` doc).
- **Input frames drop while the socket is down.** `sendText` returns `false` then, and the caller falls back to a one-shot socket. See the rule bullet "Never lose a user action to a closed socket".
- **Only real files underline.** `links.ts` validates every candidate with the daemon. General web URLs are not linkified (`urlLinks.ts` header).
- **Local echo is a ghost, never buffer state.** The xterm buffer holds only what the server sent (`localEcho.ts` header).
- **xterm plants a `<style>` element.** `hoistStyles` moves it out of the re-parented element. See the rule bullet "Hide with opacity + inert".

## Checks

```sh
npm --prefix web-ui run check
npm --prefix web-ui run test -- src/lib/terminal
```

Resize, reconnect, park, and resync bugs pass unit tests and fail live. Drive the real PTY in the isolated preview (**verify-app**).

## Neighbours

[net](../net/AGENTS.md) · [layout](../layout/AGENTS.md) · [browser](../browser/AGENTS.md) · [shared](../shared/AGENTS.md) · [chat](../chat/AGENTS.md) · daemon side: [chimaera-pty](../../../../crates/chimaera-pty/AGENTS.md).
