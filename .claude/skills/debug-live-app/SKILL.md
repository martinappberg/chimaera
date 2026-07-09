---
name: debug-live-app
description: Debug a running Chimaera daemon + web UI against a live isolated preview — read daemon logs, the browser console, and network traffic; reproduce a failure; and recognize the common failure modes (blank UI, 401, port in use, HMR vs rebuild). Use when the app misbehaves at runtime, a change doesn't show up, the UI won't connect, or you need to see what the daemon is actually doing.
---

# Debugging a live Chimaera

Chimaera's runtime bugs (terminal state, reconnect, resize, previews, agent
launch) reproduce **against the real daemon + UI**, not in unit tests. This skill
is how you observe one. Bring the app up with the **develop** skill
(`chimaerad-isolated` preview), then read it with the `preview_*` tools.

## Read the three surfaces

| Want to see… | Tool | Notes |
|---|---|---|
| What the **daemon** did | `preview_logs` (`level: error` to filter) | Rust `tracing` goes to the daemon's stderr; this is your server log. |
| A **client** error | `preview_console_logs` (`level: error`) | JS exceptions, failed sockets, Svelte warnings. |
| **API / WS** traffic | `preview_network` (`filter: failed`) | Every `/api/v1/*` call with status; pass a `requestId` to read a response body. |
| The **rendered** state | `preview_snapshot` (structure) / `preview_screenshot` (visual) | Snapshot is text — better for asserting content/roles than a screenshot. |
| A **CSS** value | `preview_inspect` (specific properties) | More reliable than eyeballing a screenshot. |

Use `preview_eval` for one-off inspection/repro (`window.location.reload()`, read a
store value) — never to *implement* a change; edit source and reload.

## Reproduce a failure

1. Bring up `chimaerad-isolated`; navigate the preview to the token URL from
   `preview_logs` (serve mode has no `/dev/manifest` auto-auth — the token rides
   the `#token=` fragment).
2. Drive the exact path that breaks (open a folder, spawn a terminal, kill the
   socket + reattach, resize, open a preview, launch an agent).
3. Watch all three surfaces above as you go. A daemon-side panic shows in
   `preview_logs`; a client crash in `preview_console_logs`; a contract mismatch in
   `preview_network` (a 4xx/5xx or an unexpected body).
4. Read source to diagnose → edit source → reload. A **debug** daemon reads
   `web-ui/dist` from disk per request, so after a UI change just rebuild the UI
   (`npm --prefix web-ui run build`) and reload — no daemon restart.

## Common failure modes (recognize these fast)

- **Blank UI on a built/served daemon** → stale or empty `web-ui/dist`. Rebuild the
  UI (Node 22). In the Vite dev loop this can't happen (HMR); on a served daemon it can.
- **UI change not showing** → served daemon won't HMR — rebuild `web-ui/dist` and
  reload. In the Vite dev loop it should HMR; if not, check the Vite process.
- **401 on every `/api/v1/*`** → the page has no token. In serve mode the token must
  ride the URL fragment; re-navigate to the `#token=…` URL from `preview_logs`.
- **Port already in use** → another daemon is up. `chimaerad-isolated` auto-assigns a
  port, so this usually means a stale process — read the new port from `preview_logs`.
- **WS connects then drops / replays the whole stream** → reconnect/gap-replay bug;
  the event bus should replay only the gap (seq-numbered). See **verify-app**.
- **Memory balloons on a big file** → a preview loaded a whole file instead of
  streaming; a resource-discipline regression (target ~150 MB RSS).

## Then verify

Once it's fixed, don't just eyeball it — drive the flow end-to-end and capture proof
(`preview_screenshot` / `preview_network` / `preview_logs`). The **verify-app** skill
is the acceptance bar; state what you ran and observed.
