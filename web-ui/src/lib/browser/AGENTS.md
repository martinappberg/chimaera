# web-ui/src/lib/browser — the browser pane

Orientation for coding agents. This area owns the browser pane: a live web app (Jupyter, marimo, Streamlit, …) shown in an iframe through the daemon's reverse proxy, and the window half of the agent `open_browser` tool. It does not own the proxy itself (`crates/chimaera-server`) or the link policy that decides when a URL opens here (`shared/urlOpen.ts`). Parent map: repo-root [AGENTS.md](../../../../AGENTS.md). Rules: [.claude/rules/web-ui.md](../../../../.claude/rules/web-ui.md). Feature page: [browser-pane](../../../../docs/features/browser-pane.md).

## File map

| File | What it owns |
|---|---|
| `BrowserView.svelte` | The pane view: the iframe on `/proxy/{id}`, back / reload / external / address field, and overlay states in place of raw browser error pages. Mints and re-mints the proxy session. |
| `proxy.ts` | Client of the proxy routes: `mintProxy` (idempotent per target), health polls, `ConfirmRequired`, address parsing, and the `browserTitles` store for tab names. |
| `agentOpen.ts` | Decides whether this window acts on a `browser_open` frame from `/ws/events`, and where the pane goes. Pure: layout ops from `layout/layout.ts`, no DOM, no stores. |

Tests: `agentOpen.test.ts`.

## State owners

| State | Owner | Who mutates |
|---|---|---|
| The tab's target (host:port + path) | The layout tab (`BrowserTab` in `layout/layout.ts`) | App, through layout ops (`setBrowserPath`, `setBrowserOpener`). |
| Proxy session id | `BrowserView.svelte`, local | The view. It is never persisted. |
| `browserTitles` (writable) | `proxy.ts` | `setBrowserTitle`. |

## Invariants / gotchas

- **A tab persists only its target.** The proxy id is a transport detail. The view mints it on demand and re-mints after expiry or a daemon restart (`proxy.ts` header).
- **A target outside the auto allowlist needs the user's confirmation.** The daemon allows loopback, this host, and compute nodes without asking; anything else raises `ConfirmRequired` (`proxy.ts`).
- **The daemon validates agent-opened addresses.** It applies the mint allowlist before it pushes `browser_open`. The window only decides placement (`agentOpen.ts` header).
- **Keep `agentOpen.ts` pure** so its placement decisions stay unit-tested.
- **Polls stop while hidden.** `BrowserView` reads `pageVisible`. See the rule bullet on recurring work.

## Checks

```sh
npm --prefix web-ui run check
npm --prefix web-ui run test -- src/lib/browser
```

Verify a change live: start a local web server in a terminal, click its URL, and check the pane through the isolated preview (**verify-app**).

## Neighbours

[layout](../layout/AGENTS.md) · [terminal](../terminal/AGENTS.md) (`urlLinks.ts` opens this pane) · [shared](../shared/AGENTS.md) (`urlOpen.ts`) · [net](../net/AGENTS.md) (`events.ts` delivers `browser_open`) · daemon: [chimaera-server](../../../../crates/chimaera-server/AGENTS.md).
