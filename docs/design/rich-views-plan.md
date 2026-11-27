# Rich views: agents' answer views, plugin web views, and GenomeSpy

> **Dated design record (2026-11-26).** No code has shipped.
>
> **Maintainer decisions, the same day:**
>
> - Plugins may ship HTML and JavaScript as web views inside Chimaera's own
>   screens. This amends
>   [platform decision 2](plugin-platform-plan.md#decisions-maintainer-2026-09-29);
>   see [the amendment](#the-amendment-to-platform-decision-2).
> - Agents' views in an answer use Chimaera's own elements first. They embed a
>   plugin's web view only where that plugin is active, and an HTML report only
>   as a last resort.
> - The answers to the four earlier questions are in [decided](#decided).
>
> **Still open:** the items in [open questions](#open-questions).
>
> **Sources:**
>
> - GenomeSpy facts: its docs and npm at v1.3.0 (2026-11-04).
> - Chimaera facts: checked against this tree at `f22fb7d2`, with file
>   references.
> - Items marked *spike* are unverified and are the first things to test.

## Why

There are two needs here, and one mechanism serves both.

**Agents make things to look at and use.** Examples: an explainer while you
learn something, a comparison of runs, a quiz, a results summary with its
figures, or a genome view of the variants it just called. Today an agent shows
these in two ways:

- **Embed cards.** Written files appear inline in an answer as embed cards
  (`![caption](path#fragment)`): a figure, a PDF page, a table slice, a sandboxed
  HTML report ([chat mode](../features/chat-mode.md#inline-artifacts),
  [embed cards](../features/files-and-previews.md#embed-cards)).
- **Hand-written HTML.** When an agent wants something richer it writes HTML.
  The page looks unlike Chimaera, follows neither theme, cannot reach workspace
  data from its sandbox, and cannot send anything back to the agent.

**Some views need a real library.** Nearly every scientific visualization
library is JavaScript: [GenomeSpy](https://genomespy.app/), IGV.js, Vega, Mol\*,
Plotly and map engines. Platform decision 2 ("no plugin HTML, script or
stylesheet anywhere") would make each one a viewer built into core, which works
against [platform decision 1](plugin-platform-plan.md#decisions-maintainer-2026-09-29):
core grows only generic pieces.

GenomeSpy (MIT, `@genome-spy/core` 1.3.0) is the first library worth building
for:

- **What it is.** A WebGL genome browser driven by a JSON **spec** in a grammar
  much like Vega-Lite's.
- **What it reads.** Indexed files in pieces with HTTP Range requests: BAM,
  BigWig and BigBed, indexed FASTA, and tabix-indexed VCF and GFF3. It reads
  TSV, CSV, JSON and Parquet whole.
- **Why it fits agents.** Its API was designed with them in mind:
  - `getSpecErrorLocation` locates a spec error as a JSON pointer.
  - `view.data.read()` and `view.data.transform()` are capped queries of the
    loaded data, "designed primarily for agentic use" (v1.2.0 notes).
  - `dataLoading` reports which sources loaded or failed.
  - `imageExport` returns PNG or SVG.

## The layering

Anything an agent or a plugin shows is built from three layers. Each one is
used only when the one before it cannot express the result:

| Layer | What it is | Who makes it | Safety |
|---|---|---|---|
| 1. **Chimaera's own elements** | A `ui/1` tree: text, markdown with math, headings, cards, tabs, sections, key–value lists, tables, lists, callouts, badges, code, diffs, images and SVG, PDF pages, slides, buttons and forms. These are the same nodes plugins already return, drawn by core. | Agents, as view files. Plugins, as screens. | Data only. It is checked by the host and drawn by core's own components. |
| 2. **A plugin's web view** | A `web` node: a page from an **active** plugin's bundle, such as GenomeSpy's genome view, with the data it needs. | Plugins on their screens. Agents inside their view files, but only where that plugin is on. | It runs in a sandbox with no origin and only the plugin's own file access and declared hosts ([below](#part-2-plugin-web-views)). |
| 3. **An HTML report** | An agent's own HTML file, embedded as today. | Agents. | Sandboxed. It cannot read workspace data or reply to the agent. This layer is unchanged and is the last resort. |

All three use **one renderer** (`plugins/ui/UiNode.svelte`), **one validator**
(`plugins/screens.rs` `check_tree`) and **one sandbox for web code**. A new node
added for plugins is available to agents at once, and a new viewer plugin's page
can be used in agents' answers. Claude Mods stay as they are. They are Claude's
own unversioned protocol (`docs/features/claude-mods.md`). `ui/1` is Chimaera's
format and works the same for every agent Chimaera runs.

## Part 1: views in an answer, in Chimaera's own elements

### What the agent writes

An agent's view is a **view file**: `*.view.json`, containing a `ui/1` tree,
the same shape a plugin's screen has:

```json
{"ui": "1", "title": "How a VCF is read",
 "root": {"type": "stack", "children": [
   {"type": "markdown", "text": "A **VCF** row is one site. Columns 1–8 describe it; from 9 on, one column per sample…"},
   {"type": "tabs", "tabs": [
     {"title": "Example row", "children": [{"type": "code", "language": "text", "text": "chr17\t7674220\t.\tC\tT\t…"}]},
     {"title": "Fields", "children": [{"type": "table", "columns": [{"key": "f", "title": "Field"}, {"key": "m", "title": "Means"}],
                                       "rows": [{"f": "POS", "m": "1-based position"}, {"f": "GT", "m": "genotype, 0/1 = het"}]}]}]},
   {"type": "form", "action": "send", "submit": "Check my answer",
    "children": [{"type": "select", "name": "a", "label": "Which column holds the genotype?", "options": ["INFO", "FORMAT + sample", "QUAL"]}]},
   {"type": "web", "plugin": "genomespy", "entry": "genome.html", "label": "The site on hg38",
    "data": {"spec": "learn/vcf-site.genomespy.json"},
    "fallback": {"type": "image", "src": "learn/vcf-site.png", "alt": "The site on hg38"}}]}}
```

- **Why a file and not text inside the message.** A file lasts. It can be
  edited and opened in a pane, and it is kept in git. The agent writes it with
  its own Write tool, and the user can edit it as JSON. Embed cards already
  work this way, and an embed names one file.
- **Where it shows.** It appears in the answer with
  `![How a VCF is read](learn/vcf.view.json)`. This is the existing embed card,
  given a new body kind for this file type. It also opens in its own pane, in
  markdown documents, and in hover previews.
- **What is allowed in it.**
  - **Paths:** workspace paths only. `output:` paths are refused, because a view
    file has no plugin.
  - **Actions:** only built-in actions, plus `send` (below).
  - **Web views:** a `web` node only when its `plugin` is active here. Anywhere
    else it draws its `fallback`, or "Turn on GenomeSpy to see this view" with
    the existing **Turn on** offer (`plugins/platform.ts` `offersFor`).
- **Errors.** The tree is checked by the same validator as plugin screens. A bad
  one shows "could not draw" with a list of what to fix, giving each problem's
  JSON path. The new `view_check {path}` tool gives the agent the same list
  ([open question 1](#open-questions)).

### Interactions with no plugin behind the view

A plugin screen sends its actions to the plugin. An agent's view has no code
behind it, so its interactions are of three kinds:

- **Kept in the window.** Tabs, collapsible sections, and reveal-on-click (a
  `section` that starts collapsed hides a quiz answer). These need no round trip.
- **Back to the agent.**
  - `ask-agent` (it exists already) puts a reference in the composer as a draft.
  - The new built-in `send` posts the form's values as a **user message** to the
    chat whose answer holds the view. It is shown in the transcript as coming
    from the view ("From *How a VCF is read*: a = FORMAT + sample"), and it runs
    only on the user's click.
  - Together these make a learning loop: the agent asks, the user answers in the
    view, and the agent replies.
- **In the workbench.** `open-file`, `open-url` (the system browser) and `copy`.

A view sends nothing on its own. Every `send` is one click by the user, so an
agent's view can never act as the user.

### What agents are told

The chat host frame (`launcher::CHAT_HOST_PROMPT`, `launcher.rs:1414`) gets one
more short paragraph. When a result is something to look at or use, write a
`*.view.json` from Chimaera's elements and embed it. Use a plugin's web view
inside it only when that plugin is active here. Write HTML only when neither
can express the result. The full list of nodes lives in a guide the agent reads
on demand, so the always-on text stays short.

## Part 2: plugin web views

### The amendment to platform decision 2

Decision 2 suits screens like lists, forms and settings, and it still governs
them. It does not suit a drawing area: a genome track, a molecule or a map is
drawn by an existing JavaScript library. The rejected alternative was a core
viewer per library. That keeps the rule on paper but moves every plugin's domain
into core.

| Decision 2 protected | How web views keep it |
|---|---|
| **A consistent look** | Only the drawing area is web. Everything around it stays `ui/1`. The page gets Chimaera's tokens, fonts and a small stylesheet for in-page controls (`chimaera-web.css`), and theme changes arrive live. The guide says web is for drawing areas, and the lock review checks this for first-party plugins. |
| **Safety by construction** | The page has no origin and cannot reach the window, the token, the daemon's API or other panes. It reads only what its plugin's `[access]` allows, and reaches only the network hosts it declared. It cannot open popups, navigate the top window, submit forms, download files or open modals. The browser enforces all of this. |
| **Works over a tunnel** | The bundle, the data URLs and the bridge are all served from the daemon's origin. |
| **Evolves without breaking** | `web/1` is versioned like `ui/1`. A host that does not know `web` draws the node's `fallback`. |
| **Accessible by construction** | Partly: canvas libraries never were. The node's `label` is required, and the elements around it stay core's. |

### The `web` node

```json
{"type": "web", "entry": "genome.html", "id": "tp53", "label": "Genome view of TP53",
 "data": {"spec": "results/tp53.genomespy.json", "locus": "chr17:7668402-7687550"}, "key": 3,
 "fallback": {"type": "callout", "text": "This chimaera cannot draw this view."}}
```

`plugin` is implied on a plugin's own screen and is required in an agent's view
file.

- **Props.**
  - `entry` is a page in that plugin's bundle.
  - `data` is JSON, at most 64 KiB, that the page reads as `chimaera.data`.
  - A new `key` delivers updated `data` to the running page without a reload,
    as `DeckNode`'s `start`/`startKey` does.
  - `id` names this instance so its plugin can call it.
  - `label` is required.
- **Layout.** It fills its slot as `pdf` and `slides` do. In an embed card it has
  a fixed height and an expand control, as HTML cards have today.
- **Where it runs.** The page runs where HTML reports already run:
  - In the browser, an iframe with `sandbox="allow-scripts"`. It never gets
    `allow-same-origin`, popups, forms, top navigation or modals.
  - In the app, a native pane view in its own web process. That view already
    handles geometry, overlaps, crashes, parking, moves, loading, embedding in a
    scrolling document, and snapshots (`shared/paneView.ts`).
- **The page's CSP.**
  - `default-src 'none'`.
  - Scripts and styles come only from its bundle. `'unsafe-eval'` and
    `'wasm-unsafe-eval'` are allowed: a page with no origin and no network gains
    nothing from eval, and Vega-style libraries compile expressions.
  - `img-src` and `media-src`: its bundle and data URLs, plus `blob:` and `data:`.
  - `connect-src`: only its data URLs and declared hosts.
  - `worker-src`: `blob:` and its bundle.
  - *Spike:* the minimum GenomeSpy needs.
- **Resource limits.**
  - At most 4 web views run per window; the least recently used one is closed
    first.
  - A parked view shows its last snapshot and is closed after 2 minutes. The
    native view already hides while parked.
  - A hidden page gets `visibility: hidden`, and the guide requires it to stop
    drawing. A native pane view in its own process can also be suspended.

### The bundle ships inside the plugin

The page's files are part of the plugin's download and never part of Chimaera.
A user who never installs GenomeSpy never downloads a byte of it.

- **How the plugin hands them over.** API **0.3** adds one export,
  `web-bundle() -> list<u8>`. It returns a tar of the page's files, which the
  plugin embeds with `include_bytes!` into its own `plugin.wasm`.
- **How the host serves them.** The host unpacks it once per installed version,
  with the same safe unpacker as tools: no absolute paths, no `..`, no links
  (`plugins/toolchain.rs`). The files land in
  `~/.chimaera/plugins/<id>/<version>/web/` and are served from there under a
  ticket.
- **Size and integrity.** GenomeSpy's whole `dist/bundle` is about 2.9 MB raw,
  which fits under the 16 MiB component cap. The lock already pins the
  component's sha256, and the maintainers' review covers what it contains. That
  means no new release file, checksum or workflow.

The manifest declares the pages and the hosts:

```toml
[web]
pages = ["genome.html"]
network = ["data.genomespy.app", "hgdownload.soe.ucsc.edu"]   # https only; each one on the card
network_user = true      # the user may add hosts in the plugin's settings
```

### Data URLs

`chimaera.url(path)` asks the host for a URL to a workspace or `output:` file.

- **Within the plugin's access.** The host refuses unless the plugin has
  `[access] files = "read"`. It also refuses paths that leave the workspace or
  go through a link, exactly as `host::read` does.
- **The route.** The host answers with `/webdata/{ticket}`. This is the same
  streamed, single-range serving as `/raw` (`fs.rs:5349-5406`), plus
  `Access-Control-Allow-Origin: *` and an `OPTIONS` answer for Range
  preflights.
- **Why opening CORS is safe here.** The ticket is the authority: only this page
  asked for it, and it names one file. HTML reports keep their no-CORS rule
  (`fs.rs:5068`).
- **Keeping the URL alive.** A ticket lives 10 minutes from its last mint. Asking
  again for an unchanged file returns the same URL and extends it (`fs.rs:4783`),
  and the bridge does this while the view is visible.
- **No bytes through the bridge.** The page uses plain `fetch`, so a library's
  own readers work unchanged (GenomeSpy's BAM, BigWig and tabix readers). A 50 GB
  BAM works, over an SSH tunnel too.
- *Spike:* Range requests from a frame with no origin, in WKWebView, WebKitGTK
  and Chrome.

### The bridge, `web/1`

The page includes the host's `chimaera-web.js`. In the browser it talks over
`postMessage`, and in the app over the existing pane bridge
(`chimaera_core::PANE_BRIDGE_JS`, `native_view.rs`). Messages are JSON, at most
64 KiB each, and rate-limited.

| Call | Does |
|---|---|
| `chimaera.data`, `onData(fn)` | The node's `data`, and new data when `key` changes. |
| `chimaera.theme`, `onTheme(fn)` | `{mode, tokens, fonts}`. `chimaera-web.css` exposes the tokens as CSS variables and styles in-page buttons, inputs and toolbars like core's. |
| `chimaera.visibility`, `onVisibility(fn)` | Whether the page is visible. The page stops drawing while hidden. |
| `url(path)` | A data URL ([above](#data-urls)). |
| `act(action, payload)` | The plugin's `on_action`, or a built-in action. In an agent's view file, only the built-ins and `send`. |
| `reference({text, file?, line?, crop?})` | Points an agent at the user's selection. It becomes a draft in the composer with the crop attached, as with the image area tool (`ImageView.svelte:272`). It is never sent on its own. |
| `save(name, blob)` | Writes into the plugin's output folder, for exports and pictures. |
| `handle(method, fn)` | Answers calls from the page's plugin ([next](#calls-from-a-plugin-or-an-agent-to-its-page)). |
| `status({state, text})` | One status pill in the view's bar: loading, ok or errors. |

### Calls from a plugin, or an agent, to its page

```
platform::web_call(cx, view, id, method, args) -> call id
ToolResult::wait_call(call_id, "rendering…")   → tool_resume(cx, name, result)
```

- **Who answers.** The call goes to the window showing that view, the most
  recently focused one in the workspace. If none shows it, a **headless host**
  answers: the daemon's Chromium engine (`agent_browser/chromium.rs`) loads the
  same page in a render-only shell, with the daemon at the other end of the
  bridge. `fresh: true` forces the headless host.
- **The channel.** It reuses the agent browser's window request channel
  (`agent_browser/channel.rs`, `browser_request` / `browser_reply`).
- **Limits.** A call waits at most 45 s. Answers are at most 64 KiB, and
  pictures go through `save` and come back as `![](output:…)`
  (`plugins/tools.rs:96`).
- **Login nodes.** On a Slurm login node the headless host refuses, as
  `chromium.rs:197` does today. On a cluster workspace it runs inside the job.
- **A built-in snapshot.** Every web view also answers
  `snapshot {width, height}` without page code (a pane-view snapshot or a CDP
  screenshot).

### Reveal: an agent opens a view next to itself

`browser_show` already hands a page to the user as a pane beside the agent
(`agent_browser/tools.rs`, its hand-over rules). `platform::reveal(cx, {view |
file, args})` gives plugins the same hand-over, using the same code: it opens a
plugin view or a claimed file beside the calling session's pane. The rules:

- only from a session's own call;
- at most 4 a minute per session;
- it never takes focus from typing;
- it is recorded in the plugin's activity log.

### References matched by pattern

`references/1` today makes chips only for ids the plugin lists, at most 5,000.
That cannot cover loci or gene names. The change adds an optional
`view: {view, args_from_match}` to a shape, so a match opens the plugin's view
with the matched text, for example `{"locus": "<match>"}`. The limits on shapes
stay as they are.

### Smaller additions

- **`host::read_at(path, offset, cap)`.** `host::read` reads only a file's first
  bytes. BigWig chromosome trees and very large BAM headers can lie past 8 MiB.

Double extensions (`*.vcf.gz`, `*.genomespy.json`) already work. A pattern
without `/` is matched against the whole file name (`matchesPattern`,
`platform.ts:208`; `platform::matches`, `platform.rs:664`).

### Trust

- **The card's Can list** shows "Draws its own views (web pages in a sandbox)",
  then one line per declared host ("Reaches data.genomespy.app"), then "You can
  let it reach other sites" when `network_user` is set.
- **Tier.** Web views alone keep a plugin **sandboxed**: the browser enforces the
  isolation, and the page's file access is the plugin's own.
- **Verified plugins.** A verified (first-party) plugin's declared hosts are
  trusted by default. The lock review covers them and nobody is asked.
- **Unverified plugins.** Their hosts are listed in the existing trust prompt.
- **Hosts the user adds.** Adding a host in the plugin's settings is itself the
  act of trust, so it does not prompt again.
- **Updates.** A release that declares a new host changes the plugin's
  capability digest, the same as a wider `[access]`.
  - For a verified plugin, the lock review covers it again.
  - For any other plugin, its user is asked again.

## Part 3: GenomeSpy, the first web-view plugin

### What the user sees

**A spec file, `*.genomespy.json`:**

- **The layout.** The editor (core's own) is on the left and the genome view
  (`web`) on the right. Above them are a **Split · Source · View** switch and a
  status pill such as `hg38 · 4 tracks · 2 loading`.
- **Live redraw.** Saving, or an agent writing the file, redraws the view and
  keeps the current region (`file-changed` gives a new node `key`, and the page
  re-embeds and zooms back).
- **Problems are editor marks** on the right line, with **Ask agent**. They come
  from two places:
  - the plugin's checks, for example `sample.bam has no .bai beside it`, or
    `this VCF names chromosomes "1, 2, …" but the view is hg38: set addChrPrefix`;
  - GenomeSpy's own errors, for example
    `Invalid field "positon". Available fields: position, value`.

  The page posts errors with their JSON pointers. The plugin, which holds the
  text, maps each one to a line and publishes `diagnostics/1`. A broken spec
  keeps its last good picture under the error.
- **Loading per track** comes from `dataLoading`. A source that fails names its
  file and does not take down the rest of the view.

**Data files open as a genome view.** These are `.bam`, `.bw`/`.bigwig`,
`.bb`/`.bigbed`, `.vcf.gz`, `.gff3.gz` and `.fa`/`.fasta`. Today all of them,
except `.vcf.gz` and `.gff3.gz` which open as tables, show only a binary info
card (`previews/files.ts:1046`).

- **What each format shows.** The plugin reads the file's header in its sandbox
  and builds a spec:

  | File | View |
  |---|---|
  | BAM | Coverage, with reads below 20 kb (`multiscale`). A BigWig beside it is used for coverage when there is one. |
  | VCF | Variants coloured by type, with genotypes when there are fewer than about 50 samples. |
  | BigWig | A signal track. |
  | GFF3 | Gene models. |
  | FASTA | The sequence at base-pair zoom. |

- **It identifies the reference genome.** A position such as `chr1:1,000,000`
  means something only on one reference build: hg38, hg19, mm10 and so on.
  - **How it tells.** Files rarely name their build, but the header lists each
    chromosome's name and length, and the lengths identify the build. For
    example, chr1 is 248,956,422 bp in hg38 and 249,250,621 bp in hg19.
  - **What it does.** The plugin matches the header against the builds GenomeSpy
    knows. If none matches, it builds the chromosome list from the header.
  - **What it reports.** It always says which build it chose and why.
  - **When files disagree** it warns instead of drawing one over the other. Data
    on the wrong build lands in the wrong place, with no error at all.
- **Make editable.** This writes the generated spec to
  `output:<name>.genomespy.json`. `save-to-workspace` then copies it beside the
  data, asking before it replaces a file. From then on that file is the one the
  user and agents share.
- **Text formats keep their table view.** `.vcf`, `.bed`, `.bedgraph`, `.gff`,
  `.narrowPeak` and `.tsv` stay in core's table view (`TABLE_PRESETS`,
  `previews/files.ts:551`). They get a **View on genome** file action instead.
- **Missing indexes.** The plugin looks for `.bai`, `.csi`, `.tbi`, `.fai` and
  `.gzi` beside the data. When one is missing, a callout names it and offers
  **Ask agent to index it**. The plugin never runs a program itself.
- **Unsupported formats.** It claims `CRAM` and `BCF` only to say that GenomeSpy
  1.3 reads neither.

**The page's own controls** stay in the page, so typing a locus or zooming never
waits on the daemon. They are styled with `chimaera-web.css`.

- **Locus field.** It accepts `chr17:7,668,402-7,687,550`, a chromosome, or a
  gene name when the spec has a searchable annotation track. It keeps a
  back/forward history.
- **Zoom.** Zoom out, zoom in and fit. On the keyboard: arrows pan, `+`/`-` zoom,
  `/` focuses the locus field, and Esc clears the selection.
- **Export.** PNG or SVG through `imageExport`, then `save` and
  **Save to workspace**.
- **@.** Sends the region, the visible tracks and any brushed selection, with a
  crop, through `reference`.
- **Theme.** Chimaera's tokens are mapped onto GenomeSpy's `theme` and
  `.gs-tooltip`. A theme change re-embeds the view at the same region.
- **Narrow panes.** The controls collapse to the locus field and a "…" menu.

**Loci in chat become chips** through pattern references. Hovering one shows a
preview, and a click opens the view at that region.

**In an agent's view file**, a `web` node with `"plugin": "genomespy"` draws a
genome view inside the answer: for a lesson, a results summary or a variant
report.

### What agents get

Tool names are prefixed `genomespy_`. The always-on instructions are short, and
`genomespy_guide` is read on demand. GenomeSpy's own
[app-agent](https://github.com/genome-spy/genome-spy/tree/master/packages/app-agent)
work found the same: keep the prompt small, read docs on demand, and never send
raw data up front.

| Tool | Needs | Does |
|---|---|---|
| `genomespy_guide {topic?}` | nothing new | The grammar for the GenomeSpy version the plugin bundles. It covers tracks per data type, multiscale, assemblies, loci and the coordinate rules, and how to put a genome view in a `*.view.json`. |
| `genomespy_inspect {path}` | nothing new | What is in a data file, read from its header: the format, the contigs with lengths and `chr` naming, the reference build it matches and how sure that match is, samples and read groups, INFO and FORMAT keys, zoom levels, indexes present or missing, and a ready-made track snippet. The headers are parsed in pure Rust over `host::read`. |
| `genomespy_check {path}` | nothing new, then calls | It checks the spec's shape, the paths, the indexes, chromosome naming against the build, and field names against TSV and VCF headers. With calls, it also runs GenomeSpy's own validation in a real embed. The answer lists problems with their lines, and the same problems appear as editor marks. |
| `genomespy_look {path, locus?, width?, height?}` | calls | A picture of the view in its answer, plus the visible region, the rows loaded per track, and any sources that failed. When the user has the view open, it renders from their window, so the agent sees what they see. Otherwise it renders headless. |
| `genomespy_query {path, view, locus?, transform?, limit?}` | calls | Runs `data.read` or `data.transform` (filter, aggregate, window) with a row cap. |
| `genomespy_where {}` | calls | The genome views open here, each one's region, and any brushed selection. |
| `genomespy_show {path, locus?}` | reveal | Opens or focuses the view next to the calling agent's session, or moves the user's open view to a region. |

A typical loop:

1. `inspect` the files.
2. Write `results/tp53.genomespy.json` with the agent's own Write tool.
3. `check` it.
4. `look` at it.
5. Fix any problems.
6. `show` it, or embed it in a `*.view.json` in the answer.
7. Later, `where` and `query` answer questions about the user's selection.

### The manifest

```toml
id = "genomespy"
name = "GenomeSpy"
category = "data"
api = "0.3"
[requires] chimaera = ">=<the release with web views>"
[access] files = "read"
[provides] mcp_tools = ["genomespy_guide", "genomespy_inspect", "genomespy_check", …]
           events = ["file-saved", "file-changed", "settings-changed"]
[web] pages = ["genome.html"]
      network = ["data.genomespy.app", "hgdownload.soe.ucsc.edu", "ftp.ensembl.org"]
      network_user = true
[[views]] id = "spec"    slot = "file"  title = "Genome view"
[[views]] id = "data"    slot = "file"  title = "Genome view"
[[views]] id = "browser" slot = "tab"   title = "GenomeSpy"   # every genome view here + recent regions
[[files]] match = ["*.genomespy.json"]                                     view = "spec"
[[files]] match = ["*.bam", "*.bw", "*.bigwig", "*.bb", "*.bigbed",
                   "*.vcf.gz", "*.gff3.gz", "*.fa", "*.fasta", "*.fa.gz"]  view = "data"
[[actions]] match = ["*.vcf", "*.bed", "*.bedgraph", "*.gff", "*.gff3", "*.narrowPeak"]
            label = "View on genome"  action = "view-on-genome"
[[settings]] max_samples (50) · reads_below_bp (20000)
[recommends] the Chromium plugin, for agents' pictures on hosts with no Chrome
```

### The repository

- **Rust (`src/`).** All of it is pure and natively tested on real header bytes:
  - header parsers for BAM `@SQ`/`@RG`, VCF `##contig`/`#CHROM`, BigWig and BigBed
    chromosome trees, `.fai` and `.tbi` names;
  - build matching against GenomeSpy's assembly tables, vendored with their
    source cited;
  - spec templates;
  - JSON pointer → line mapping;
  - the checks, the guide and the tools.
- **The page (`web/`).**
  - `genome.html` and a pinned, self-hosted `@genome-spy/core`, with no CDN.
  - `genome.js` holds the embed (paths mapped through `url()`, index URLs
    explicit), the theme mapping, error posting, the in-page controls,
    `reference`, and the call handlers (`png`, `domain`, `zoom`, `selection`,
    `read`, `transform`, `errors`, `loading`).
  - CI builds it into the tar that `web-bundle()` returns.
- **Upgrades.** A GenomeSpy upgrade is a plugin release, not a Chimaera release.
  The plugin is sandboxed, so a bump that asks for nothing new auto-merges into
  the lock.

## Phases and acceptance

Every phase is verified live in the isolated preview, the macOS app (WKWebView)
and Chrome:

- light and dark;
- narrow and wide panes;
- the keyboard;
- a remote workspace over the throttled-tunnel setup.

Test data are small committed files generated from public data, under about 5 MB
in total.

0. **Spike: about 3 days, then thrown away.** A GenomeSpy page, served from a
   folder laid out like a plugin's, in a frame with no origin, a native pane view
   and headless Chromium. It tests:
   - Range `fetch` to a CORS ticket route, with and without a preflight;
   - the CSP GenomeSpy needs;
   - idle CPU;
   - working offline, including where the WebGL font atlas loads from;
   - bundle size;
   - whether an untrusted spec can turn text into HTML (tooltips, `text` marks,
     `description`).

   It ends with a go/no-go note in this record.
1. **Answer views in Chimaera's elements.** This needs no web code and can ship
   before phase 2.
   - `*.view.json` as an embed body and as a pane view, with the same renderer
     and validator.
   - Window-only interactions, plus `send` with its transcript attribution.
   - The paragraph in the host frame, and `view_check`.
   - Acceptance: an agent asked to explain a file format answers with a view
     that has tabs, a table and a quiz. The user's answer comes back as an
     attributed message, and the agent replies. The view is correct in light,
     dark, narrow and wide.
2. **The web views platform.**
   - The `web` node, API 0.3 `web-bundle()`, `/webdata` and `url()`.
   - `web/1`: data, theme, visibility, act, reference, save and status.
   - CSP built from `[web]`, the Can list and the capability atoms, and the
     resource limits.
   - `plugins/test-web` and `tests/plugin_web.rs`, plus the guide's section and
     the feature page.
   - Acceptance:
     - the fixture's page draws in both hosts and in an agent's view file;
     - it reads a workspace file by range;
     - it is refused outside its access, at an undeclared host, and where its
       plugin is off;
     - it follows the theme live;
     - its @ lands a draft with a crop.
3. **GenomeSpy 0.1.**
   - Specs and data files open; Make editable; View on genome; diagnostics.
   - `guide`, `inspect` and `check`.
   - Acceptance:
     - given a BAM and a VCF, an agent inspects them and writes a spec that opens
       correctly the first time;
     - the user edits it live;
     - a missing index, a `chr` mismatch and a typo in a field each read
       plainly.
4. **Calls and the headless host.** `look`, `query`, `where`, and the `check`
   that runs GenomeSpy.
   - Acceptance:
     - With the view closed, `look` returns a correct picture within 10 s.
     - On a login node it refuses plainly.
     - When the user brushes a region and asks "what's in here?", the agent
       answers from `where` and `query`.
5. **Reveal and locus chips.**
6. **Later, if wanted.**
   - GenomeSpy's sample-collection App (cohorts, provenance) as a second page.
   - A privileged opt-in plugin for indexing and bgzipping.
   - Other visualization plugins on the same platform: Vega-Lite (charts in
     agents' views, [open question 2](#open-questions)), Mol\*, maps.

**Shipping.** Phases 1 and 2 are `feat:` PRs to Chimaera. Each needs the ship-pr
gates, the feature pages, the guide, and wire fixtures for any new frames.
Phase 3 starts the plugin's repository with CI and release, then a lock entry
with `tier = "sandboxed"`.

## Risks

- **Third-party code in the user's window.** The defences:
  - the page has no origin and runs under a CSP;
  - its data is limited to the plugin's access;
  - it reaches only the hosts it declares;
  - the lock reviews each first-party bundle;
  - the spike reviews every path where spec text could turn into HTML.
- **Agents' views are untrusted output.** A view is data checked by the host and
  drawn by core. Markdown in it is sanitized as in chat. A web node inside it
  runs only an active plugin's page, with that plugin's limits. A view never
  sends anything without the user's click.
- **A page that ignores visibility** can keep a browser CPU busy. The native view
  can be suspended. For first-party plugins, the fixture test checks CPU and
  the lock review checks the page.
- **Look drift inside pages.** `chimaera-web.css`, the guide's rule and the lock
  review limit it.
- **Platform cost.** Phase 2 spans the daemon, the UI, the app's pane bridge and
  the WIT. It is paid once, for every visualization plugin after this one.

## Decided

1. **Plugin web views**, rather than a core viewer per library.
2. **The bundle ships inside the plugin**, embedded in its `plugin.wasm`, and
   never in Chimaera. Only people who install the plugin download it.
3. **Network hosts.** A verified plugin's declared hosts are trusted by default
   (the lock review covers them). Hosts a user adds in the plugin's settings are
   trusted because the user added them. An unverified plugin's hosts appear in
   its trust prompt.
4. **Reveal reuses `browser_show`'s hand-over**: the agent opens the view beside
   its own session, without taking focus.
5. **Spec files are named `*.genomespy.json`.**
6. **Answer views use Chimaera's elements first** (a `*.view.json` `ui/1` tree),
   a plugin's web view only where that plugin is active, and HTML only as a last
   resort.

## Open questions

1. **A `view_check` tool for every agent.** It would be a built-in MCP tool
   beside the browser tools, answering a view file's problems with their JSON
   paths. Without it, an agent sees a broken view only if the user says so.
   *Recommended: yes*, as one tool with a short description.
2. **Charts in agents' views.** `ui/1` has tables but no charts, and "a chart of
   the result" is the most common thing a view would want. There are two routes:
   - a first-party Vega-Lite plugin on web views, which needs the plugin to be on;
   - a small `chart` node in core, which every view could use.

   *Recommended: the Vega-Lite plugin*, which keeps to the layering. Revisit if
   charts turn out to be needed in most answers.
3. **The view file's name.** `*.view.json` is short and says what it is. The
   alternative is `*.ui.json`. *Recommended: `*.view.json`.*

## Sources

- **GenomeSpy**
  - [embedding](https://genomespy.app/docs/api/embedding/)
  - [EmbedResult](https://genomespy.app/docs/api/reference/interfaces/EmbedResult/)
  - [editor integration: feedback for LLM agents](https://genomespy.app/docs/api/editor-integration/)
  - [runtime state and selections](https://genomespy.app/docs/api/runtime-state/)
  - [lazy data sources](https://genomespy.app/docs/grammar/data/lazy/)
  - [genomic coordinates](https://genomespy.app/docs/grammar/genomic-coordinates/)
  - [releases](https://github.com/genome-spy/genome-spy/releases)
  - [genome-spy-python](https://genomespy.app/genome-spy-python/)
- **Chimaera**
  - [extension guide](../agent-guides/plugins.md)
  - [platform plan](plugin-platform-plan.md)
  - [PowerPoint plan](pptx-plugin-plan.md)
  - [chat mode: inline artifacts](../features/chat-mode.md#inline-artifacts)
  - [embed cards](../features/files-and-previews.md#embed-cards)
  - [Claude Mods](../features/claude-mods.md)
  - [agent browser](../features/agent-browser.md)
