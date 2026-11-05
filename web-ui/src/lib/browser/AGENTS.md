# web-ui/src/lib/browser — the browser pane

Orientation for coding agents. This area owns the browser pane: a live web app (Jupyter, marimo, Streamlit, …) shown in an iframe through the daemon's reverse proxy, the window half of the agent `open_browser` tool, and the window half of the agent browser (#273: the daemon's `browser_*` MCP tools drive pages through this window's pane views). It does not own the proxy itself (`crates/chimaera-server`) or the link policy that decides when a URL opens here (`shared/urlOpen.ts`). Parent map: repo-root [AGENTS.md](../../../../AGENTS.md). Rules: [.claude/rules/web-ui.md](../../../../.claude/rules/web-ui.md). Feature page: [browser-pane](../../../../docs/features/browser-pane.md).

## File map

| File | What it owns |
|---|---|
| `BrowserView.svelte` | The pane view: the page on `/proxy/{id}` — in the macOS app a native pane view of its own (`shared/paneView.ts`; URL bar, title and path from its `page` reports, back/forward/reload through `nav`, text selections as references), else the same-origin iframe — back / reload / external / address field, and overlay states in place of raw browser error pages. Mints and re-mints the proxy session. |
| `frameLocation.ts` | Pure helpers both paths share: `inAppPath` (the app's own path under the proxy prefix), `onRootForm`, `pageAddress` (the app's own address for a reference: no query, no ticket). |
| `proxy.ts` | Client of the proxy routes: `mintProxy` (idempotent per target), health polls, `ConfirmRequired`, address parsing, and the `browserTitles` store for tab names. |
| `verifiedSessions.ts` | Proxy sessions this window minted and health-checked, by target, for 5 minutes: a remounted pane (a move, a reopen) shows its page at once and re-checks behind it. Kept out of `proxy.ts` so the entry bundle stays under budget. |
| `agentOpen.ts` | Decides whether this window acts on a `browser_open` frame from `/ws/events`, and where the pane goes; `placeHandover` places a hand-over tab the same way. Pure: layout ops from `layout/layout.ts`, no DOM, no stores. |
| `agentBrowser.ts` | The agent browser's window half, free of the DOM and the shell: `AgentBrowserChannel` answers each `browser_request` (open an agent's own page off-screen, drive/nav/close, hand over, the `state` marker) with one `browser_reply`; `PaneViewRegistry` (tab id → pane-view id); `shouldTakeOver` (the take-over gate); per-tab drive state for the chrome. Wire shapes live in `net/browserWire.ts`. |
| `agentBrowserRuntime.ts` | The window's one `agentBrowser` channel on the real `native.ts` calls, the mirrored `browserLinks` store (tab → agent), and the debounced `browser_hello` (visibility, focus, size, registered tabs). App attaches it to its events socket. |

Tests: `agentOpen.test.ts`, `agentBrowser.test.ts`, `verifiedSessions.test.ts`.

## State owners

| State | Owner | Who mutates |
|---|---|---|
| The tab's target (host:port + path) | The layout tab (`BrowserTab` in `layout/layout.ts`) | App, through layout ops (`setBrowserPath`, `setBrowserOpener`). |
| Proxy session id | `BrowserView.svelte`, local | The view. It is never persisted. |
| `browserTitles` (writable) | `proxy.ts` | `setBrowserTitle`. |
| `browserLinks` (readable) | `agentBrowserRuntime.ts` | `setBrowserLinks` (sessions frame), `setBrowserLinkLocally` (App's optimistic link/unlink/hand-over). |
| Tab → pane-view registry, agent-own views, drive state | `agentBrowser` (`AgentBrowserChannel`) | Its methods: `BrowserView` registers its view (`onViewId`), reports input (`userInput`) and Resume; App feeds requests and socket drops. |

## Invariants / gotchas

- **A tab persists only its target.** The proxy id is a transport detail. The view mints it on demand and re-mints after expiry or a daemon restart (`proxy.ts` header).
- **A target outside the auto allowlist needs the user's confirmation.** The daemon allows loopback, this host, and compute nodes without asking; anything else raises `ConfirmRequired` (`proxy.ts`).
- **The daemon validates agent-opened addresses.** It applies the mint allowlist before it pushes `browser_open`. The window only decides placement (`agentOpen.ts` header).
- **Keep `agentOpen.ts` pure** so its placement decisions stay unit-tested.
- **An agent's own page is a pane view framed past the window's right edge** (`offscreenRect`): WebKit counts it visible and runs it at full speed. It follows window resizes (`relayout`) and closes when the events socket drops (the daemon failed it).
- **A hand-over adopts, never reloads.** The tab carries a transient `adopt` (view id, never persisted); its view shows the page again at the URL this window last showed it at and without `agent` (the shell then navigates nothing and hands the view to the person), then owns it like any pane view. Until it mounts, requests by tab resolve to the adopting view.
- **Take-over is gated** (`shouldTakeOver`): only a shared pane, not paused, that its agent drives now or acted on within 15 s. The window pauses at once and the daemon's `state` confirms.
- **A pane follows its page to another target.** A page that navigates itself to another `/proxy/{id}` (an agent's `location.assign`) re-points the tab (`proxyTarget` resolves the id) while the live view stays; `paneView` takes a `url` the page itself already reported as shown, without a reload.
- **A shared tab that leaves the layout is unshared** (`DELETE /links/browser/{tab}`, App, `sharedTabsLeft`): its tab or pane closed, or the tab dragged to another window. A shared pane survives a window reload: the tab comes back with the layout, its new pane view registers, and the hello lists it again (the daemon re-instruments it for the new socket). A workspace switch is not a close either.
- **A browser tab shares only from its chain** (a link-intent drag or the menu), and only in the app (an agent drives a page through its pane view).
- **Polls stop while hidden.** `BrowserView` reads `pageVisible`. See the rule bullet on recurring work.

## Checks

```sh
npm --prefix web-ui run check
npm --prefix web-ui run test -- src/lib/browser
```

Verify a change live: start a local web server in a terminal, click its URL, and check the pane through the isolated preview (**verify-app**).

## Neighbours

[layout](../layout/AGENTS.md) · [terminal](../terminal/AGENTS.md) (`urlLinks.ts` opens this pane) · [shared](../shared/AGENTS.md) (`urlOpen.ts`) · [net](../net/AGENTS.md) (`events.ts` delivers `browser_open`) · daemon: [chimaera-server](../../../../crates/chimaera-server/AGENTS.md).
