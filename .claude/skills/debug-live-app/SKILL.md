---
name: debug-live-app
description: Debug a running Chimaera daemon + web UI against a live isolated preview — read daemon logs, the browser console, and network traffic; reproduce a failure; and recognize the common failure modes (blank UI, 401, port in use, HMR vs rebuild). Use when the app misbehaves at runtime, a change doesn't show up, the UI won't connect, or you need to see what the daemon is actually doing.
---

# Debugging a live Chimaera

Chimaera's runtime bugs (terminal state, reconnect, resize, previews, agent
launch) reproduce **against the real daemon + UI**, not in unit tests. This skill
is how you observe one. Bring the app up with the **develop** skill
(`chimaerad-isolated` preview), then read it with the available inspection tools.

## Use safe fixtures and bounded diagnostics

- Never use the maintainer's real chats, research data, or cluster files as a
  repro. Do not copy them into tests, fixtures, docs, or logs you keep.
- Use the fixtures the repo has: the fake Claude agent
  ([`crates/chimaera-agent/src/bin/fake-claude.rs`](../../../crates/chimaera-agent/src/bin/fake-claude.rs)),
  the agent test fixtures in `crates/chimaera-agent/tests/fixtures`, the headless
  daemon smoke (`scripts/smoke-daemon.mjs`), and the web-UI `*.fixture.*` files.
  Use a throwaway workspace directory for file and terminal repros.
- Bound every diagnostic you collect. Read logs with a line limit (for example
  `preview_logs` with `lines`, or `tail -n 200`) and a time window. Do not dump a
  whole log or a whole response body into your context. Cap saved evidence at a
  few hundred lines or a few hundred KB. Redact tokens before you keep any URL.

## Read the three surfaces

| Want to see… | Tool | Notes |
|---|---|---|
| What the **daemon** did | `preview_logs` (`level: error` to filter) | Rust `tracing` goes to the daemon's stderr; this is your server log. |
| A **client** error | `read_console_messages` (`onlyErrors: true`) | JS exceptions, failed sockets, Svelte warnings. |
| **API / WS** traffic | `read_network_requests` (`urlPattern`) | Every `/api/v1/*` call with status; pass a `requestId` to read a response body. |
| The **rendered** state | `read_page` (structure) / `computer` `screenshot` (visual) | The page tree is text — better for asserting content/roles than a screenshot. |
| A **CSS** value | `javascript_tool` (`getComputedStyle`) | More reliable than eyeballing a screenshot. |

The table describes preview tooling when it is exposed in your session. Otherwise,
use the develop skill's manual isolated launcher and its terminal output or log
file, plus the available browser/native-app tools. Do not assume unavailable
`preview_*` APIs exist; name any surface you could not inspect.

Use `javascript_tool` for one-off inspection/repro (`window.location.reload()`, read a
store value) — never to *implement* a change; edit source and reload.

## Reproduce a failure

1. Write the smallest reproduction first: the fewest steps, on a fresh isolated
   daemon, with a fixture from above. If you cannot reproduce it, stop. Report
   that, with the bounded evidence you have.
2. Bring up `chimaerad-isolated`; navigate the preview to the token URL from
   `preview_logs` (serve mode has no `/dev/manifest` auto-auth — the token rides
   the `#token=` fragment).
3. Drive the exact path that breaks (open a folder, spawn a terminal, kill the
   socket + reattach, resize, open a preview, launch an agent).
4. Watch all three surfaces above as you go. A daemon-side panic shows in
   `preview_logs`; a client crash in `read_console_messages`; a contract mismatch in
   `read_network_requests` (a 4xx/5xx or an unexpected body).
5. Read source to diagnose → edit source → reload. A **debug** daemon reads
   `web-ui/dist` from disk per request, so after a UI change just rebuild the UI
   (`npm --prefix web-ui run build`) and reload — no daemon restart.

## Record hypothesis, test, result

Keep one entry per hypothesis, in the order you test them:

```
Hypothesis: <one cause, stated so a test can disprove it>
Test:       <the one change or observation that separates it from the others>
Result:     confirmed | refuted | inconclusive (<what you saw>)
```

Test one hypothesis at a time. Change one thing between observations. Put the
final record in the PR or the report, so the next person does not repeat a refuted
path.

## Common failure modes (recognize these fast)

- **Blank UI on a built/served daemon** → stale or empty `web-ui/dist`. Rebuild the
  UI (Node 26). In the Vite dev loop this can't happen (HMR); on a served daemon it can.
- **UI change not showing** → served daemon won't HMR — rebuild `web-ui/dist` and
  reload. In the Vite dev loop it should HMR; if not, check the Vite process.
- **401 on every `/api/v1/*`** → the page has no token. In serve mode the token must
  ride the URL fragment; re-navigate to the `#token=…` URL from `preview_logs`.
- **Port already in use** → another daemon is up. `chimaerad-isolated` auto-assigns a
  port, so this usually means a stale process — read the new port from `preview_logs`.
- **WS connects then drops / chat replays unexpectedly** → inspect auth,
  `last_seq` and the journal head on `/ws/chat/{id}`. Chat replays the gap;
  `/ws/events` sends full roster snapshots and PTYs reattach with screen
  snapshots. See **verify-app** for each transport's contract.
- **Memory balloons on a big file** → inspect which layer retains it. Large
  ranged/paged formats must stay incremental; whole-file viewers must enforce
  their format caps. Unbounded extraction is a resource-discipline regression
  (daemon target ~150 MB RSS).

## Then verify

Once it's fixed, don't just eyeball it — drive the flow end-to-end and capture proof
(a screenshot / `read_network_requests` / `preview_logs`). The **verify-app** skill
is the acceptance bar; state what you ran and observed.
