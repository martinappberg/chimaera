# Plugin web views, and GenomeSpy as their first plugin

> **Dated design record (2026-11-26).**
>
> - **Approved by the maintainer the same day:** plugins may ship HTML and
>   JavaScript, as web views inside Chimaera's own screens. This amends
>   [platform decision 2](plugin-platform-plan.md#decisions-maintainer-2026-09-29),
>   and the [amendment](#the-amendment-to-platform-decision-2) records why.
>   GenomeSpy is the first plugin built on it.
> - **Still open:** the rest of this record, including the
>   [decisions](#decisions-for-the-maintainer) at the end.
> - **Status:** no code has shipped.
> - **Sources:** GenomeSpy facts come from its docs and npm at v1.3.0
>   (2026-11-04). Chimaera facts were checked against this tree at `f22fb7d2`,
>   with file references. Items marked *spike* are not yet verified; they are
>   the first things to test.

## Why

[GenomeSpy](https://genomespy.app/) (MIT, `@genome-spy/core` 1.3.0) is a WebGL
genome browser. A JSON **spec** describes the view in a grammar much like
Vega-Lite's:

- **Views and tracks:** `layer`, `vconcat`, `concat`, `facet` and `multiscale`.
- **Marks:** `rect`, `point`, `rule`, `tick`, `text`, `link` and `arrow`.
- **Encodings** over a `locus` scale.

It reads indexed files in pieces with HTTP Range requests: BAM with `.bai`,
BigWig and BigBed, indexed FASTA, and tabix-indexed VCF and GFF3. It also loads
TSV, CSV, JSON and Parquet whole.

Its API is designed for agents. The v1.2.0 notes say `view.data.transform()` is
"designed primarily for agentic use". The parts that matter here:

- `getSpecErrorLocation` returns where a spec error is, as a JSON pointer.
- `view.data.read()` and `view.data.transform()` query the data the view has
  loaded, with a cap on rows.
- `awaitVisibleLazyData()` and `dataLoading.getSnapshot()` report which sources
  have loaded or failed.
- `imageExport.raster()` and `imageExport.svg()` export the view as PNG or SVG.

The goal is one genome view that both agents and users work in:

- An agent given BAM, VCF or BigWig files finds out what they contain and opens
  them for the user.
- The agent writes a spec as a text file that both of them edit.
- The agent sees a picture of the result.
- The agent works with what the user is looking at and has selected.

GenomeSpy is also one case of a more general problem. Nearly every scientific
visualization library is JavaScript: GenomeSpy, IGV.js, Vega, Mol\*, Plotly and
map engines. Under platform decision 2 ("no plugin HTML, script or stylesheet
anywhere"), each of them would have to be a viewer built into core. This record
lets a plugin ship such a library itself, so core grows one generic piece rather
than a collection of viewers.

## The short version

1. **A new `web` node for plugin screens.** A plugin's screen is still a `ui/1`
   tree. The editor, problems list, status, switches and settings around it stay
   Chimaera's own. One node, `web`, is a page from the plugin's own bundle. It
   runs where an HTML report runs today:
   - in the browser, a sandboxed iframe with no origin;
   - in the macOS app, a native pane view in its own web process
     (`shared/paneView.ts`).

   Plugins still look like Chimaera, and only the drawing area is theirs.
2. **Doors in and out.** The page reaches Chimaera through exactly four doors,
   each one bounded by the host:
   - **`web/1`**, a small versioned message channel: actions, theme, references,
     saves, and calls from its plugin.
   - **Data URLs** for workspace files, minted only within the plugin's
     `[access]` and readable with plain `fetch`, Range requests included.
   - **The network hosts the manifest declares**, enforced by the browser through
     CSP.
   - **Its own bundle.**
3. **The bundle travels inside the component.** The page's files are embedded in
   `plugin.wasm`, and the host unpacks them once per installed version. So the
   lock's existing sha256 covers them, the lock review covers them, and the
   release format does not change.
4. **Agents can call the page.** A plugin's tool can call a method on its page,
   and the page answers with `chimaera.handle`. The call goes to the user's
   window if one shows the view, otherwise to a headless page in the daemon's
   Chromium engine. This is how agents get pictures and read the live view.
5. **GenomeSpy becomes a plugin.** `id = "genomespy"` lives in its own
   repository and pins its own GenomeSpy build, so the library updates on the
   plugin's schedule. It needs no programs. Specs (`*.genomespy.json`) open as
   an editor beside a live view. Data files open straight into a view. Agents get
   tools to inspect files, check specs, see the view, query it, and open it next
   to their session.

## The amendment to platform decision 2

Decision 2 was made for screens such as lists, forms, settings and cards. For
those, sending data that Chimaera renders is clearly right. It does not suit a
drawing area: a genome track, a molecule or a map is drawn by a library that
already exists, and that library is JavaScript. The rejected alternative to
amending the decision was a core viewer node per library. That keeps the rule
on paper but moves every plugin's domain into core, which is the opposite of
[platform decision 1](plugin-platform-plan.md#decisions-maintainer-2026-09-29).

What decision 2 protected, and how web views keep it:

| Protected | How it holds |
|---|---|
| **A consistent look** | Only the drawing area is web. The parts around it stay `ui/1`. The page gets Chimaera's tokens, fonts and a small stylesheet for in-page controls (`chimaera-web.css`), and theme changes arrive live. The guide says web is for drawing areas, not whole screens, and the lock review checks it for first-party plugins. |
| **Safety by construction** | The page has no origin. It cannot reach the window, the token, the daemon's API or other panes. It can read only what its plugin's `[access]` allows, and the network only at declared hosts. It cannot open popups, navigate the top window, submit forms, start downloads or open modals. The browser enforces all of this. |
| **Works over a tunnel** | Everything is served from the daemon's origin: the bundle, the data URLs and the bridge. |
| **Evolves without breaking** | `web/1` is versioned like `ui/1`. A host that does not know `web` draws the node's `fallback`. |
| **Accessible by construction** | This holds only partly, and canvas libraries were never accessible anyway. A `web` node must have a `label`, and everything around it stays core's accessible components. |

## The platform: what core adds

### The `web` node

```json
{"type": "web",
 "entry": "genome.html",
 "id": "tp53",
 "label": "Genome view of TP53",
 "data": {"spec": "results/tp53.genomespy.json", "locus": "chr17:7668402-7687550"},
 "key": 3,
 "fallback": {"type": "callout", "text": "This chimaera cannot draw this view."}}
```

- **Props.**
  - `entry` is a page in the plugin's bundle.
  - `data` is JSON of at most 64 KiB that the page receives as `chimaera.data`.
  - A new `key` sends the new `data` to the running page as an update rather than
    reloading it. `DeckNode`'s `start`/`startKey` works the same way.
  - `id` names the instance for calls.
  - `label` is required.
- **Layout.** It fills its slot the way `pdf` and `slides` do (`fill`, `split`;
  `plugins.md` "Where a view draws").
- **Where it runs.** In the browser it runs in an iframe with
  `sandbox="allow-scripts"`; it never gets `allow-same-origin`, popups, forms,
  top navigation or modals. In the app it runs in a native pane view, which
  already handles geometry, overlaps, crashes, parking, moves, loading and
  snapshots (`paneView.ts` header).
  - The page's CSP:
    - `default-src 'none'`
    - `script-src` and `style-src` limited to its bundle, plus `'unsafe-eval'`
      and `'wasm-unsafe-eval'`. These are allowed because a page with no origin
      and no network gains nothing from eval, and Vega-style libraries compile
      expressions.
    - `img-src` and `media-src` from its bundle, data URLs, `blob:` and `data:`
    - `connect-src` limited to its data URLs plus the declared hosts
    - `worker-src blob:` and its bundle
  - *Spike:* confirm the minimum GenomeSpy needs.
- **Resources.** Limits on this side are needed to keep the app light:
  - At most 4 web views run per window; past that, the least recently used one
    closes first.
  - A parked view shows its last snapshot, then closes after 2 minutes. The
    native view already hides while parked.
  - While a view is hidden the page gets `visibility: hidden`. The guide requires
    it to stop drawing, and the lock review checks this. A browser cannot force
    a page to stop, but a native pane view in its own process can be suspended.

### Delivery: the bundle inside the component

- **A new export.** API **0.3** adds one export, `web-bundle() -> list<u8>`. It
  returns a tar archive of the page's files, built with `include_bytes!` in the
  plugin.
- **Unpacking.** The host calls it once per installed version and unpacks it with
  the same safe unpacker as tools: no absolute paths, no `..` and no links
  (`plugins/toolchain.rs`). The files go to
  `~/.chimaera/plugins/<id>/<version>/web/` and are served from there under a
  ticket.
- **Size.** It fits under the 16 MiB component cap. GenomeSpy's whole
  `dist/bundle` is about 2.9 MB raw, and smaller after trimming.
- **Integrity.** The lock already pins `sha256_wasm`, so the bundle needs no new
  asset, no new checksum line and no change to the release workflow.
- **Manifest.** It declares the pages:

  ```toml
  [web]
  pages = ["genome.html"]
  network = ["data.genomespy.app", "hgdownload.soe.ucsc.edu"]   # https only; each on the card
  network_user = true      # the user may add hosts in Settings → Plugins (decision 3)
  ```

The alternative is a fourth release asset, `web.tar`, with its own line in
`SHA256SUMS` and its own `sha256_web` in the lock. That suits bundles above
about 12 MB. It is listed as [decision 2](#decisions-for-the-maintainer).

### Data URLs

`chimaera.url(path)` asks the host for a URL to a workspace or `output:` file:

- **It follows the plugin's access.** The host refuses when the plugin's
  `[access] files` is not `read`. Paths that leave the workspace or pass through
  a link are refused, as `host::read` refuses them.
- **The URL is on a new route.** The host answers
  `/webdata/{ticket}`: the same streamed single-range `/raw` serving
  (`fs.rs:5349-5406`), with `Access-Control-Allow-Origin: *` and an `OPTIONS`
  answer for Range preflights.
- **The ticket is the authority.** Only this page asked for it, and it names one
  file. The HTML preview's no-CORS rule for reports (`fs.rs:5068`) is unchanged.
- **Ticket lifetime.** It lives 10 minutes from its last mint, and asking again
  for an unchanged file returns the same URL and extends it (`fs.rs:4783`). The
  bridge does this for the page while the view is visible. A changed file gets a
  new URL, which is correct.
- **No relay through the bridge.** The page fetches with plain `fetch`, so a
  library's own readers work unchanged and no bytes pass through it. GenomeSpy's
  BAM, BigWig and tabix readers are an example. A 50 GB BAM works, and over an
  SSH tunnel too.
- *Spike:* Range from an origin-less frame in WKWebView, WebKitGTK and Chrome,
  with and without a preflight.

### The bridge, `web/1`

The host provides `chimaera-web.js`, which a page includes. In the iframe it
talks over `postMessage`; in the native view it uses the existing pane bridge
(`chimaera_core::PANE_BRIDGE_JS`, `native_view.rs`). Messages are JSON, at most
64 KiB, and rate-limited.

| Call | Does |
|---|---|
| `chimaera.data`, `onData(fn)` | The node's `data`, and updates when the node's `key` changes. |
| `chimaera.theme`, `onTheme(fn)` | `{mode, tokens, fonts}`. `chimaera-web.css` exposes the tokens as CSS variables, with core's button, input and toolbar styles for in-page controls. |
| `chimaera.visibility`, `onVisibility(fn)` | Whether the page is visible. The page stops drawing when hidden. |
| `url(path)` | A data URL (see above). |
| `act(action, payload)` | The plugin's `on_action`, or a built-in action (`open-file`, `open-view`, `open-url`, `copy`, `ask-agent`, `save-to-workspace`). The plugin may answer with a new tree. |
| `reference({text, file?, line?, crop?})` | Points an agent at what the user selected: a draft in the composer, with the crop attached. It works like the image area tool (`ImageView.svelte:272`, `App.svelte` `referenceSelection`) and is never sent on its own. |
| `save(name, blob)` | Writes into the plugin's output folder under the same caps as `output_write`, for exports and pictures. |
| `handle(method, fn)` | Answers calls from its plugin (next section). |
| `status({state, text})` | One status pill in the view's bar: loading, ok or errors. |

### Calls: a plugin, or an agent, reaches its page

```
platform::web_call(cx, view, id, method, args) -> call id
ToolResult::wait_call(call_id, "rendering…")   → tool_resume(cx, name, result)
```

- **Where a call goes.** To a window showing that view, the one most recently
  focused in the workspace. If no window shows it, it goes to a **headless host**:
  the daemon's Chromium engine (`agent_browser/chromium.rs`) loads the same page
  in a render-only shell, where the bridge's other end is the daemon instead of a
  window.
  - `fresh: true` forces the headless host, at the spec's own starting region.
- **The channel.** It reuses the agent browser's window request channel
  (`agent_browser/channel.rs`, the `browser_request` / `browser_reply` frames on
  `/ws/events`).
- **Limits.** These match long tools: a call waits at most 45 s, each answer is
  at most 64 KiB, and pictures are written with `save` and answered as
  `![](output:…)` (`plugins/tools.rs:96`). A window that disappears mid-call
  fails the call cleanly.
- **Login nodes.** As with Chromium today, the headless host refuses on Slurm
  login nodes (`chromium.rs:197`). On a cluster workspace it runs inside the job.
  When no engine exists, the tool explains why and how to get one: open the view,
  or install the Chromium plugin.
- **A built-in snapshot.** Every web view also answers `snapshot {width, height}`
  without any page code: a pane-view snapshot or a CDP screenshot.

### Reveal: an agent opens a view next to itself

`platform::reveal(cx, {view | file, args})` asks the windows showing this
workspace to open a plugin view or a claimed file beside the calling session's
pane, under these rules:

- It works only from a session's own call.
- It is limited to 4 a minute per session.
- It never takes focus from typing: the tab opens marked for attention, or the
  existing one gains focus.
- It is recorded in the plugin's activity log.

This follows `browser_show`'s hand-over rules. It is
[decision 4](#decisions-for-the-maintainer).

### References matched by pattern

Today `references/1` makes chips only for ids the plugin lists, at most 5,000.
Loci and gene names cannot be listed. The change adds an optional
`view: {view, args_from_match}` to a shape, so a match opens the plugin's view
with the matched text, for example `{"locus": "<match>"}`. The other limits stay:
at most 16 shapes, each regex at most 80 bytes, with no groups or anchors. The
hover preview asks the plugin through its `query` route.

### Smaller additions

- **`host::read_at(path, offset, cap)`.** `host::read` reads only a file's first
  bytes. BigWig chromosome trees and very large BAM headers can lie beyond the
  first 8 MiB, and `output_read` already accepts an offset.

Double extensions such as `*.vcf.gz` and `*.genomespy.json` already work: a
pattern without `/` is matched against the whole file name (`matchesPattern`,
`platform.ts:208`; `platform::matches`, `platform.rs:664`).

### Trust

- **What the card's Can list says:** "Draws its own views (web pages in a
  sandbox)", plus one line per network host ("Reaches data.genomespy.app"). If
  `network_user` is set: "You can let it reach other sites".
- **Tier.** Web views alone keep a plugin **sandboxed**, because the browser
  enforces the isolation and the page's file access is the plugin's own. Network
  hosts are capability atoms, so a release that adds a host asks the user again,
  as a wider `[access]` does today.

## GenomeSpy, the plugin

### What the user sees

**A spec file, `*.genomespy.json`:**

- **The layout.** The editor (core's own) sits on the left and the genome view
  (`web`) on the right. Above them are a **Split · Source · View** switch and a
  status pill such as `hg38 · 4 tracks · 2 loading`.
- **Live updates.** Saving, or an agent writing the file, redraws the view
  without losing the current region. The plugin hears `file-changed`, the node
  gets a new `key`, and the page re-embeds and zooms back.
- **Problems.** They appear as editor marks on the right line, with **Ask agent**.
  Two sources feed them:
  - The plugin's checks: `sample.bam has no .bai beside it`, or `this VCF names
    chromosomes "1, 2, …" but the view is hg38 ("chr1, …"): set addChrPrefix`.
  - GenomeSpy's own errors: `Invalid field "positon". Available fields: position,
    value`.

  The page posts each error with its JSON pointer through `act`. The plugin owns
  the JSON text, so it maps the pointer to a line and publishes `diagnostics/1`.
  A broken spec keeps its last good picture under the error, so the view does
  not go blank while someone types.
- **Loading per track.** Each data source reports `loading`, `complete` or
  `error` (`dataLoading`). A source that fails names its file and does not fail
  the view.

**Data files (`.bam`, `.bw`/`.bigwig`, `.bb`/`.bigbed`, `.vcf.gz`, `.gff3.gz`,
`.fa`/`.fasta`) open as a genome view:**

- **The view is built from the header.** The plugin reads the header in its
  sandbox and builds a spec from it:

  | File | View |
  |---|---|
  | BAM | Coverage, with reads below 20 kb (`multiscale`). A BigWig beside it is used for coverage when present. |
  | VCF | Variants coloured by type, with genotypes when there are fewer than about 50 samples. |
  | BigWig | A signal track. |
  | GFF3 | Gene models. |
  | FASTA | The sequence at base-pair zoom. |

  Today all of these except `.vcf.gz` and `.gff3.gz`, which open as tables, show
  only a binary info card (`previews/files.ts:1046`).
- **It identifies the reference genome.** A position such as `chr1:1,000,000`
  means something only against one reference build: hg38, hg19, mm10 and so on.
  Files rarely name their build, but the header lists each chromosome's name and
  length, and those lengths identify the build. For example, chr1 is 248,956,422
  bp in hg38 and 249,250,621 bp in hg19. The plugin matches them against the
  builds GenomeSpy knows. If none matches, it builds the chromosome list from the
  header. It always says which build it chose and why. When two files disagree,
  it warns instead of drawing one over the other, because data drawn on the wrong
  build lands in the wrong place with no error at all.
- **Make editable.** This writes the generated spec to
  `output:<name>.genomespy.json`. `save-to-workspace` then copies it next to the
  data, asking before it replaces anything. From then on that file is the shared
  one.
- **Text formats.** `.vcf`, `.bed`, `.bedgraph`, `.gff`, `.narrowPeak` and `.tsv`
  keep core's table view (`TABLE_PRESETS`, `previews/files.ts:551`), which suits
  them. They get a **View on genome** file action instead, because a claim always
  wins over the built-in view.
- **Index files.** `.bai`, `.csi`, `.tbi`, `.fai` and `.gzi` are found beside the
  data. When one is missing, a callout names the file and offers
  **Ask agent to index it** (`samtools index`, `tabix -p vcf`). The plugin never
  runs a program itself.
- **Unsupported formats.** `CRAM` and `BCF` are claimed only to say plainly that
  GenomeSpy 1.3 reads neither.

**The page's controls** belong to the page, because typing a locus or zooming
should not wait on a round trip through the daemon. They use `chimaera-web.css`,
so they look like core's:

- **Locus field.** It takes `chr17:7,668,402-7,687,550`, `chr17`, or a gene
  name when the spec has a searchable annotation track (GenomeSpy 1.3 search),
  with back and forward through visited regions.
- **Zoom and fit.** Zoom out, zoom in and fit, with keys: arrows pan, `+`/`-`
  zoom, `/` focuses the locus field, Esc clears the selection.
- **Export.** PNG or SVG through `imageExport`, then `save` and
  **Save to workspace**.
- **@.** This sends the region, the visible tracks and any brushed selection,
  with a crop, through `reference`.
- **Theme and tooltips.** Chimaera's tokens are mapped onto GenomeSpy's `theme`
  (axes, grid, fonts, view fill), and `.gs-tooltip` is styled with the same
  tokens. A theme change re-embeds the view at the same region.
- **Narrow panes.** The controls collapse to the locus field and a "…" menu.

**Loci in chat become chips** (pattern references). Hovering one previews it;
clicking opens the view at that region.

### What agents get

Tool names are prefixed `genomespy_`. The always-on instructions are short, and
`genomespy_guide` gives the rest on demand. GenomeSpy's own
[app-agent](https://github.com/genome-spy/genome-spy/tree/master/packages/app-agent)
work found the same: keep the prompt small, look up docs on demand, and never
send raw data up front.

| Tool | Needs | Does |
|---|---|---|
| `genomespy_guide {topic?}` | nothing new | The grammar for the GenomeSpy version the plugin bundles. It covers tracks per data type, multiscale, assemblies, loci and the coordinate rules, and names the exact `$schema`. |
| `genomespy_inspect {path}` | nothing new | Reports what is in a data file without loading its data: the format; contigs with lengths and `chr` naming; the reference build it matches, with how sure it is; samples and read groups; INFO and FORMAT keys; zoom levels; indexes present or missing; and a ready track snippet. It parses headers in pure Rust over `host::read`. |
| `genomespy_check {path}` | nothing new, then calls | Checks a spec. In phase 2 it checks shape, paths, indexes, chromosome naming against the build, and field names against TSV and VCF headers. With calls it also runs GenomeSpy's own validation in a real embed. The answer lists problems with lines, and the same list appears as editor marks. |
| `genomespy_look {path, locus?, width?, height?}` | calls | Shows the agent the view in its answer, plus the visible region, rows loaded per track and any failed sources. It renders from the user's window when the user has the view open (so the agent sees what they see), otherwise headless. |
| `genomespy_query {path, view, locus?, transform?, limit?}` | calls | Runs `data.read` or `data.transform` (filter, aggregate, window) over the loaded data, with a row cap. For example: "how many variants here have AF > 0.1?". |
| `genomespy_where {}` | calls | What the user is looking at: the open genome views, each one's region, and any brushed selection. |
| `genomespy_show {path, locus?}` | reveal | Opens or focuses the view next to the calling agent's session, or moves the user's open view to a region. |

A typical loop:

1. `inspect` the data files.
2. Write `results/tp53.genomespy.json` with the agent's own Write tool. Plugins
   never replace an agent's native tools.
3. `check` the spec.
4. `look` at it.
5. Fix any problems.
6. `show` it to the user.
7. Later, use `where` and `query` to answer questions about the user's selection.

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

- **Rust (`src/`):**
  - header parsers: BAM `@SQ`/`@RG`, VCF `##contig` and `#CHROM`, BigWig and
    BigBed chromosome trees, `.fai`, and `.tbi` names;
  - build matching against GenomeSpy's built-in assembly tables, vendored with
    their source cited;
  - spec templates for each data type and their combinations;
  - JSON pointer → line mapping;
  - the semantic checks, the guide and the tools.

  All of it is pure and natively tested on real header bytes.
- **The page (`web/`):**
  - `genome.html` plus a pinned, self-hosted `@genome-spy/core` with no CDN;
  - `genome.js` glue: embed with `url()`-mapped paths and explicit index URLs,
    the theme mapping, posting errors with their locations, the in-page
    controls, the `reference` call, and the call handlers (`png`, `domain`,
    `zoom`, `selection`, `read`, `transform`, `errors`, `loading`).

  It is built by the plugin's CI into the tar archive that `web-bundle()` returns.
- **Upgrades.** A GenomeSpy upgrade is a plugin release, not a Chimaera release.
  The plugin is sandboxed, so if the release asks for no new capabilities, the
  lock bump merges automatically.

## Phases and acceptance

Each phase is verified live in the isolated preview, the macOS app (WKWebView)
and Chrome. That means checking light and dark, narrow and wide panes, the
keyboard, and a remote workspace over the throttled-tunnel setup. Test data are
small committed files generated from public data, under about 5 MB in total.

0. **Spike (about 3 days; the code is thrown away).** A GenomeSpy page served
   from a plugin-like folder. It runs in an origin-less iframe, in a native pane
   view and in headless Chromium. It tests:
   - Range `fetch` to a CORS ticket route, with and without preflight;
   - the CSP GenomeSpy needs (eval, workers, WebGL);
   - zero idle CPU;
   - working offline, including where the WebGL font atlas loads from;
   - bundle size;
   - a review of whether an untrusted spec can turn text into HTML (tooltips,
     `text` marks, `description`).

   It ends in a go or no-go note added to this record.
1. **The web views platform:**
   - the `web` node;
   - API 0.3 `web-bundle()` with unpacking;
   - `/webdata` with `url()`;
   - `web/1`: data, theme, visibility, act, reference, save, status;
   - CSP from `[web]`;
   - the Can list and capability atoms;
   - the resource caps;
   - a test fixture plugin, `plugins/test-web`;
   - `tests/plugin_web.rs`;
   - the guide's web section and feature page.

   **Acceptance:** the fixture's page draws in both hosts, reads a workspace file
   by range, is refused outside its access and at an undeclared host, follows
   the theme live, and its @ lands a draft with a crop.
2. **GenomeSpy 0.1:**
   - specs and data files open;
   - Make editable;
   - View on genome;
   - diagnostics;
   - `guide`, `inspect` and `check`.

   **Acceptance:** given a BAM and a VCF, an agent inspects them and writes a
   spec that opens correctly the first time. The user edits it live. A missing
   index, a `chr` mismatch and a typo in a field each read plainly.
3. **Calls and the headless host:** `look`, `query`, `where`, and the
   engine-backed `check`.

   **Acceptance:**
   - `look` returns a correct picture within 10 s with the view closed, on a
     host whose only browser comes from the Chromium plugin. On a login node it
     refuses with a plain explanation.
   - The user brushes a region and asks "what's in here?". The agent answers from
     `where` and `query`, with counts and the top rows.
4. **Reveal and locus chips:** `show`, and pattern references.
5. **Later, if wanted:**
   - GenomeSpy's sample-collection App (`@genome-spy/app`; cohorts and
     provenance) as a second page;
   - a second plugin, privileged and opt-in, for indexing and bgzipping;
   - other visualization plugins on the same platform (Vega-Lite, Mol\*, maps),
     each a plugin.

**Shipping.** Phase 1 is a `feat:` PR to Chimaera with the ship-pr gates, the
feature page, the guide, and wire fixtures for any new frames. Phase 2 starts
the plugin repository with its CI and release, then a lock entry with
`tier = "sandboxed"`.

## Risks

- **Third-party code in the user's window.** It is sandboxed, but it is still
  running, and agents write the specs it reads. The defences:
  - an origin-less frame with CSP;
  - data limited to the plugin's access;
  - declared hosts only;
  - the lock review of a first-party bundle;
  - the spike's review of every path from text to HTML.
- **A page that ignores visibility.** It can keep a CPU busy in the browser. The
  native view can be suspended. The lock review and a CPU check in the fixture's
  test catch it for first-party plugins; for third-party plugins the card says
  "draws its own views".
- **Look drift.** In-page controls are the plugin's. `chimaera-web.css`, the
  guide's rule and the lock review limit it.
- **Platform cost.** Phase 1 is real work across the daemon, the UI, the app's
  pane-view bridge and the WIT. It is paid once for every visualization plugin
  after this one.

## Decisions for the maintainer

1. ~~Core viewer, or plugin web views~~ **Decided 2026-11-26: plugin web views.**
2. **Where the bundle travels.** *Recommended: embedded in the component*
   (`web-bundle()`, so no release-format change). The alternative is a fourth
   release asset, needed past about 12 MB.
3. **Network hosts users can add** (`network_user`). Specs can name any URL, so
   GenomeSpy declares the common genomics hosts and lets the user add others in
   its settings. Each added host goes into the CSP and is shown on the card.
   *Recommended: yes*, with additions scoped to the host the user chose.
4. **Reveal.** Either the agent opens a view beside its own session under
   `browser_show`'s rules, or it only posts a chip that the user clicks.
   *Recommended: open it beside the session, without taking focus.*
5. **The spec file name.** `*.genomespy.json`, or any `.json` whose `$schema` is
   `genomespy.app`. The second needs content sniffing, which is a larger change.
   *Recommended: `*.genomespy.json`.*

## Sources

- **GenomeSpy:**
  - [embedding](https://genomespy.app/docs/api/embedding/)
  - [EmbedResult](https://genomespy.app/docs/api/reference/interfaces/EmbedResult/)
  - [editor integration: feedback for LLM agents](https://genomespy.app/docs/api/editor-integration/)
  - [runtime state and selections](https://genomespy.app/docs/api/runtime-state/)
  - [lazy data sources](https://genomespy.app/docs/grammar/data/lazy/)
  - [genomic coordinates](https://genomespy.app/docs/grammar/genomic-coordinates/)
  - [releases](https://github.com/genome-spy/genome-spy/releases)
  - [genome-spy-python](https://genomespy.app/genome-spy-python/)
- **Chimaera:**
  - [extension guide](../agent-guides/plugins.md)
  - [platform plan](plugin-platform-plan.md)
  - [PowerPoint plan](pptx-plugin-plan.md)
  - [agent browser](../features/agent-browser.md)
  - [files and previews](../features/files-and-previews.md)
