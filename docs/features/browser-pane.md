# Browser pane (reverse-proxied web apps)

Live web apps — Jupyter, marimo, Streamlit, RStudio — as first-class workbench panes.
The daemon carries a ticketed reverse proxy, so an app listening on `localhost` **on the
daemon's host** (a laptop, a dev server, or a cluster compute node) renders in an iframe pane
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
plane `ANY /proxy/{id}[/{*path}]`; agent-opened panes add the MCP `open_browser` tool
(`browser_open.rs`) and a `browser_open` frame on `/ws/events`.

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
  - The daemon's own port is refused as a target (a same-origin loop). Another
    Chimaera daemon is reachable but not runnable: its UI served under `/proxy/…` shares
    the host window's origin, so `main.ts` shows a notice instead of booting
    (`net/api.ts::isNestedInProxy`) — booting would overwrite the host's token in the
    shared sessionStorage.

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
  ssh is hostbased on many clusters; anything interactive fails honestly),
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
- **The compute-node hunt.** An app started inside an allocation prints `localhost`
  URLs whose loopback is the *compute node's* — the daemon's own has nothing there. So
  when a loopback target is unreachable on a Slurm host, the pane probes the same port
  on the user's RUNNING jobs' nodes (single-node jobs, capped at 4 — already
  allowlisted, and a hit's proven route, relay included, stays cached). Exactly one
  answer moves the pane there with a dismissable "moved from localhost:PORT" chip in
  the chrome; several answers become one-click node choices in the can't-reach state;
  none stays honest silence.
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

## Agent-opened panes (`open_browser`)

- **What & when.** An agent that has just started a web app (a dev server, a notebook, a
  dashboard) calls the MCP tool `open_browser {url}` so the person watching the workspace
  sees what it is building, beside the agent, instead of a printed URL. Every MCP-equipped
  session has it (workers and Masterminds). It is **showing, not inspecting**: strictly
  one-way — nothing about the page (content, screenshots, load state) ever reaches the agent,
  and a successful call says nothing about whether the page works, so agents verify their
  app with their own tooling first and use the pane to present the result.
- **How it works.** The daemon parses the URL with the terminal link rules (`http` only, no
  userinfo, an explicit port unless the host is loopback — then 80; path, query and fragment
  kept), then applies the proxy's own mint allowlist (`proxy::check_target`, the function
  `POST /proxy` uses). A target that would need the in-pane confirmation is refused with a
  tool error telling the agent to give the user the URL; the daemon's own port stays refused.
  An accepted call pushes one additive frame on `/ws/events` —
  `{"type":"browser_open","session_id","workspace_id","host","port","path"}` — and mints
  nothing: the pane mints its ticket on mount, as every pane does. Older UIs ignore the
  unknown frame type.
- **Which window, where, and focus.** The window whose layout holds the calling session's tab
  acts, and so does any *visible* window showing the same workspace without that tab (a
  window cannot see another's layout, so two windows on one workspace may both open it).
  An existing tab on the same `host:port` is re-pointed and shown; otherwise the pane joins a
  pane already showing a browser, else fills an empty pane, else splits beside the session's pane (under the pane
  cap), else becomes a tab in a pane that is neither the session's nor the focused one. It
  never covers the session or the focused pane, keeps a zoomed pane zoomed, and never takes
  focus — the user keeps typing where they were
  (`web-ui/src/lib/browser/agentOpen.ts`, pure and unit-tested).
- **Who opened it.** An agent-opened tab remembers its opener (`BrowserTab.openedBy`, the
  session id; the latest opener wins when an agent re-points a pane). The pane's top bar
  ends with a quiet pill — the agent's mark (`SessionGlyph`) in its link hue, "opened by"
  and the session's name as the rail shows it, read live from the roster so a rename shows
  through; a click reveals that session (the linked-terminal chips' reveal path). Once the
  session has ended the pill turns into a muted note ("opened by fix CI · ended", or
  "opened by an agent that has ended" once the roster has forgotten it), no longer a
  control. In a narrow pane only the mark shows, the words in its tooltip. The user
  pointing the pane at another `host:port` (the address bar) drops the attribution — it
  would no longer be true; in-app navigation and the compute-node hunt (the same app,
  found on its node) keep it. Saved layouts carry it as an optional `wb` on the browser tab:
  older layouts restore unchanged, and a value that is not an id-shaped string is dropped
  without touching the rest of the tab.
- **In the transcript.** The call reads as what happened — "Showed localhost:8000 in a
  browser pane", "Didn't open example.org:8080 in a browser pane" for a refusal or when no
  window was connected (`chat/toolLabels.ts`; the address from Claude's input, else from the
  result text, which is all a Codex row carries; the query is left off, a Jupyter `?token=`
  being a credential). The expanded row shows the full result.
- **Key behaviors.** No queueing: a frame reaches only the windows connected when it is sent
  (a stale one older than 10 s is dropped), and with no window connected the tool says so and
  tells the agent to hand over the URL. Rate-limited per session (3 in any 10 s — an agent
  presenting a frontend and its dashboard in one step gets both — and 12 per hour; the
  refusal says how long to wait; bounded history). Always allowed: the harness does not prompt for it, since targets are
  held to the mint allowlist with no confirm path. **Where it lives:** `crates/chimaera-server/src/browser_open.rs` (parse, feed,
  limits, consumer count), the tool def in `mcp.rs`, the frame in `ws.rs::handle_events`,
  the UI half in `browser/agentOpen.ts` + `net/events.ts` + `App.svelte::onAgentBrowserOpen`,
  the attribution in `browser/BrowserView.svelte` (fed by `layout/Pane.svelte`).
- **Known limit.** A page that focuses itself on load (an `autofocus` field) can still pull
  focus into the pane: the pane treats focus moving into its iframe as the user clicking in.

## Links everywhere else (and the real browser)

- **What & when.** One policy for every link chimaera renders — terminal output, chat
  prose, markdown previews (both the authoritative render and the live split), the
  launcher's docs links, the update toast. A **proxyable** URL opens in a browser pane;
  anything else opens in the user's **real browser**.
- **Why it needs a native command.** In the app the window's navigation guard admits
  only the daemon origin, and nothing receives a `target="_blank"` new-window request —
  so an external link was silently swallowed (found live). Markdown previews were worse:
  their anchors carried no `target` at all, so a click was a *top-level* navigation that
  would replace the whole workbench in a browser. `open_external(url)`
  (`chimaera-app/src/shell/commands.rs`) hands the URL to the platform opener;
  `window.open` remains the plain-browser fallback.
- **Only http/https.** Hrefs are agent-authored and therefore untrusted, and the
  platform opener would act on `file:`, a `.desktop`, or an application scheme — so the
  scheme is checked client-side (`shared/urlOpen.ts`) *and* re-checked in the shell,
  where the rule is enforced once for every caller.
- **Where it lives.** `web-ui/src/lib/shared/urlOpen.ts` (the classifier + policy + the
  shared right-click menu; App registers the pane opener, the same module-handler
  pattern the reference/upload inserters use), `net/native.ts` (`openExternal`), and the
  command + its IPC lockstep in `chimaera-app`.
- **Key behaviors.** Click follows the default above; **Cmd/Ctrl+click** puts the pane in
  a fresh split (matching file links); **right-click** any rendered link for *Open in
  Chimaera* / *Open Beside* / *Open in Browser* / *Copy Link* — "Open in Chimaera"
  appears only when the URL is actually proxyable, so the menu never offers a pane that
  would just show "can't reach".

## Key constraints

- **Same-origin is deliberate, and for the real cases it costs nothing.** The proxied app
  is served from the workbench's own origin — that is what makes Jupyter's
  `frame-ancestors 'self'`, its cookies, and the absolute-path rescue work. It does mean
  script in a proxied page can reach `window.parent` (the daemon token in
  `sessionStorage`, the parent DOM). Weigh that against **what the app could already
  do**, because the token grants code execution *as the user, on the daemon's host*:
  - **loopback / self-host** — the app runs as the user on that same host and can read
    the manifest token off disk. Grants nothing new. This assumes the loopback port is
    the user's own: on a host other people can log in to, anyone can listen on a loopback
    port, and a page served from one runs with the workbench's origin like any other.
    A person clicking a URL chooses the port; an agent calling `open_browser` (pre-allowed
    by the maintainer's decision, so no prompt shows the URL) chooses it for them.
  - **remote workspace** (daemon and app on the same dev server or compute node;
    an explicit login-node override works the same way) — same identity, same host. Grants nothing new, and reaches nothing on the
    user's laptop.
  - **second-hop compute node** — when the daemon runs elsewhere and both hosts share
    the user's home, the app can already read that same manifest token.
  - The one non-equivalent case is a **confirmed remote** target *in the native app*,
    where `window.parent` also reaches granted Tauri commands (`connect_host` et al) —
    a remote-app -> local-machine hop. It takes deliberately confirming a hostile app.
  - **If we ever want to close that**, the answer is a separate origin (the data plane on
    its own port), which keeps a *real* origin so cookies, `Referer`, and the rescue all
    still work. Sandboxing is NOT a substitute: `sandbox` without `allow-same-origin`
    yields an opaque origin whose requests count as cross-site, so the app's own
    `SameSite` cookies are withheld (`SameSite=None` needs `Secure`, impossible on http)
    and the rescue loses both its cookie and its `Referer`. It would touch every tunnel
    path (`connect`, cluster job forwards), each forwarding a single port — not worth it for
    the residual risk above unless the threat model changes.
- **http upstream only.** The data plane opens a plain `TcpStream` and speaks clear-text
  HTTP/1.1, so a TLS-enabled app (`https://localhost:8443`) is deliberately **not**
  proxyable: `proxyableUrl` and the address bar refuse it and hand it to the user's real
  browser rather than showing a pane that could only fail.
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
_Captured 2026-07-21 (from the maintainer, on the shipping PR)._

- **Problem it solves (maintainer's words):** "a native browser within the workflow
  so that you can see everything from developing web apps to jupyter notebook to
  marimo etc. — it is a crucial part of an agent workbench and very helpful for the
  user." Not a bolt-on preview: the live-app surface belongs *inside* the workbench,
  beside the terminals and agents that produce those apps.
- **How settled:** "a crucial part of an agent workbench" — treat the capability
  itself (live web apps as first-class panes, remote-transparent through the daemon)
  as core to the workbench vision. The mechanics (rescue cookie, relay rungs, chrome
  layout) are how it works for now, improvable freely.
- **Constraints stated in the build request:** never an open relay (ticket-gated,
  target allowlisted to detected/user-confirmed addresses); bounded memory
  (streaming, no buffering); the daemon↔UI wire stays stable.
- **Follow-up he flagged immediately:** compute-node apps printing `localhost` URLs
  must not dead-end ("this we need to fix") — the compute-node hunt above is that
  fix, shipped in the same PR.

### Why agents can open a pane (`open_browser`)
_Captured 2026-10-04 (from the maintainer, in the session that built it)._

- **Problem it solves (maintainer's words):** "the in app browser is nice if we want
  someone to be able to preview what is building". The pane is the person's view of the
  agent's work, opened for them instead of a URL they have to click.
- **One tool, one direction, on purpose:** offered a way for agents to read the pane
  back, he declined: "it is enough then with open browser".
- **Never a way to validate:** "I dont want agents to think that is how they can validate
  the app ... they should use their own browsers inspect as much as possible". The tool
  description, the session instructions and the result text all say so; keep them saying it.
- **Silent, by decision:** he chose no permission prompt ("I'd rather it be silent") and
  confirmed "Silent everywhere" after being shown the shared-host risk recorded under
  Key constraints. Do not add a prompt, or a per-host exception, without asking.
