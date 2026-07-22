# Browser pane (reverse-proxied web apps)

Live web apps — Jupyter, marimo, Streamlit, RStudio — as first-class workbench panes.
The daemon carries a ticketed reverse proxy, so an app listening on `localhost` **on the
daemon's host** (a laptop, a dev server, an HPC login node) renders in an iframe pane
through the same origin and tunnel as the rest of the workbench: remote-transparent by
construction, with a second hop to Slurm compute nodes. Click the URL Jupyter prints in
any terminal and it opens beside your shell.

**Where it lives (shared):** daemon `crates/chimaera-server/src/proxy.rs` (the whole
proxy: registry, policy, data plane, relay hop; router rows in `router.rs`, sweeper +
relay teardown in `lifecycle.rs`). UI `web-ui/src/lib/browser/` (`BrowserView.svelte`
the pane, `proxy.ts` the mint/health client + title store) +
`web-ui/src/lib/terminal/urlLinks.ts` (URL detection) + the `BrowserTab` kind in
`layout/layout.ts`. Wire: `POST/GET /api/v1/proxy`, `DELETE /api/v1/proxy/{id}`,
`GET /api/v1/proxy/{id}/health` (bearer-authed) and the unauthenticated ticketed data
plane `ANY /proxy/{id}[/{*path}]`.

## Proxy sessions (the ticket model)

- **What & when.** A *proxy session* pins an unguessable 128-bit id to exactly one
  `host:port` target. Minting requires the bearer token; the data plane is authorized by
  the id alone (iframes cannot send Authorization headers — the `/raw/{ticket}` story).
- **How it's used.** The UI mints on pane mount and re-mints whenever a session expires
  or the daemon restarts — a `BrowserTab` persists only its TARGET, never a ticket, so
  restarts heal invisibly. Mint is idempotent per target.
- **Key behaviors / never-an-open-relay.**
  - Targets are allowlisted at mint: loopback and the daemon's own hostname always
    qualify; a node named in the user's own Slurm queue (the compute snapshot,
    nodelists expanded) qualifies; anything else needs `confirm: true`, which the UI
    sends only after an explicit in-pane dialog.
  - Nothing a data-plane request carries can change where it forwards; `Host`, `Origin`,
    and `Referer` are rewritten to the target's own authority (Jupyter rejects WS
    handshakes whose Origin mismatches its Host, and the ticket must never leak
    upstream in a URL). CONNECT is refused; an `x-chimaera-proxied` loop guard stops a
    target that forwards back to the daemon.
  - The registry is capped (32) with a 24h idle TTL; mounted panes keep-alive via the
    health route; the UI revokes a target's session when its last tab closes.
  - The daemon's own port is refused as a target (a same-origin loop).

## The data plane (HTTP + WebSocket pass-through)

- **How it works.** Per request: dial the target, `hyper` HTTP/1.1 client handshake,
  request and response bodies **streamed both directions** (never buffered — login-node
  RSS discipline). Hop-by-hop headers are stripped both ways. A 101 response relays
  verbatim and both sides become a raw `copy_bidirectional` byte tunnel (fixed buffers,
  global cap 256) — Jupyter kernels, Streamlit's `_stcore/stream`, marimo's `/ws` all
  ride this.
- **The absolute-path rescue.** Apps with `base_url /` (Jupyter) emit absolute
  `/static/…`, `/api/…` URLs that escape any path prefix. Serving a proxied *document*
  sets an HttpOnly `chimaera_proxy` cookie; requests that would otherwise fall to the
  SPA index.html and carry a live ticket in that cookie **or** a `/proxy/{id}/` Referer
  forward to that ticket's target with their original path. Real daemon routes always
  match first, reserved namespaces (`/api/v1`, `/ws/`, `/raw/`, …) are never rescued,
  the workbench shell is protected (`/` and top-level `Sec-Fetch-Dest: document`
  navigations are never rescued — rescuing one would hand the whole UI to the app), and
  chimaera's own embedded assets win before an app's `/assets/…` is tried. Two
  simultaneous absolute-path apps contend only on the cookie half (WS + redirects); the
  last document navigation owns it — reloading a pane re-claims it.
- **Header fixups** (headers only, bodies are opaque): absolute-path `Location`
  responses re-prefix under `/proxy/{id}`; `Referrer-Policy: same-origin` is injected
  when absent so app pages don't leak ticket URLs to external links.
- **Honest failure pages.** Expired sessions 404 and unreachable targets 502 with a
  quiet theme-neutral page + an `x-chimaera-proxy` header the client reads — never a raw
  browser error inside a pane.
- **Routing gotcha (pinned by test):** axum's `{*path}` refuses an empty tail, so
  `/proxy/{id}/` — the app's root document — is its own route row; without it the SPA
  fallback serves the workbench recursively inside the pane.

## The second hop (Slurm compute nodes)

- **How it works.** A non-loopback target dials direct TCP first (~3s — covers
  `--ip=$(hostname)` binds, the common cluster guidance). On failure the daemon stands
  up its own `ssh -N -L` relay child to the node's loopback (`BatchMode` — login→node
  ssh is hostbased on clusters like Sherlock; anything interactive fails honestly),
  connects through it, and caches the proven route per session. Relay children are
  owned by their registry entry: killed on revoke, idle expiry, and graceful daemon
  shutdown (never strand an `ssh -N` on a login node). Failures surface as the pane's
  "can't reach" state — probe-and-degrade-honestly, the compute posture.

## The pane (UI)

- **How it's used.** Click a proxyable URL printed in any terminal (Jupyter's
  `?token=` URL included — path + query ride along); or `Mod2+B` opens a blank pane
  with an address field (`localhost:8888`, `host:port/path`, a pasted URL, or a bare
  port). The chrome is quiet: back/forward/reload, the address (editable — a different
  `host:port` re-points the same pane), open-in-real-tab (the proxied URL, so it works
  for remote localhost too). The tab wears the live page title (Jupyter renames per
  notebook) over a globe glyph; states are honest overlays — connecting, confirm (for
  non-allowlisted hosts), can't-reach with quiet auto-retry, and a non-destructive
  "unreachable" chip when a running app stops answering.
- **Key behaviors.** The pane keeps its iframe alive across tab switches (the keep-alive
  layer model — a notebook never reloads because you glanced at a terminal); mounted
  panes ping health every 60s (doubles as the proxy keep-alive); navigation inside the
  app is tracked (same-origin iframe) and persisted onto the tab so reloads land where
  you were; `openBrowser` dedupes on target so clicking Jupyter's URL twice focuses the
  pane you already have (Cmd/Ctrl+click forces a fresh split).
- **URL detection** (`urlLinks.ts`): pure client-side regex over rendered terminal
  lines — zero daemon validation calls. Only proxyable URLs underline: loopback hosts,
  or any host with an explicit port. Ordinary web URLs (`https://github.com/…`) stay
  deliberately unlinkified — the standing terminals decision.

## Key constraints

- **Same-origin trust is deliberate.** The proxied app runs same-origin with the
  workbench — that is exactly why Jupyter's `frame-ancestors 'self'` and its cookies
  work through the pane. The delta over the status quo is small (the proxied server
  already runs as the user on the daemon's host and could read the manifest token off
  disk), but it is why non-local targets get the explicit confirm dialog. Don't proxy
  apps you don't trust.
- **Streaming, bounded, capped** — no response buffering, fixed tunnel buffers, capped
  registry/tunnels/relay children. Same review bar as previews.
- **The Vite dev loop can't exercise the rescue** (unknown root paths aren't proxied to
  the daemon); use the isolated daemon (`chimaerad-isolated`) to verify Jupyter-class
  apps live.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why the browser pane exists
_Intent: pending — drafted with the feature (2026-07-21); to be confirmed by the maintainer._

- **Problem it solves (from the build request):** the deliverables of bioinformatics
  work are often *live apps* (a Jupyter/marimo notebook, a Streamlit dashboard, RStudio)
  running on the machine that owns the data — usually a cluster login or compute node
  whose `localhost` the laptop cannot reach. The pane makes them one click from the
  terminal that started them, through the daemon the user already trusts.
- **Constraints stated up front:** never an open relay (ticket-gated, target
  allowlisted to detected/user-confirmed addresses); bounded memory (streaming, no
  buffering); the daemon↔UI wire stays stable.
