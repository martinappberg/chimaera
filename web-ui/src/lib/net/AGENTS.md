# Client transport map

The public UI runs directly on a daemon, inside the native shell, or beneath a browser gateway path. Read [the transport contract](../../../../crates/chimaera-link/PROTOCOL.md) before changing gateway behavior.

| File | Responsibility |
|---|---|
| `base.ts` | derive the immutable per-tab `/app/{host}` prefix and construct daemon HTTP/WebSocket URLs |
| `api.ts` | authentication bootstrap, typed HTTP wrapper, browser CSRF marker, reauthentication signal; `ApiError` turns the daemon's project-connection codes (`project_unavailable`, `workspace_owned_elsewhere`, `remote_unavailable`, `workspace_scope_changed`, `read_only`, `worker_asleep`) into plain sentences and keeps the code on `.code` |
| `events.ts` | daemon event stream and reconnect lifecycle; `remote_unavailable`/`workspace_scope_changed`/`worker_asleep` mean "reconnect", only other errors are fatal |
| `placement.ts` | browser-view placement reads, scoped socket auth, `placementLabel` ("In the cloud" / "On another computer" / "· reconnecting") for routed rows, and `parsePause`/`sessionPause`/`pauseLabel`: a session's `moved`/`paused` state (socket frame or the row's additive `pause`) in plain words |
| `native.ts` | native IPC with browser fallbacks and prefixed window URLs |
| `proReturn.ts` | targeted account-return listener: register before consuming the native pending-window marker, coalesce events, and ignore late results after teardown |

Viewing a project that runs elsewhere is passive: sockets attach without `wake=interaction`, and only a user action carries wake intent. In a native window the daemon keeps the viewer socket open and holds the first input itself; in a browser view a send or keystroke into a dropped socket reconnects once with wake intent (never queued). Viewer chrome (terminal access strip, chat connection row) is gated on a routed row (`typeof session.placement === "object"`) or a browser view (`isBrowserGateway()`), so free users never see it. Viewport width matters only in an account-gateway browser view, and only as a starting default: at phone width (≤700 px) it starts terminals watching (`terminal/viewerMode.svelte.ts`) and opens its layout in focus mode (`App.svelte`). A plain daemon in a browser and a native window of any width keep the layout `main` gives them (free users see no change), and neither default is ever saved. The full contract is [VIEWING.md](../../../../crates/chimaera-link/VIEWING.md).

Never put browser credentials in JavaScript, storage, URLs or proxy pages. Gateway requests use an HttpOnly cookie and same-origin proof; account browser sessions are distinct from daemon bearer tokens. Host choice belongs to the URL, never a shared cookie. Keep direct/native URLs unchanged, and preserve nonsecret workspace/window bootstrap metadata when switching hosts. Arbitrary preview apps must run on their isolated preview origin; their opaque proxy ids are capabilities, never raw daemon tokens. No user-selected upstream origin is accepted by the gateway helper.

Checks: `npm --prefix web-ui run check`, targeted `base.test.ts`, and live shared-UI HTTP + terminal/event WebSocket verification through the gateway. A passing direct-daemon check alone does not exercise gateway prefixes.
