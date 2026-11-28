# Web views

A plugin can ship its own HTML and JavaScript page for a drawing area that an existing JavaScript library draws: a genome track, a molecule, a map. An agent's page can read the files beside it and import an active plugin's library. Neither kind of page can send anything to a site it did not declare. Design: [rich views, Part 2](../design/rich-views-plan.md#part-2-plugin-web-views) and [the fixed rule for agents' pages](../design/rich-views-plan.md#what-a-page-can-reach-one-fixed-rule-nothing-for-the-user-to-choose). Writing one: [the plugin guide](../agent-guides/plugins.md#web-views-api-03).

**Where it lives (shared):**

- **Daemon.** `crates/chimaera-server/src/plugins/web.rs`: the `[web]` table, the bundle unpack, grants, data tickets, data keys, the CSPs, and the routes. Also `fs.rs` (`serve_page` for agents' pages, `serve_file`/`Serve`) and the `web` node check in `plugins/screens.rs`.
- **Interface.** WIT `chimaera:plugin@0.3.0` exports `web.web-bundle` (`crates/chimaera-plugin-api/wit/`; 0.2 is frozen in `wit-0.2/`). The SDK's `Plugin::web_bundle` defaults to empty. 0.3 also adds `host::read_at` (a workspace file from an offset, same limits as `read`), which genome headers past 8 MiB need.
- **Page side.** `chimaera-core`: `page_api.js` (`window.chimaera`, `web/1`), `pane_bridge.js` (relays `web_*` in the app's view; `command` hands the host's word to the page) and `chimaera_web.css` (`WEB_CSS`).
- **UI.**
  - `web-ui/src/lib/plugins/ui/WebNode.svelte` and `webViews.svelte.ts`: the node and the per-window limit.
  - `web-ui/src/lib/shared/guardFrame.ts`: the navigation guard.
  - `pageCrop.ts`, `pageNeeds.ts`, `paneFrame.ts`, `paneView.ts` (`PaneViewControl.command`) and `paneViewBridge.ts` (the `web_*` decoders).
  - `shared/embed/HtmlBody.svelte` and `previews/HtmlView.svelte`, for agents' pages.
- **App.** `crates/chimaera-app/src/shell/native_view.rs`: `native_view_command`, and `/web/` among a view's allowed paths.
- **Wire.**
  - `POST /api/v1/workspaces/{id}/plugins/{pid}/web {entry}` (bearer) answers `{url}`.
  - `POST /api/v1/fs/page-key {ticket}` (bearer) answers an agent page's `{key, needs}`.
  - The ticket routes: `GET /web/{grant}/{*rest}` (with `~url?path=`), `POST /web/{grant}/~save?name=`, `GET /webdata/{ticket}`, `GET /webdata/{key}/{*rest}` and `GET /lib/{ticket}/{*rest}`. Each also answers `OPTIONS`.

## A plugin's own page

- **What & when.** A plugin draws a drawing area with a JavaScript library, inside its own `ui/1` screen. The page fills its slot with no head or frame of the host's: its tab or screen names it, and its `label` is its accessible name. The screen's other nodes stay core's.
- **How it's used.**
  1. A 0.3 plugin declares `[web] pages`, `libraries` and `network`, and returns its bundle from `web_bundle()`.
  2. One of its views returns a `web` node: `entry`, `label`, `data`, `key`, `id` and `fallback`.
  3. Opening the view mints a grant and shows the page. In the macOS app the page runs in a native view of its own. Elsewhere it runs in a sandboxed frame inside the navigation guard.
  4. The page reads `chimaera.data`. It calls `act`, `status`, `reference`, `url` and `save`. It follows `onTheme` and `onVisibility`.
- **Key behaviors and constraints.**
  - **The bundle.** It is unpacked once per build, into `<output root>/.web/<id>-<sha16>/`. It holds files and folders only: no links, at most 64 MiB and 4,096 entries. Other builds' bundles are removed.
  - **The grant.**
    - A grant is a `w-` ticket for (plugin, workspace, build). It lives 10 minutes from its last mint. The node mints it again every 5 minutes while the node is shown.
    - Every request checks again that the plugin is on there (cached for 2 s, and cleared by the switch) and still runs that build. Otherwise the answer is 404.
  - **The page's CSP.**
    - It keeps `sandbox allow-scripts`.
    - Scripts, styles and fonts come from the daemon. `'unsafe-eval'` and `'wasm-unsafe-eval'` are allowed.
    - Images, media and requests go to the daemon and to the declared hosts only. Workers come from `blob:` only, so a worker keeps the page's own CSP.
    - The daemon is named twice: `'self'`, and its origin as the request reached it (`web::request_origin`: `Host`, and `X-Forwarded-Proto`). WebKit matches `'self'` against the page's own origin, which a sandboxed page doesn't have, so without the explicit origin the macOS app's view and Safari refuse every request to the daemon.
    - Forms are not allowed (`form-action 'none'`).
    - Every bundle file has `Access-Control-Allow-Origin: *`, so ES modules load.
  - **`url(path)`.**
    - It answers a `/webdata/d-…` data ticket: one file, served by Range with CORS. A ticket is per plugin and workspace, and each use renews its 10 minutes.
    - A workspace path needs `[access] files = "read"`. Without it the answer is 403.
    - A path never goes through `..` or a link. That is refused with the link named.
    - `output:` paths are always the plugin's own.
  - **`save(name, blob)`.** It writes `output:<name>`: at most 8 MiB, charged to the output quota.
  - **Host commands.** `data`, `visibility` and `theme` reach the running page without a reload: a `postMessage` command into a frame, or `native_view_command` into the app's view (a `chimaera:host` event).
  - **Status.** `chimaera.status` shows only while it says something: a small line under the page for `warn` and `bad`, and for `busy` once it lasts past 0.6 s. `ok` and `idle` show nothing.
  - **The cursor (the app).** A pointer move over a page's native view goes to that view alone (`mac.rs` `move_monitor`). Otherwise the window's own web view, whose tracking area covers the view, also set the cursor at each move, and it flickered between the two.
  - **Limits.** At most 4 live web views run per window. The least recently shown view closes first and keeps a still. A view hidden for 2 minutes closes.
- **How to extend it.** Add a `web/1` call in `page_api.js` and the host's handler (`WebNode.svelte` `onWeb`). Decode a new page event in `paneViewBridge.ts`, and gate it in `paneFrame.ts` and `pane_bridge.js`. Keep the [guide](../agent-guides/plugins.md#web-views-api-03) in step.
- **Acceptance checks.**
  - `cargo test -p chimaera-server plugin_web`.
  - `bash scripts/build-plugins.sh` (the `test-web` fixture).
  - Live: install `plugins/dist-test/test-web` by path and switch it on. Open **Web board**: the page draws with its data. **Read sample.csv** answers 206. **Fetch outside** is refused. **Leave the page** leaves nothing (the frame shows the browser's blocked page). **Ping the plugin** re-renders the screen, and the page gets the new data without a reload. A theme switch recolours it live. **@** drafts with a picture.

## An agent's page under the fixed rule

- **What & when.** An agent writes HTML (a plot, a lesson, a report). The page reads the files beside it, and it imports libraries from active plugins and the library CDNs. It sends nothing anywhere else. Nobody configures anything.
- **How it's used.**
  - **Reading a file.** The page calls `await chimaera.url("data.csv")` and `fetch`es the URL, by Range too.
  - **Importing a library.** It names `chimaera:lib/<name>/<file>` in a `<script src>` or an `import`.
  - **The app's look.** It links `chimaera:web.css`.
- **Key behaviors and constraints.**
  - **The CSP.** Every HTML page that `/raw` serves (the file, or the folder route) gets the fixed rule (`web::agent_page_csp`):
    - scripts, styles and fonts from the daemon and from `cdnjs.cloudflare.com`, `cdn.jsdelivr.net`, `unpkg.com`, `cdn.plot.ly` and `cdn.bokeh.org`;
    - images and media from the daemon, `data:` and `blob:`;
    - requests to the daemon only, plus the declared hosts of a plugin library the page imports;
    - no forms.

    An old report that loads outside images or calls an outside API no longer reaches them inside Chimaera. It still opens fully in the browser pane.
  - **Rewriting.** The daemon rewrites `chimaera:lib/` and `chimaera:web.css` to `/lib/{l-ticket}/…` (a library ticket of the page's own, renewed by each use), for a page of at most 4 MiB. At most 4 pages are read whole at once. A page that names neither is served byte for byte, with ranges.
  - **The data key.**
    - A host that shows the page inside its guard (`HtmlBody`, `HtmlView`) asks `POST /fs/page-key` with its bearer token, then puts the key in the page's URL (`chimaera-key`).
    - `url` then answers `/webdata/{key}/<path>`, for the page's folder and below: never `..`, a hidden name or a link.
    - A page loaded any other way (a browser pane, a direct visit, a page asking for itself) has no key and reads nothing. The folder route `/raw/{t}/{rest}` never sends CORS.
  - **Libraries.**
    - `/lib/{ticket}/<name>/…` serves a library only from a plugin that is on in a workspace holding the page.
    - Where the plugin is installed but off, the daemon names it in the same `page-key` answer (`needs`), never the page. The card or the preview then shows "This page uses <name>. Turn it on here", by the plugin's own name. Turning it on reloads the page.
  - **The navigation guard (browser, Linux, Windows).**
    - Hosts put the sandboxed frame inside a same-origin `srcdoc` wrapper whose CSP is `frame-src 'self'`. The browser checks the container's `frame-src` before the frame navigates, so `location = "https://…"` sends no request.
    - The wrapper relays `postMessage` both ways.
    - The macOS app's view needs no guard: its shell refuses the navigation itself.
  - **References.** `chimaera.reference({text, crop})` drafts with a picture: into the chat whose answer shows the page, else into the agent the person works with. It is honoured only after a press in the page.
  - **What the rule does not stop** (accepted, as in the design): a few bytes encoded in a library CDN's URLs, WebRTC (CSP has no directive for it), DNS prefetch, and in the app a link the person clicks opening in their browser.
- **Acceptance checks.**
  - `cargo test -p chimaera-server plugin_web` (`an_agent_page_imports_a_plugin_library_and_reads_the_files_beside_it`, `a_plain_report_keeps_its_bytes_under_the_fixed_rule`).
  - `cargo test -p chimaera-server embed`.
  - Live: in a workspace with `test-web` on, open a page that imports `chimaera:lib/testlib/lib.mjs`. It logs the module's greeting, and reading `data.csv` beside it answers 206. Asking for `../x` is refused, and an outside fetch is refused. Switched off, the page offers to turn it on.

## Key constraints

- **The grant, key or ticket is the authority.** No web route takes the bearer token. Each answers 404 once its plugin is off, its build changed, or it expired.
- **Plain words on the card.** Each page, library and host is a Can line, so a release that adds one asks again. Web views alone keep a plugin sandboxed.
- **No bytes through the bridge.** `url` and `save` go to the daemon directly under the page's own ticket. The bridge carries only small JSON: `act` and `status` at most 10 a second, `reference` crops under 200 KB, else a rect that the app's host takes a picture of.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Plugin web views and agents' pages that read files — why it exists

*Pending:* to be captured with the maintainer through **capture-feature-intent**.
