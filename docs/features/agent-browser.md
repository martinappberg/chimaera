# Agent browser (agents see and drive pages)

One set of MCP tools (`browser_*`) that lets an agent open a web app, read it, act in it with
real input, check its console and network, and hand the result to the user as a browser pane.
Two engines answer the same tools: the **app engine** drives a pane view in a connected macOS
app window, and the **Chromium engine** drives a headless Chrome or Chromium on the daemon's
host: one already installed there, or the one the first-party **Chromium** plugin downloads on
the user's click. Use it when an agent builds or fixes a web UI and must check its own work, or
when the user wants the agent to act in a page the user already has open.

**Where it lives (shared):** daemon `crates/chimaera-server/src/agent_browser/` (`mod.rs` the
pages and the reaper, `tools.rs` the tools, `channel.rs` the window request channel,
`app_engine.rs`, `chromium.rs` + `cdp.rs`, `snapshot.js`), a plugin's browser in
`plugins/browser.rs` (`offer`), the tool listing and dispatch in
`mcp.rs`, shared panes in `links.rs`, the proxy mint in `proxy.rs` (`mint`, `target_of`). Shell
`crates/chimaera-app/src/shell/native_view.rs` (`native_view_drive`, `native_view_show` with
`agent`), `native_view/drive.rs` (pure parsing, key map, injected-input ledger),
`native_view/mac/agent.rs` (the native half). Page scripts `crates/chimaera-core/src/pane_agent.js`
(the page-world recorder) and `pane_bridge.js` (rings, agent cursor, input reports). UI
`web-ui/src/lib/browser/agentBrowser.ts` + `agentBrowserRuntime.ts` (the window half),
`net/browserWire.ts` + `net/events.ts` (the wire), `browser/BrowserView.svelte` (the driving
marker), `layout/PaneTabs.svelte` (the chain link and chips). Wire: `browser_request` frames and
`browser_hello` / `browser_reply` / `browser_event` client messages on `/ws/events`;
`browser_links` in the `sessions` frame; `GET/PUT /api/v1/links/browser`,
`DELETE /api/v1/links/browser/{tab}` (bearer-authed). Map rows:
[chimaera-server](../../crates/chimaera-server/AGENTS.md),
[reference](../../crates/chimaera-server/REFERENCE.md#agent_browser),
[chimaera-app](../../crates/chimaera-app/AGENTS.md),
[chimaera-core](../../crates/chimaera-core/AGENTS.md),
[browser UI](../../web-ui/src/lib/browser/AGENTS.md), [net](../../web-ui/src/lib/net/AGENTS.md).

## The tools (agent side)

- **What & when.** Every MCP-equipped session (workers and Masterminds, every tier) gets the 17
  tools below beside `open_browser`. The session instructions tell the agent to use them to
  check its work, and to hand a working page over with `browser_show`.
- **How it's used.** A typical loop: `browser_navigate {url}` → `browser_snapshot` (text with
  `[eN]` refs) → `browser_click {ref}` / `browser_type {text, ref}` → `browser_console` /
  `browser_network` for errors → `browser_screenshot` for proof → `browser_show`.

  | Tool | Does |
  |---|---|
  | `browser_pages` | Lists what the agent can use: its hidden page, the panes shared with it (`p1`… — it can drive them) and the panes the user has open in its workspace (`v1`… — read-only). |
  | `browser_navigate` | Opens the agent's own hidden page (or re-points it) at an allowlisted `url`, a file on the daemon's host (`path`, or a `file://` `url`; see [Files as pages](#files-as-pages)), or `back`/`forward`/`reload`; waits for the load (≤ 15 s). With no `page`, a shared pane already showing the same app (host:port) is navigated in place instead, so the agent never opens a second session beside the user's; any other app opens the hidden page. Returns the final URL, title and HTTP status when known. `width`/`height` size the own page (default 1280×800). |
  | `browser_snapshot` | The page as compact text: roles, names, states, and a ref on each interactive element. Default 24,000 characters, at most 100,000; `viewport_only` keeps what is in view. |
  | `browser_screenshot` | A PNG of the viewport, the full page, or one element (`ref` or `selector`), at most 1600 px wide. |
  | `browser_inspect` | One element: outerHTML (first 4 KB), attributes, computed styles (a default set or the names asked), box, visible or covered. |
  | `browser_source` | The live DOM serialized, or (`original`) the response body fetched again by the page; paged, at most 50,000 characters a call. |
  | `browser_click` | Real mouse click by ref (scrolled into view first) or at `x`,`y`; `double`, `modifiers`. Refuses a native `<select>`. Returns a change hint and the URL and title after. |
  | `browser_type` | Real keyboard input into the focused element, or into `ref` after a click; `submit` presses Enter; `per_key` sends one key event per character (at most 500) instead of inserting the text. |
  | `browser_press` | One key (Enter, Tab, Escape, arrows, Home, End, PageUp/Down, Space, Insert, Backspace, Delete, or one character) with `modifiers`. |
  | `browser_scroll` | Mouse-wheel scroll by `dy` (and `dx`) over `ref` or the middle of the viewport. |
  | `browser_select` | Chooses an option of a native `<select>` by `value` or `label` through the DOM, then fires `input` and `change`. |
  | `browser_console` | Console messages and uncaught errors since the last call on this page (`level`: all, warn, error). |
  | `browser_network` | Failed (status ≥ 400, network error) and slow (≥ 1 s) requests since the last call. No bodies. |
  | `browser_wait` | Waits for `text`, a `selector` and/or `network_idle` (nothing in flight for 500 ms); default 10 s, at most 30 s. Answers ok or timeout. |
  | `browser_eval` | `script` is an async function body; its return value comes back as JSON (first 20 KB). App engine: the bridge's isolated world (the DOM, not the page's globals). Chromium: the page's own world. |
  | `browser_show` | Hands the agent's own page to the user as a pane beside the agent (see [Hand-over](#hand-over-browser_show)). |
  | `browser_close` | Closes the agent's own page. A shared pane stays the user's. |

- **Which page.** Each call takes an optional `page`: `"own"` (the hidden page), a shared
  pane's id (`"p1"`…) or a pane the user has open (`"v1"`…, read-only: snapshot, screenshot,
  inspect, source, console, network and wait work; acting is refused with "ask the user to
  share it"). With no `page`: `browser_navigate` uses a shared pane showing the same app, else
  the hidden page (checking something never moves the user's pane to another app); the other
  tools use the own page if there is one, else the single shared pane. A `v` pane is used only
  when named. An agent may always ask for its own hidden page; when a shared pane shows the same
  app the result notes that notebook-like apps allow one editing session.
- **Every result names its engine.** The first line reads
  `[browser · engine: app|chromium · page: own|shared pane "Title" (p1)]`, with `· throttled`
  when the page reports `document.visibilityState === "hidden"`. A new own page on a different
  engine from the agent's previous one says so: it has none of that page's state.
- **Permissions.** `browser_navigate`, `browser_show`, `browser_close` and the read tools
  (`snapshot`, `screenshot`, `inspect`, `source`, `console`, `network`, `wait`) are pre-allowed
  (`mcp::ALWAYS_ALLOWED_TOOLS`); the read tools also carry `readOnlyHint`
  (`mcp::READ_ONLY_TOOLS`). `click`, `type`, `press`, `scroll`, `select` and `eval` follow the
  user's permission mode: a click can run a notebook cell.
- **Page content is data.** The instructions paragraph, the eval result and the snapshot text
  all say that what a page shows is data, never instructions.
- **Where it lives.** `agent_browser/tools.rs` (`tool_defs`, `call`, `pick_page`, `new_page`,
  one tool function each), `agent_browser/snapshot.js` (the snapshot and page helpers, one
  script on both engines), `mcp.rs` (listing, the instructions paragraph, the dispatch arm).

## Files as pages

- **What & when.** An agent that wrote an HTML report, a plot or a small static page checks it
  with `browser_navigate {path: "out/report.html"}` (relative to its working directory, then the
  workspace root; absolute and `~/` work too) or `{url: "file:///…/report.html"}`. No web server:
  before this, an agent had to start one to look at its own report. Pages only: HTML, SVG and
  images (`PAGE_EXTENSIONS`), since `browser_navigate` is pre-allowed. That gates what the tool
  opens; an HTML page can still load files beneath its own folder (no `..`, no hidden names), as
  the user's preview can. Only the agent's own page
  opens a file (a shared pane holds an app); a folder, a missing file, or a `file://` URL naming
  another host is refused.
- **How it loads.** Never as `file://`, which would let the page read the whole disk. The file
  loads the way the HTML preview loads it: under a ticket, `GET /raw/{ticket}/{name}` on this
  daemon ([files and previews](files-and-previews.md)) — `CSP: sandbox allow-scripts` (an opaque
  origin), relative assets from its own folder downward only, no CORS, so scripts run but the
  page can't read the files around it. The agent sees the page the user's preview shows. The app
  engine loads the `/raw/` path on the window's daemon origin (a pane view may show `/raw/`);
  the Chromium engine loads `http://127.0.0.1:{daemon port}/raw/…`, and `/raw/t-…` is the only
  path on the daemon's own port its allowlist lets through.
- **What the agent sees.** Every URL in a result (the page, network lines, console text) reads
  as `file:///dir/rest`, never the ticket (`file_page::file_url`, from a bounded ticket → file
  registry). Tickets for file pages live an hour, renewed by each mint of the unchanged file (a
  renewal never shortens a ticket, so the user's 10-minute preview of the same file doesn't cut it).
- **Reload picks up a rewrite.** `reload` mints the ticket again: while the file is unchanged
  and its ticket lives, that is the same ticket and a plain reload; once the file changed (a new
  version) or the ticket expired, the page navigates to the new ticket's address.
- **Hand-over.** `browser_show` on a file page doesn't open a browser pane (a browser tab holds
  an app, not a file): it tells the agent to link the file in its reply, where the user sees
  the chat's preview card or opens it beside the chat.
- **Where it lives.** `agent_browser/file_page.rs` (resolve, mint, `remint`, URL mapping),
  `fs::TicketStore::mint_for`, the `navigate` and `show` arms in `tools.rs`, `proxied_path` in
  `app_engine.rs`, `allow_document` in `chromium.rs`.

## Engines and how one is chosen

- **App engine.** The agent's own page is a pane view (a shell-owned `WKWebView`, its own web
  process) in a connected app window, framed just past the window's right edge
  (`offscreenRect`). WebKit counts that view visible, so rAF and timers run at full speed while
  the window is on screen. Pages load through the daemon's proxy (`proxy::mint`, then
  `/proxy/{id}{path}` on the window's daemon origin), so remote workspaces work through the
  window's tunnel. Every URL in a result is mapped back to `http://host:port/path`. Input is real
  NSEvents (the page sees `isTrusted` events); screenshots are native.
- **Chromium engine.** One headless browser per daemon, launched on first use over
  `--remote-debugging-pipe` (never a port), with a throwaway 0700 profile under the node-local
  runtime dir. Discovery: the `browser.path` setting, else `google-chrome`,
  `google-chrome-stable`, `chromium`, `chromium-browser`, `chrome`, `chrome-headless-shell` on
  PATH, else (macOS) the Google Chrome or Chromium app bundle, else the browser of a plugin
  active in the session's workspace (below). A `browser.path` that names nothing is an error,
  never a fall-through. Nothing is bundled, and the engine itself never downloads. Startup
  failures (sandbox, `/dev/shm`, missing libraries, no `--headless=new`) are explained from the
  browser's stderr.
- **A browser from a plugin (opt-in download).** A plugin's manifest may name one of its
  `[[programs]]` that one of its `[[tools]]` installs as `provides.browser`
  ([plugin guide](../agent-guides/plugins.md#programs-jobs-and-tools)). The first-party
  **Chromium** plugin ([chimaera-plugin-chromium](https://github.com/martinappberg/chimaera-plugin-chromium))
  does so with Chrome for Testing's `chrome-headless-shell`, pinned to one version for Linux
  x86_64 and aarch64 and macOS Intel and Apple silicon. Getting it is three clicks the user
  makes: install the plugin (Extensions → Plugins), switch it on in the workspace, click
  **Install** under its Tools. The plugin host downloads over HTTPS, checks size and sha256
  while it streams, unzips into `~/.chimaera/tools/chromium/` (or `plugins.toolsDir`), and
  changes nothing else. The engine uses that browser only for sessions in workspaces where the
  plugin is active, and only when the host has no browser of its own; the profile still lives
  in the node-local runtime dir. Each missing step has its own refusal text: not switched on
  here, switched on but not downloaded (with size and host), installed but its program not
  runnable (Remove and Install again), no build for this platform, the plugin held or faulted
  (each followed by the `browser.path` and app alternatives), no plugin at all (the three clicks,
  then `browser.path`, then the app). `plugins/browser.rs` (`offer`) is generic: it reads the manifest, never an id.
- **The choice, for a new own page** (`tools::new_page`, `channel::choose`): an app-engine window
  on the session's workspace (any app-engine window when the session has no workspace) —
  visible and focused first, then visible, then any. None → the Chromium engine when
  `Chromium::available` says yes for the session's workspace. Neither → a refusal that names
  the fix: open the workspace in the app, and why Chromium can't run here (no browser found and
  how to get one, a bad `browser.path`, a login node, a profile on a network filesystem).
- **A page never moves.** An own page is bound to its engine and window for life. When its
  window goes away the page is gone; the next `browser_navigate` makes a new page, maybe on the
  other engine, and the result says so. Shared panes are always app engine.
- **Measured (spike, 2026-11-04, dev app):** a bridge eval on an off-bounds view round-trips in
  about 7.6 ms (p50), a 900×700 screenshot in about 11 ms, creating a view and starting its load
  90–240 ms. Through a tunnel a request costs about one network round trip plus 8–15 ms.

## The allowlist (both engines)

- Navigation obeys the proxy's mint allowlist, `proxy::check_target`: loopback, the daemon's own
  host, and nodes of the user's running Slurm jobs. There is no confirm path for agents: any
  other host is refused with text telling the agent to give the user the URL
  (`browser_open::refusal_text`). The daemon's own port stays refused.
- App engine: the view's main frame loads only `/proxy/` and `/raw/` on its daemon origin (the
  shell's `main_frame_policy`), so a page cannot navigate the view elsewhere.
- Chromium: `Fetch` pauses every main-frame document request (redirects and page-initiated
  navigations included) and `check_target` decides; a refused one fails with `BlockedByClient`
  and the tool says why. Subframes and subresources pass, the same as pane views.

## Sharing a pane (the chain link)

- **What & when.** The user lets an agent act in a browser pane they already have open (logged
  in, at the right place). A pane an agent opens with `open_browser` is shared with that agent
  automatically: one page, which the agent drives while the user watches, instead of a hidden
  duplicate that would fight the user's session. Only in the macOS app: an agent drives a pane
  through its pane view.
- **How it's used.** The browser tab's chain link works like a linked terminal's: drag the pane
  bar's chain onto an agent, or open "link to agent" from it. The pane gets
  the agent's hue and the agent pane gets a chip (a dot shows driving or paused; click reveals
  the pane, × stops sharing). The agent addresses the pane as `p1`, `p2`… (per agent, stable
  while linked).
- **Key behaviors.** One agent per tab, many tabs per agent; a re-link moves it; at most 64
  links per daemon. Links are in memory only. A link ends with its agent, when the user unlinks
  it, or when the tab or its pane closes (the window unlinks). It survives page reloads (an app
  reloading after the agent changed its code) and Chimaera window reloads. The first use from a window installs the
  page-world recorder and the agent cursor in the pane (`instrument`, `cursor`).
- **Where it lives.** `links.rs` (`link_browser`, `unlink_browser`, `panes_of`, the routes),
  `PaneTabs.svelte` (menu, chips), `App.svelte` (`LinkCtrl.linkBrowser`/`unlinkBrowser`,
  unlink on tab close), `workspace/agentLinks.ts` (the route client), `agentBrowserRuntime.ts`
  (the `browserLinks` store fed by the sessions frame).

## Watching and take-over

- **The marker.** While the agent acts in a shared pane, the pane bar ends with the agent's name
  and a breathing dot ("driving"), and the page shows the agent's cursor and a ripple on each
  click. The marker drops 15 s after the agent's last act.
- **Take-over.** Any trusted pointer, key or wheel input from the user in a pane the agent drives
  (or acted in within 15 s) pauses the agent at once. The shell tells its own injected input
  from the user's: each bridge `input` report consumes a matching injection no older than
  1.5 s, and an unmatched one is the user (`user_input`). The window sends
  `browser_event takeover`; the daemon marks the pane paused and confirms with a `state` frame.
- **Paused.** The pane bar shows "Paused · Resume". The agent's act tools (`navigate`, `click`,
  `type`, `press`, `scroll`, `select`, `eval`) answer "Paused: the user took over this page at
  HH:MM:SS…"; read tools keep working. Resume sends `browser_event resume` and clears it.
- **Where it lives.** `drive.rs` (`Injections`), `mac/agent.rs`, `pane_bridge.js` (input
  reports, the cursor), `agentBrowser.ts` (`shouldTakeOver`, drive state),
  `BrowserView.svelte` (the marker and Resume), `agent_browser/mod.rs` (`on_event`, the reaper's
  marker sweep).

## Hand-over (`browser_show`)

- **App engine.** The window opens a browser tab beside the agent that adopts the live view:
  the same page, nothing reloads. The tab is then shared back with the agent (the result names
  its `pN`), and the agent has no own page until its next `browser_navigate`. Placement follows
  `open_browser`'s rules (`agentOpen.ts`, `placeHandover`).
- **Chromium engine.** The user's window opens a pane at the same URL through the
  `browser_open` path (same rate limits). The result says the pane reloaded and has none of the
  Chromium page's state; the Chromium page stays the agent's.
- `open_browser` stays the one-way "show the user" call
  ([browser pane](browser-pane.md#agent-opened-panes-open_browser)); the instructions prefer
  `browser_show` after the agent has used the browser.

## Limits and resources

- One own page per agent; one browser call at a time per agent.
- At most 6 app-engine agent views per daemon (`APP_VIEWS_MAX`); at most 4 Chromium pages.
- Own pages close after 10 idle minutes, when their agent ends, or when their window goes away
  (the reaper, every minute). The Chromium browser closes when it has no pages left and is
  killed by process group at daemon shutdown.
- Login-node daemons never launch Chromium: not in a Slurm job, Slurm detected, and the host not
  marked "not a cluster" (`compute::is_login_node`; an unasked daemon runs detection first).
- On Linux, the Chromium profile is refused on NFS, Lustre, GPFS, CIFS/SMB, BeeGFS, CephFS or
  PanFS (`statfs`); the refusal says to point `CHIMAERA_RUNTIME_DIR` or `XDG_RUNTIME_DIR` at a
  node-local directory.
- The window channel: per-socket queue of 64 frames, 256 requests in flight daemon-wide, a 15 s
  default deadline per request, replies capped at 8 MB. A closed socket fails its requests at
  once (`Gone`).

## Known gaps

- **macOS only for the app engine.** The Linux and Windows apps have no pane views; there the
  Chromium engine answers, or the refusal says what is missing.
- **Native `<select>`:** clicking one would open an NSMenu with a nested main-thread loop, so
  `browser_click` refuses it; use `browser_select`. A custom (drawn) listbox clicks normally.
- **No right-click**, ever (the same NSMenu problem). No context-menu path exists.
- **No HTML5 drag-and-drop:** injected input never starts a drag session (`dragstart` fires,
  `drop` never does). Pointer drags such as sliders work at the engine level; no tool sends a
  drag yet.
- **No file upload:** `<input type=file>` is not answered.
- **Throttling:** when the window is minimized or hidden, or the app is hidden, WebKit throttles
  the view (rAF stops, timers run 1–7 times a second). Everything still works, but debounced
  pages react slowly; results carry `· throttled`.
- **Focus:** an injected click makes the view first responder; the shell puts the window's
  previous first responder back after every action.

## Acceptance checks

- `cargo test -p chimaera-server agent_browser`: the end-to-end tests drive the tools over a
  real `/ws/events` socket against a stand-in window (`src/tests/agent_browser.rs`), plus the
  Chromium engine's unit tests (`agent_browser/chromium/tests.rs`).
- `cargo test -p chimaera-server a_plugin_tool_supplies_the_agent_browser`: `provides.browser`
  validation, the Can line, and each step of a plugin's browser (off here, on but not
  downloaded, installed, not runnable, removed) through the real tool-install route (`tests/plugin_jobs.rs`).
- `just browser-smoke`: the Chromium engine against a real browser
  (`agent_browser/chromium/live_tests.rs`; needs `CHIMAERA_TEST_CHROMIUM`).
- `just app-check`: the shell's `drive.rs` unit tests (parsing, key map, ledger, eval wrapper).
- `npm --prefix web-ui run test -- src/lib/browser`: the window half (`agentBrowser.test.ts`)
  and the wire fixtures (`net/wireFixtures.test.ts`).
- `cargo test -p chimaera-server a_file_opens_as_a_ticketed_page`: a file page end to end
  (refusals, the `/raw` page and its asset, `file://` URLs, reload after a rewrite, the
  hand-over refusal).
- Live (**verify-app**): in the macOS app, start a local web server, ask an agent to navigate,
  snapshot, click and type in it, and read its console; share a pane by the chain link, watch
  the marker, act in the pane and check that the agent is told; `browser_show` and check that
  the page does not reload. With no app window, check the Chromium engine answers (or refuses
  with the fix). On a host with no browser (an isolated daemon whose PATH has none), check the
  refusal names the Chromium plugin, then install it, switch it on, Install its tool, and check
  `browser_navigate` answers on `engine: chromium`.

## Status: partial

- The app engine runs on macOS only. Linux and Windows pane views, file upload and a live view
  of a Chromium page are not built.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why agents can see and drive pages
_Captured 2026-11-05 (the maintainer, in the session that built it; the design decisions are on issue #273)._

- **Problem it solves:** agents check their own web work. An agent building or fixing a web UI (a dev server, a notebook, a dashboard) sees and drives the page itself, inside Chimaera, instead of guessing or running a headless script the user never sees.
- **How settled it is:** all provisional. Only the why is settled; every piece can change if something works better, including the engine split, the tool set, the snapshot format and the pane chrome. The maintainer's #273 decisions (allowlist only, sharing through the chain link, eval allowed, user input pauses the agent, the app engine by default and Chromium otherwise) are the current design, not frozen contracts.
- **Page reloads keep a share:** the maintainer asked that a shared pane stay shared when its page reloads because the agent changed the app's code. A share is keyed by the browser tab, so in-pane reloads and hot reloads never end it, and since 2026-11-05 neither does a reload of the Chimaera window.
- **Do not change (or: open to change):** open to change; ask before weakening the navigation allowlist, which is the #273 Q1 decision.

### Why an agent can open a file as a page
_Captured 2026-11-05 (the maintainer, in the session that built it; the questionnaire was left at its defaults)._

- **Problem it solves:** an agent that wrote an HTML report and wanted to look at it started a local web server, because the browser tools took only http addresses. The maintainer's reaction: it shouldn't need to serve anything, it can use the browser tool.
- **How settled it is:** all open to change, including the allowed file types, the ticket lifetime and how `browser_show` treats a file page.
- **Deliberately left out or open:** nothing in particular.
- **Do not change (or: open to change):** open to change.

### Why a plugin can download the Chromium
_Captured 2026-11-05 (the maintainer, in the session that built it; the opt-in download was planned in #273, and the maintainer chose a separate plugin over a built-in setting button)._

- **Problem it solves:** the agent browser on cluster jobs and remote Linux hosts that have no Chrome or Chromium, where the user can't install one.
- **How settled it is:** all open to change: the pinned version, serving only workspaces where the plugin is switched on, being used only when the host has no browser of its own, and being a separate plugin rather than built in.
- **Deliberately left out or open:** nothing in particular; it is kept simple.
- **Do not change (or: open to change):** open to change.
