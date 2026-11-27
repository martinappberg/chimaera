# GenomeSpy: genome data that agents can open, read, check and show

> **Dated design record (2026-11-26). Proposed, not approved.** No code has
> shipped. The [decisions](#decisions-for-the-maintainer) at the end are the
> maintainer's to make. Facts about GenomeSpy are from its docs and npm registry
> at v1.3.0 (2026-11-04). Facts about Chimaera were checked against this tree at
> `f22fb7d2`, with file references. Items marked *spike* are unverified and are
> the first things to test.

[GenomeSpy](https://genomespy.app/) (MIT, `@genome-spy/core` 1.3.0) is a WebGL
genome browser driven by a declarative grammar that resembles Vega-Lite. A
visualization is a JSON **spec**: tracks built from views (`layer`, `vconcat`,
`concat`, `facet`, `multiscale`), marks (`rect point rule tick text link arrow`)
and encodings over a `locus` scale. Data comes from files the browser reads in
pieces with HTTP Range requests: BAM with `.bai`, BigWig and BigBed, indexed
FASTA, and tabix-indexed VCF and GFF3. It also loads plain TSV, CSV, JSON and
Parquet. Its API was built with agents in mind:

- `getSpecErrorLocation`: an error comes back as a JSON pointer into the spec.
- `view.data.read()` and `view.data.transform()`: query the loaded rows, with a
  cap on how many come back. The v1.2.0 release notes say these are "designed
  primarily for agentic use".
- `awaitVisibleLazyData()` and `dataLoading.getSnapshot()`: report which data
  sources have finished loading or failed.
- `imageExport.raster()` and `imageExport.svg()`: export the current view as a
  picture.

In Chimaera, agents and users should share one genome view. An agent given BAM,
VCF or BigWig files can find out what is in them and open them for the user. It
can write a spec as a text file that both of them edit, see the rendered result
itself, and work with what the user is looking at or has selected.

## The short version

1. **The viewer is core; the GenomeSpy plugin adds the authoring and agent side.**
   - Chimaera adds one new `ui/1` node, `genomespy`, which draws a spec with a
     pinned GenomeSpy build that ships in the web UI and loads only when needed.
     Core already does this for PDF (pdf.js), PowerPoint, Word, Marp and Parquet.
   - The new GenomeSpy plugin (`id = "genomespy"`, in its own repository) handles
     everything specific to genomics:
     - reading file headers;
     - guessing the reference assembly;
     - generating specs;
     - semantic checks;
     - the agent tools and the guide.
   - The PowerPoint plan split the work the same way: viewing decks stays in core,
     and the plugin is "the opt-in authoring half"
     ([pptx-plugin-plan](pptx-plugin-plan.md)).
2. **The plugin is sandboxed.** It needs `files = "read"`, its own output folder,
   and no programs. That means a one-click install from the verified list, and
   updates that merge automatically.
3. **The spec file is the shared text file.** Files named `*.genomespy.json` open
   with an editor and a live viewer side by side, like LaTeX's Split · Source · PDF.
   - A user's edit or an agent's edit redraws the view within about 300 ms.
   - Problems are marked in the editor at the right line.
4. **Data files open directly.** `.bam`, `.bw`, `.bb`, `.vcf.gz`, `.gff3.gz` and
   indexed `.fa` files open in a genome view built from their headers.
   - Today all of these, except `.vcf.gz` and `.gff3.gz`, open as a binary info card.
   - **Make editable** turns the view into a `*.genomespy.json` file.
5. **Agents get four abilities, each from a small generic addition to the host:**
   - read data files and specs (works today);
   - see a picture of a view (needs node calls and headless rendering);
   - work with the user's live view: move it, read from it, see their selection
     (needs node calls);
   - open a view beside the agent's own session (needs reveal).

   Each addition is generic, so later viewers can use it too: a Vega-Lite chart,
   a molecule viewer, a map.

## Why not let the plugin ship GenomeSpy itself

Maintainer decision 2 of the
[platform plan](plugin-platform-plan.md#decisions-maintainer-2026-09-29) says
plugins draw screens in Chimaera's own format: "There is no plugin HTML, script
or stylesheet anywhere." GenomeSpy is a JavaScript and WebGL library, so a plugin
cannot ship it. There are three possible routes:

| Route | What it means | Verdict |
|---|---|---|
| **A. A core `genomespy` viewer node** (recommended) | Core bundles a pinned `@genome-spy/core`, draws it with Chimaera's tokens and controls, and owns how file paths become URLs. The plugin sends specs as data. | It keeps decision 2. The viewer gets a consistent look and theming, and works over the tunnel. Core grows one format viewer, as it did for PDF, slides and PowerPoint. GenomeSpy's version becomes tied to Chimaera's releases. |
| B. A general sandboxed web-content node | Plugins ship HTML/JS that runs in a sandboxed iframe with a postMessage bridge. | This reverses decision 2 and brings the costs the plan rejected: a second security boundary, an inconsistent look, and themes the host cannot control. It is not justified for one viewer. |
| C. No host change | The plugin writes a standalone GenomeSpy HTML file and opens it in the HTML preview. | The preview runs in an opaque-origin sandbox and is denied CORS (`fs.rs:5069`), so a page cannot read workspace BAM or BigWig files. Only inline data works. It would also load GenomeSpy from a CDN. This is fine as a throwaway check, but not as the product. |

Route A breaks a rule if the node is designed carelessly: "the daemon and client
must not branch on a new plugin's id or file format". It stays within that rule
for three reasons:

- **The node is a viewer for a published, versioned grammar** (`$schema`
  `https://genomespy.app/schema/core/v1.json`), and any plugin may use it. Likely
  users are a variant-calling plugin, an nf-core results plugin or a single-cell
  plugin. GenomeSpy is not tied to one plugin's file format.
- **Core holds no genomics logic.** It has no idea what a BAM is. It maps
  workspace paths to URLs, applies the theme, provides the controls, and answers
  calls it defines itself.
- **The spec decides what is drawn.** Choosing to show a BAM as coverage plus
  reads is the plugin's decision.

## What the user sees

### A spec file: `*.genomespy.json`

- **Split view.** The editor (core's own) is on the left and the genome view on
  the right, with a **Split · Source · View** switch (`segmented`) and a status
  chip: `hg38 · 4 tracks · 2 loading`.
  - Saving, or an agent's write, redraws the view without losing the current
    region. The plugin hears `file-changed` and calls `invalidate`; the node
    re-embeds with the same `zoomTo` domain.
- **Problems in place.** The plugin's checks and GenomeSpy's own errors become
  `diagnostics/1` marks on the right line, with **Ask agent**. Examples:
  - `Invalid field "positon". Available fields: position, value`
  - `sample.bam has no .bai beside it`
  - `contigs are "1, 2, …" but the assembly is hg38 ("chr1…"): set addChrPrefix`

  A broken spec keeps its last good picture under an error callout, so the view
  never goes blank while someone types.
- **Loading is shown per track.** GenomeSpy's `dataLoading` reports one
  `loading`, `complete` or `error` entry per data source. The node shows these as
  small status pills and an error row naming the file. A data source that fails
  does not fail the whole view.

### A data file: `.bam .bw .bigwig .bb .bigbed .vcf.gz .gff3.gz .fa`

- **It opens as a genome view.** The plugin reads the file header in its sandbox
  and builds a spec:
  - **BAM:** coverage, plus reads below 20 kb using `multiscale`. A BigWig beside
    it is used for coverage when present.
  - **VCF:** variants coloured by type, with the sample genotypes when there are
    fewer than about 50 samples.
  - **BigWig:** a signal track.
  - **GFF3:** gene models.
  - **FASTA:** the sequence at base-pair zoom.

  The assembly comes from the header's contig names and lengths. The plugin
  matches them against GenomeSpy's built-in assemblies (hg38, hg19, mm10, mm9,
  dm6). If none matches, it uses a `genomes.contigs` entry built from the header.
- **Make editable.** The plugin writes the generated spec to
  `output:<name>.genomespy.json`, and the built-in `save-to-workspace` action
  copies it next to the data, asking before it replaces a file. From then on
  that file is the shared text file.
- **Plain text formats keep their table view.** `.vcf`, `.bed`, `.bedgraph`,
  `.gff`, `.narrowPeak` and `.tsv` stay in core's `TableView` (`TABLE_PRESETS`,
  `previews/files.ts:551`), which works well for them. Instead they get a file
  action, **View on genome**. A claim always wins over the built-in view, so the
  plugin claims only formats that have no good view today.
- **Index files are found automatically.** The plugin looks for `.bai`, `.csi`,
  `.tbi`, `.fai` and `.gzi` beside the data file. If one is missing, the view
  shows a callout that names the missing file. The callout offers **Ask agent to
  index it** ("index it" means running `samtools index` or `tabix -p vcf`). The
  plugin never runs a program itself, which keeps it sandboxed.

### The view's own controls

The viewer uses core's controls and theme tokens. It does not use GenomeSpy's
`attachControls` buttons.

- **A locus field.** It accepts `chr17:7,668,402-7,687,550`, `chr17`, or a gene
  name when the spec has a searchable annotation track (GenomeSpy 1.3's search
  utilities). It also has back and forward history for regions.
- **Zoom and fit.** Zoom out, zoom in and fit, plus the keys: arrows pan, `+` and
  `-` zoom, `/` focuses the locus field, and Esc clears a selection.
- **Export.** PNG and SVG go to the output folder through `imageExport`, and
  **Save to workspace** copies them out. Large SVGs have their dense parts
  rasterized; GenomeSpy reports this, and the view says so.
- **@ (point the agent here).** This sends the current region and the visible
  tracks as a reference, the same way the slide viewer does (`DeckNode.svelte:586`).
  With a brushed selection, it also sends a crop and the region's coordinates,
  the way the image area tool does (`ImageView.svelte:272`). The text goes into
  the composer as a draft and is never sent on its own.
- **Theme.** Chimaera's light and dark tokens are mapped onto GenomeSpy's
  `theme` (axes, grid, fonts and view fill), and `.gs-tooltip` is styled with the
  same tokens. Switching theme re-embeds the view at the same region.
- **Narrow panes.** The toolbar collapses to the locus field and a "…" menu.

### Loci in chat become chips *(phase 3)*

When an agent writes `chr17:7,668,402-7,687,550` or `TP53`, the text becomes a
reference chip, the same mechanism Knowledge ids use. Hovering shows a small
preview, and clicking opens the genome view at that region. This needs a small
generic change to `references/1` (the [host additions](#generic-host-additions),
item 4).

## What agents get

Tool names are prefixed `genomespy_`. The always-on instructions are short (well
under the 8 KiB cap), and `genomespy_guide` gives the rest on demand. GenomeSpy's
own app-agent work came to the same conclusion: keep the prompt small, look up
the docs when needed, and never send raw data up front
([app-agent ARCHITECTURE](https://github.com/genome-spy/genome-spy/tree/master/packages/app-agent)).

| Tool | Phase | Does |
|---|---|---|
| `genomespy_guide {topic?}` | 1 | The grammar cheat sheet for the GenomeSpy version core bundles. It covers tracks per data type, multiscale, assemblies, loci and the coordinate rules, and links to the exact `$schema`. |
| `genomespy_inspect {path}` | 1 | Reports what is in a data file without loading its data (details below). |
| `genomespy_check {path}` | 1 → 3 | Checks a spec. Phase 1 checks shape, paths, indexes, chromosome naming against the assembly, and field names against TSV and VCF headers. Phase 3 adds GenomeSpy's own validation from a real embed. It answers with problems and their lines, and publishes the same list as editor marks. |
| `genomespy_look {path, locus?, width?, height?}` | 2 | Shows the agent the view as a picture in the tool answer (`![…](output:…)`, `tools.rs:96`). Alongside it: the visible region, rows loaded per track, and any data that failed. When the user has the same file open, the picture comes from their view; otherwise it is rendered headless. |
| `genomespy_query {path, view, locus?, transform?, limit?}` | 3 | Runs `data.read` or `data.transform` (filter, aggregate, window) over the data the view has loaded, with a cap on rows returned. For example: "how many variants with AF > 0.1 are in this window?" |
| `genomespy_where {}` | 3 | What the user is looking at here: open genome views, each one's region, and any brushed selection. |
| `genomespy_show {path, locus?}` | 3 | Opens or focuses the view next to the calling agent's session, under the same rules as `browser_show` ([decision 3](#decisions-for-the-maintainer)). It can also move a view the user already has open to a region. |

`genomespy_inspect` works out of the box in the WASM sandbox with
`host::read`, using pure-Rust BGZF/deflate. It reports:

- the format;
- each contig with its length, and whether names use the `chr` prefix;
- the assembly guess and how confident it is;
- BAM: read groups and sample names;
- VCF: sample names, INFO and FORMAT keys, and whether the file is bgzipped
  and indexed;
- BigWig: zoom levels;
- index files present or missing;
- a ready-made track snippet for this file.

This tool is what lets an agent understand the data. The typical mistakes in
genomics visualization are a mismatched assembly, chromosome naming that does
not match (`chr1` vs `1`), and a missing index. This tool and `genomespy_check`
catch all three before the user notices them.

A typical loop:

1. `inspect` the data files.
2. Write `results/tp53.genomespy.json` with the agent's own native Write tool.
   Plugins never replace an agent's native tools.
3. `check` it.
4. `look` at the picture.
5. Fix anything wrong.
6. `show` it to the user.
7. Later, `where` and `query` answer questions about what the user selected.

## Generic host additions

Each addition is small, versioned in the `ui/1` and WIT contracts, and useful
beyond GenomeSpy. Without any of them, phase 1 still delivers the user-facing
view and the read-only agent tools.

### 1. The `genomespy` ui/1 node *(phase 1)*

The node's props:

```json
{"type": "genomespy",
 "spec": {…} | "src": "path/to/x.genomespy.json" | "output:gen/x.genomespy.json",
 "locus": "chr17:7668402-7687550", "locusKey": 3,
 "id": "tp53-view", "label": "TP53 region",
 "fallback": {"type": "callout", "text": "This chimaera cannot draw genome views."}}
```

- **Where the spec comes from.** A plugin passes either a whole spec or a `src`
  file. A spec can be large; the tree cap is 256 KiB (`screens.rs` `TREE_MAX`).
  The node watches `src` itself.
- **URLs are core's job.** Every `url` and `indexUrl` in the spec that is a
  workspace path or `output:` path is mapped to a `/raw/{ticket}` URL.
  - The `/raw` route already serves single byte ranges as 206, streams the bytes
    without buffering them, and has no size cap (`fs.rs:5349-5406`). Over an SSH
    tunnel it is same-origin. A 50 GB BAM works.
  - Paths that leave the workspace are refused (`resolvePlace`, `platform.ts:502`).
  - The plugin makes every index URL explicit in the spec, so the node never has
    to know GenomeSpy's implicit rule (`url + ".bai"`).
  - `http(s)` URLs pass through unchanged (UCSC, `data.genomespy.app`). The view
    lists the remote hosts it reads from ([decision 4](#decisions-for-the-maintainer)).
- **Keeping URLs stable.** A ticket lives 10 minutes from its last mint, and
  using it does not extend that (`fs.rs:4843`). Minting again for the same path
  and mtime returns the same ticket and extends its life (`fs.rs:4783`). So the
  node re-mints every 4 minutes while it is visible, and the URLs do not change,
  as `DeckNode` does (`DeckNode.svelte:215`).
  - Coming back to a view that was hidden longer than the TTL re-mints the
    tickets. If any URL changed, the node re-embeds at the same region.
  - A data file that changes on disk gets a new ticket. That is correct: the
    view should not keep reading an old file.
- **Locus changes do not re-embed.** A changed `locus` together with a new
  `locusKey` calls `zoomTo`, the same pattern as `DeckNode`'s `start`/`startKey`.
  Locus `zoomTo` treats its end as inclusive (GenomeSpy issue #620), and the
  node corrects for this in one place.
- **Errors go back to the plugin.** Errors and data-loading problems, together
  with their `getSpecErrorLocation` pointer, are posted to the plugin as a view
  action of at most 64 KiB (`screens.rs:565`). The plugin maps the pointer to a
  line in its own JSON text and publishes `diagnostics/1`. The plugin holds the
  text, so the line mapping is its job and not core's.
- **Bounded resources.** Browsers limit live WebGL contexts (WebKit's limit is
  low), and panes are kept alive while parked. The node therefore:
  - keeps at most 4 live GenomeSpy instances per window, least recently used
    first out;
  - shows its last raster export as a still picture while parked;
  - calls `finalize()` once it has been parked for 2 minutes;
  - renders on demand only, and does nothing while the page is hidden
    (*spike*: check that GenomeSpy draws nothing when idle);
  - uses `renderer: "auto"`, which falls back to Canvas2D on WebKitGTK and other
    hosts without WebGL2.
- **What changes in core.** Adding the node touches the validator arm in
  `screens.rs`, `KNOWN_NODES` in `ui/screen.ts`, a lazily loaded
  `GenomeSpyNode.svelte` from `UiNode.svelte`, the guide's node table, the
  `screens.rs` unit test, and the every-node fixture
  (`plugins/test-platform`, `tests/plugin_platform.rs:166`).
- **The library.** `@genome-spy/core` goes in as an exactly pinned dependency of
  `web-ui`, imported only through dynamic `import()`. Its ES build is about 640 KB
  plus lazy chunks of about 1.3 MB, all JavaScript, with no WASM and no CDN.
  - Built-in assemblies make no network requests (`genomeStore.js`).
  - Where the WebGL font atlas comes from is not documented (*spike*: block the
    network and look). Cluster workspaces must work offline.
  - *Spike*: measure what it adds to the binary, which embeds `web-ui/dist`.

### 2. Node calls *(phase 2 to 3; the one big new piece)*

A plugin can call methods on a node that **core defines**. Two hosts can answer:

- **a window** that shows the view, the one most recently focused in that
  workspace; or
- **a headless host** when no window shows it: the daemon's existing headless
  Chromium engine (`agent_browser/chromium.rs`) loading a core-owned
  render-only route of the same web UI.

```
platform::node_call(cx, view, node_id, method, args_json) -> call id
ToolResult::wait_call(call_id, "rendering…")   → tool_resume(cx, name, result)
```

- **GenomeSpy's methods** are `png {width, height}`, `domain`, `zoom {locus}`,
  `selection`, `read {view, limit}`, `transform {view, transform, limit}`,
  `errors`, and `loading`. They are core code and call only GenomeSpy's public
  API, so a plugin cannot run script. They give the plugin these tools:
  - `png` → `genomespy_look`;
  - `read` and `transform` → `genomespy_query`;
  - `domain` and `selection` → `genomespy_where`;
  - `errors` → `genomespy_check` (phase 3).
- **The call channel reuses the agent browser's.** That is the
  `browser_request` / `browser_reply` frames on `/ws/events`
  (`agent_browser/channel.rs`).
- **The same limits as long tools.** A call waits at most 45 s. Each answer is
  capped, and pictures are written to the output folder before the tool answers
  with `![](output:…)`. A window that goes away during a call fails that call
  cleanly.
- **Headless rendering limits.** Headless rendering refuses on Slurm login nodes,
  as the Chromium engine already does (`chromium.rs:197`). On a cluster
  workspace it runs inside the job allocation.
  - When no engine is available, `look` explains why and says how to get a
    picture: open the view, or install the Chromium plugin.
  - The Chromium plugin is a recommendation in the manifest, never a requirement.
- **Which window answers.** Preferring the user's own window means the agent sees
  exactly what the user sees, including the region they zoomed to. A `fresh: true`
  argument forces the headless host and the spec's own starting region.

### 3. Reveal: an agent opens a view next to its own session *(phase 3)*

`platform::reveal(cx, {view | file, args})` asks the windows showing this
workspace to open a plugin view or a claimed file beside the calling session's
pane. The rules:

- It is allowed only from a session's own call.
- It is rate-limited (at most 4 a minute per session).
- It never takes focus from what the user is typing. It opens as a tab that is
  marked for attention, or focuses that tab if it is already open.
- It is shown in the plugin's activity log.

`browser_show` already sets the precedent (`agent_browser/tools.rs`, the
hand-over rules). Native chat actions open files through the same layout calls.

### 4. Pattern references with a view template *(phase 3)*

Today, `references/1` makes chips only for ids the plugin lists, at most 5,000.
Loci and gene names cannot be listed. The change adds an optional
`view: {view, args_from_match}` to a `shape`, so a match opens the plugin's view
with the matched text, for example `{"locus": "<match>"}`. The rest stays as it
is: at most 16 shapes, each regex at most 80 bytes with no groups or anchors.
Hover asks the plugin through its `query` route for a one-line preview (gene name
and region size).

### Smaller fixes, generic

- **`host::read_at(path, offset, cap)`.** Today `host::read` reads only the first
  bytes of a file. BigWig chromosome trees and BAM files with very large headers
  can lie beyond the first 8 MiB, and `output_read` already accepts an offset.

Double extensions such as `*.vcf.gz` and `*.genomespy.json` need no change: a
pattern without `/` is matched against the whole file name (`matchesPattern`,
`platform.ts:208`; `platform::matches`, `platform.rs:664`).

## The plugin (its own repository)

```toml
id = "genomespy"
name = "GenomeSpy"
category = "data"
api = "0.2"
[requires] chimaera = ">=<the release with the genomespy node>"
[access] files = "read"
[provides] mcp_tools = ["genomespy_guide", "genomespy_inspect", "genomespy_check", …]
           events = ["file-saved", "file-changed", "settings-changed"]
[[views]] id = "spec"  slot = "file"   title = "Genome view"
[[views]] id = "data"  slot = "file"   title = "Genome view"
[[views]] id = "browser" slot = "tab"  title = "GenomeSpy"   # all genome views here + recent loci
[[files]] match = ["*.genomespy.json"]                     view = "spec"
[[files]] match = ["*.bam", "*.bw", "*.bigwig", "*.bb", "*.bigbed",
                   "*.vcf.gz", "*.gff3.gz", "*.fa", "*.fasta", "*.fa.gz"] view = "data"
[[actions]] match = ["*.vcf", "*.bed", "*.bedgraph", "*.gff", "*.gff3", "*.narrowPeak"]
            label = "View on genome"  action = "view-on-genome"
[[settings]] remote_data (allow | ask)  · max_samples (50) · reads_below_bp (20000)
[recommends] the Chromium plugin, for agents' pictures on hosts with no Chrome
```

What the plugin's code covers:

- **Parsers**, all pure and natively tested on real header bytes: BAM `@SQ` and
  `@RG`, VCF `##contig` and `#CHROM` samples, BigWig and BigBed chromosome trees,
  `.fai`, and `.tbi` sequence names.
- **Assembly matching:** the contig lengths against GenomeSpy's built-in
  assembly tables, vendored with their source cited.
- **Spec templates** for each data type and for combinations of them, such as
  a BAM with a VCF, or several BigWigs.
- **JSON pointer → line mapping** for its own text.
- **Semantic checks** of a spec.
- **The guide**, versioned to the core node's GenomeSpy version. The render
  `args` carry `genomespy: "1.3.0"`, so a guide written for a newer grammar can
  say "this chimaera draws 1.3".

The plugin claims `CRAM` and `BCF` only to show a plain sentence: GenomeSpy 1.3
reads neither. It never claims them silently.

## Phases and acceptance

Every phase is verified live in the isolated preview, in the macOS app
(WKWebView), and in Chrome. Each must look right in light and dark, at narrow and
wide pane widths, from the keyboard, and with a remote workspace over the
throttled-tunnel setup. The test data are small committed files, generated from
public data and kept under about 5 MB in total.

0. **Spike, about 2 days, throwaway.** Settles every *spike* item above:
   - GenomeSpy runs in WKWebView, WebKitGTK and Chrome;
   - BAM, BigWig and VCF load through `/raw` tickets, locally and over a tunnel;
   - offline with the network blocked;
   - idle CPU is near zero and parked views behave;
   - the binary-size delta;
   - an XSS review of untrusted specs: tooltips, `text` marks and `description`,
     with no custom tooltip handlers;
   - the theme mapping.

   It ends in a go or no-go note added to this record.
1. **Viewer and read-only agent tools.** Core gets the `genomespy` node, ticket
   mapping, theming, controls, @, parked-view handling, and errors posted back.
   The plugin gets specs and data files opening, Make editable, View on genome,
   diagnostics, and `guide` / `inspect` / `check`.
   - Acceptance: an agent given a BAM and a VCF inspects them, writes a spec that
     opens correctly the first time, and the user edits it with every change
     showing live.
   - Failures read plainly: a missing index, a `chr` mismatch, a typo in a field.
2. **Agents' eyes.** Node calls, with `png` from a window and from the headless
   host; `genomespy_look`.
   - Acceptance: with the view closed and on a host where only the Chromium
     plugin provides a browser, `look` returns a correct picture within 10 s.
   - On a login node it refuses with a plain explanation.
3. **The live view.** Reveal, `where`, `query`, engine-backed `check`, and locus
   chips.
   - Acceptance: the user brushes a region and asks "what's in here?". The agent
     answers from `where` + `query` with counts and the top rows, without the user
     pasting coordinates.
4. **Later, only if needed.** GenomeSpy's sample-collection App (cohorts of
   thousands of samples, provenance), using `@genome-spy/app` with its own schema,
   as a separate `app: true` mode of the node. Also optional privileged
   conveniences such as indexing and bgzipping, which belong in a second plugin
   or an opt-in tool, never in the sandboxed one.

Shipping steps:

- Each core PR needs the ship-pr gates, the feature-catalog page and the guide's
  node table.
- The plugin gets its own CI and release.
- The lock entry is `tier = "sandboxed"`, so later updates that ask for no new
  capabilities merge automatically.

## Risks

- **Version coupling.** A grammar change in GenomeSpy reaches users only through
  a Chimaera release. *Mitigation:* the node pins one exact version and reports
  it, the plugin's guide and checks follow it, and the daily release batch keeps
  the delay short.
- **Untrusted specs in the main window.** GenomeSpy runs as first-party code, but
  agents write the specs. *Mitigations:*
  - no custom tooltip handlers;
  - remote URLs are listed, and a setting asks before reading them;
  - `import` of remote specs follows the same rule;
  - the spike reviews every path where spec text could become HTML.
- **Memory and GPU.** The instance cap, parking and `finalize` bound GPU and
  memory use. The daemon keeps no data buffers, because `/raw` streams.
- **Maintainers' bandwidth.** The library's maintainers are a small academic
  team, but they release about weekly to monthly and their API is aimed at agents
  on purpose. Pinning, and treating each upgrade as its own reviewed PR, keeps
  Chimaera insulated.

## Decisions for the maintainer

1. **Route A: a core `genomespy` node, with the plugin doing genomics.** The
   alternatives are B (a general web-content node, which reverses platform
   decision 2) and C (an HTML preview only, which cannot read workspace data).
   *Recommended: A.*
2. **The spec file name.** `*.genomespy.json` is explicit and matches
   genome-spy-python's `chart.save("x.json")` once renamed. The alternative is to
   recognise any `.json` whose `$schema` is `genomespy.app`. Globs cannot read
   file content, so that would need a content-sniffing claim, which is a larger
   host change. *Recommended: `*.genomespy.json`.*
3. **Reveal (an agent opening a view).** Either allow it beside the agent's
   session under `browser_show`'s rules, or only post a chip the user clicks.
   *Recommended: allow it beside the session, without taking focus.*
4. **Remote data.** Either `allow` remote URLs by default and list the hosts in
   the view, or `ask` the first time per host. *Recommended: `ask`*, because
   agent-written specs can name any URL.
5. **Phase 3's two host additions** (node calls and reveal) are the large items.
   Approve them as general platform features (a Vega-Lite or molecule viewer
   would use them too), or keep GenomeSpy at phases 1–2.
   *Recommended: build node calls in phase 2, because the agent's pictures
   depend on it, and decide reveal after phase 2.*

## Sources

- GenomeSpy:
  - [embedding](https://genomespy.app/docs/api/embedding/)
  - [EmbedResult](https://genomespy.app/docs/api/reference/interfaces/EmbedResult/)
  - [editor integration: feedback for LLM agents](https://genomespy.app/docs/api/editor-integration/)
  - [runtime state and selections](https://genomespy.app/docs/api/runtime-state/)
  - [lazy data sources](https://genomespy.app/docs/grammar/data/lazy/)
  - [genomic coordinates](https://genomespy.app/docs/grammar/genomic-coordinates/)
  - [releases](https://github.com/genome-spy/genome-spy/releases)
  - [genome-spy-python](https://genomespy.app/genome-spy-python/)
- Chimaera:
  - [extension guide](../agent-guides/plugins.md)
  - [platform plan](plugin-platform-plan.md)
  - [PowerPoint plan](pptx-plugin-plan.md)
  - [agent browser](../features/agent-browser.md)
  - [files and previews](../features/files-and-previews.md)
