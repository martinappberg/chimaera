# Client transport map

The public UI runs directly on a daemon, inside the native shell, or beneath a browser gateway path. Read [the transport contract](../../../../crates/chimaera-link/PROTOCOL.md) before changing gateway behavior.

| File | Responsibility |
|---|---|
| `base.ts` | derive the immutable per-tab `/app/{host}` prefix and construct daemon HTTP/WebSocket URLs |
| `api.ts` | authentication bootstrap, typed HTTP wrapper, browser CSRF marker, reauthentication signal |
| `events.ts` | daemon event stream and reconnect lifecycle |
| `native.ts` | native IPC with browser fallbacks and prefixed window URLs |

Never put browser credentials in JavaScript, storage, URLs or proxy pages. Gateway requests use an HttpOnly cookie and same-origin proof; account browser sessions are distinct from daemon bearer tokens. Host choice belongs to the URL, never a shared cookie. Keep direct/native URLs unchanged, and preserve nonsecret workspace/window bootstrap metadata when switching hosts. Arbitrary preview apps must run on their isolated preview origin; their opaque proxy ids are capabilities, never raw daemon tokens. No user-selected upstream origin is accepted by the gateway helper.

Checks: `npm --prefix web-ui run check`, targeted `base.test.ts`, and live shared-UI HTTP + terminal/event WebSocket verification through the gateway. A passing direct-daemon check alone does not exercise gateway prefixes.
