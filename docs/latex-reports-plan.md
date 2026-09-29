# LaTeX and Typst: the first plugins on the platform

Dated 2026-09-25, revised 2026-09-26, 2026-09-28 and 2026-09-29. A plan, not a
record: nothing here has shipped. It covers two workbench plugins, **LaTeX** and
**Typst**: building documents on the host, showing the PDF beside the source,
jumping between the two, turning compile errors into editor marks an agent can fix,
showing what changed, installing a TeX Live when the host has none, teaching agents
to write reports here, and turning markdown into a PDF. They are the first
privileged plugins on the [plugin platform](plugin-platform-plan.md), which owns
everything generic (screens, programs, side-program installs, output folders, data
surfaces, change bars, trust). This plan is what is specific to LaTeX and Typst, and
which platform pieces they use. It was split out of the documents plan
(`docs/document-workbench-plan.md`, shipped as martinappberg/chimaera#159) and built
from reads of the tree (last at `4b2f9a1`) plus a survey of TeX Live, latexmk,
Tectonic, TinyTeX, Typst, SyncTeX, pandoc and the tools that already do this (VS
Code's LaTeX Workshop, texlab, tinymist, Overleaf's compile limits). Claims about
the current code are traced in the [appendix](#appendix-what-the-code-does-today);
outside facts are in [sources](#sources).

## Decisions

**Current (maintainer, 2026-09-29): the platform route.** LaTeX and Typst are
plugins, the first on the plugin platform, which lets plugins draw screens in the
Chimaera format, run declared programs and install the side programs they need, with
verified plugins installing freely and anything else needing explicit trust
([platform decisions](plugin-platform-plan.md#decisions-maintainer-2026-09-29)).

How it got here: on 2026-09-26 this plan made them plugins on a `build` contribution
point; on 2026-09-28 the maintainer made them core document support instead, kept
lean, because the plugin host then could not run programs, draw screens or wait for a
build; on 2026-09-29 the maintainer chose to grow the plugin host into a platform that
can, so the plugins return, now with nothing LaTeX-shaped in core.

Decided along the way, and still in force:

1. **LaTeX builds with TeX Live through latexmk.** The host's own TeX Live always wins.
2. **No Tectonic** (2026-09-28): a second, older LaTeX would build documents
   differently than TeX Live does for co-authors and journals
   ([why](#latex-the-hosts-tex-live-or-tinytex)).
3. **One-click TeX Live through TinyTeX** (2026-09-29) where the host has none: a real,
   current TeX Live in the home folder, now declared as the LaTeX plugin's side program
   ([installs](#tinytex-and-typst-as-side-programs)).
4. **Build output goes to a folder outside the repository**, capped: now the plugin's
   output folder under the platform's quota.
5. **Build on open and when an agent edits a file**, with a per-document switch.
6. **TeX Live's restricted shell escape stays** as the site configured it.
7. **Changes compare against the last commit** by default.
8. **Typst is the recommended format for new agent reports**; LaTeX for a journal
   template or an existing project.
9. **Markdown to PDF starts with a small Typst template**, now an action the Typst
   plugin adds to markdown files.
10. **The agent tools are pre-allowed** where the plugins are on, like every plugin
    tool.

Superseded by the platform route: the core build module, `check_document` building
`.tex` and `.typ` (the plugins bring their own tools), and the reworked Settings →
Documents panel (each plugin's settings and tools appear in Settings → Plugins).

## The short version

- **Two plugins, installed like any verified plugin.** `chimaera-plugin-latex` and
  `chimaera-plugin-typst`, each its own repository, pinned in `plugins/plugins.lock`.
  Opening a `.tex` or `.typ` file where the plugin is missing offers "Install the LaTeX
  plugin (verified)" in one click.
- **Privileged, and says so.** Both run programs, so their cards list exactly which
  ("Runs on this host: latexmk, pdflatex, xelatex, lualatex, bibtex, biber, tlmgr"),
  what reaches the network ("tlmgr: CTAN mirrors") and what they may download
  ("TinyTeX 2026.09, 152 MB"). The maintainers review every release, and the artifacts
  it pins, before the lock pins it.
- **Your engines first, then one click.** LaTeX uses the host's TeX Live (whatever
  `module load texlive` puts on PATH in a terminal), else the plugin's TinyTeX. Typst
  uses your `typst`, else the plugin's. The platform resolves programs; the user's copy
  wins.
- **Source and PDF side by side.** The plugin's file view is a split of the platform's
  `editor` and `pdf` components. Saving builds (debounced, one job at a time, niced,
  time-limited, output capped), and so does an agent's write to any file of the
  document while it is open.
- **Errors become editor marks.** The plugin parses the log and publishes
  diagnostics; the platform draws the marks, the problems list and **Ask agent**.
- **Missing LaTeX packages install on the spot** into the plugin's TinyTeX.
- **Jumps both ways.** The plugin turns SyncTeX into a source map; the platform's PDF
  component jumps with it, and selections become `@report.tex#L120-L128 "quote"`
  references. Typst gets text matching.
- **Multi-file projects work**: the main file from a `% !TEX root` comment, a
  `\documentclass`, or the last build's inputs.
- **Changes are visible** through the platform's change bars and PDF change marks,
  against the last commit, a branch or the previous build.
- **Agents check their own work** with the plugins' own tools, `compile_latex` and
  `compile_typst`, which wait for the build and return the errors.
- **Markdown to PDF** is an **Export PDF** action the Typst plugin adds to markdown
  files.
- **Safe on a login node**: the platform's job limits, plus the LaTeX rules (no
  unrestricted shell escape, no project Perl without consent, the old-LuaTeX gate).

## Principles

1. **Compile where the work is.** Sources, figures and agents live on the host; the
   build runs there, and only the pages you look at cross the tunnel.
2. **The host's toolchain is the truth.** The same TeX Live the user's terminals get,
   through the same environment prelude, so a document builds here as it does for
   co-authors and cluster jobs.
3. **Nothing LaTeX-shaped in core.** Every LaTeX or Typst decision lives in the plugins.
   Every generic piece they need (a runner, a view, a source map, change bars) is the
   platform's, available to any plugin.
4. **Never litter the repository.** Aux files, logs and PDFs stay in the plugin's
   output folder until the user asks for a PDF beside the source.
5. **Opening a file never runs project code without consent.** A project `latexmkrc`
   is Perl; unrestricted shell escape is arbitrary commands.
6. **Not an IDE.** No language server, completion or refactoring
   ([DESIGN.md](../DESIGN.md#scope-philosophy-and-non-goals)).
7. **One pipeline for people and agents.** A save and an agent's `compile_latex` run
   the same job and update the same view.

## The two plugins

### LaTeX

```toml
id = "latex"
name = "LaTeX"
version = "0.1.0"
api = "0.2"
summary = "Build .tex to PDF beside the source, with errors as editor marks, jumps both ways, and TeX Live on one click."
homepage = "https://github.com/martinappberg/chimaera-plugin-latex"

# latexmk starts the engines and the bibliography tools itself. All are declared, so
# the card names every program that runs and the platform reads their versions (the
# old-LuaTeX gate needs lualatex's). The same as a [[programs]] table per entry.
programs = [
  { name = "latexmk", version = ["-v"] },
  { name = "pdflatex" }, { name = "xelatex" }, { name = "lualatex" },
  { name = "bibtex" }, { name = "biber" },
  { name = "tlmgr", network = "CTAN mirrors (TeX Live packages)" },  # only on its TinyTeX
  { name = "latexdiff" },                                            # the changes PDF, later
]

[access]
files = "read"
timeline = "none"
sessions = "none"

[[tools]]
id = "tinytex"          # artifacts and setup steps as in the platform plan's example
programs = ["latexmk", "pdflatex", "xelatex", "lualatex", "bibtex", "biber", "tlmgr"]

[[files]]
match = ["*.tex", "*.ltx"]
view = "document"
debounce_ms = 800

[[views]]
id = "document"
slot = "file"

[[views]]
id = "trusted"          # the card section listing trusted latexmkrc files
slot = "card"

# [[settings]]: one table per row of section 9

[provides]
mcp_tools = ["compile_latex", "latex_guide"]
surfaces = ["diagnostics", "output", "sourcemap"]
events = ["file-saved", "file-changed", "job-finished", "settings-changed"]

[adds]
ui = [".tex opens as source | split | PDF · builds on save · errors as editor marks · Cmd-click the PDF to jump to the line · TeX Live on one click"]
agents = ["compile_latex builds a document and returns its errors · latex_guide: how to write LaTeX here"]

[release]
github = "martinappberg/chimaera-plugin-latex"
```

### Typst

The same shape: one program, `typst`, declared with
`network = "Typst Universe (@preview packages)"` because Typst downloads a package on
its first import; a `typst` tool (the official static binaries per platform);
`[[files]] match = ["*.typ"]` with `debounce_ms = 300`; an
`[[actions]] match = ["*.md"] label = "Export PDF"`; the surfaces `diagnostics` and
`output` (no source map: the `pdf` component matches text instead); and the tools
`compile_typst` and `typst_guide`.

### Which platform pieces they use

| LaTeX or Typst needs | Platform piece ([plan](plugin-platform-plan.md)) |
|---|---|
| run latexmk, typst, tlmgr | programs and jobs (section 6), with their limits |
| TinyTeX and Typst binaries | tools (section 8) |
| build folders | output folders (section 7) |
| `.tex` opens as source and PDF | file kinds and a file view (sections 3 and 5) |
| editor and PDF side by side | the `split`, `editor` and `pdf` components (section 3) |
| errors in the editor | the `diagnostics/1` surface (section 4) |
| the PDF and its status | the `output/1` surface (section 4) |
| jumps and selections | the `sourcemap/1` surface and the `pdf` component (sections 3 and 4) |
| build on save and on agent writes | `file-saved`, `file-changed`, host debounce, the watch set (section 5) |
| a chosen main file, a trusted `latexmkrc` | durable state (section 7) |
| settings | declared settings (section 9) |
| `compile_latex` waiting for a build | long agent tools (section 6) |
| change bars and PDF change marks | the `editor` and `pdf` components (section 3) |
| reading big logs and SyncTeX files | `output-read` from an offset (section 7) |
| absolute paths for `-outdir`, and mapping printed paths back | the `roots` import (section 6) |

## 1. Engines

### Finding them

The platform resolves every declared program on the PATH the user's terminals get
(the host and workspace environment prelude, captured once per prelude change), then
in the plugin's tools, and reports versions through `tool-state`. The plugin never
names a path. It decides between the results.

### LaTeX: the host's TeX Live, or TinyTeX

For a given main file:

1. **A project `latexmkrc`** is honored, its Perl running only after the user trusts
   it ([security](#security)).
2. **The engine** comes from a `% !TEX program = xelatex | lualatex | pdflatex` magic
   comment, else the workspace's setting, else pdfLaTeX.
3. **The host's latexmk and that engine** build it. **Yours always wins**, unless the
   workspace's setting says to use the plugin's TinyTeX (for a cluster whose TeX Live
   is old or short of packages).
4. **Else the plugin's TinyTeX**, once installed.
5. **Otherwise nothing builds yet**, and the view offers the install
   ([below](#when-there-is-no-engine)).

**Why not Tectonic** (decided 2026-09-28). Tectonic is the one LaTeX small enough to
install like an agent (a 10 MB binary that downloads packages on demand), but it is
the wrong second LaTeX:

- Its package bundle was last updated to **TeX Live 2022**, so a document can build
  differently than for co-authors, journals and cluster jobs.
- It is **XeTeX only**: no pdfTeX, no LuaTeX.
- Its bundle ships **biblatex 3.17**, which needs exactly **biber 2.17**; any newer
  biber fails (Tectonic issue 1267, open).
- It defaults to **US letter** paper, and its first build of a document needs the
  network.

A document that builds here should build the same way everywhere else. Where TeX Live
is missing, TinyTeX is the answer: it *is* TeX Live, current, just trimmed.

**Is TeX Live common?** Mostly where LaTeX gets written, and rarely by default:

- **HPC and university clusters:** usually available, as a module or a system
  package; Settings → Environment is where `module load texlive` goes.
- **Linux workstations and servers:** only if someone installed it. It is one
  package-manager command with admin rights (on Debian and Ubuntu `latexmk` is its
  own package), or TeX Live's own installer into the home folder without them.
- **Macs:** not by default. People who write LaTeX usually have MacTeX (about 5 GB) or
  the smaller BasicTeX.
- **Fresh cloud machines and containers:** usually not.

Hence the one-click TinyTeX: without it, a `.tex` file on a laptop or a fresh
machine would open but never build.

### When there is no engine

The document still opens and edits; the result pane shows a calm empty state, drawn
with the platform's `empty` node:

- **No TeX Live on this host.** "No TeX Live on *sherlock*: Chimaera looked on the PATH
  your terminals get, including your environment prelude." The main button is **Install
  TeX Live (TinyTeX, about 150 MB)**, which says what it downloads and from where
  before it runs. Beside it, quietly: "Have your own? On a cluster, load it in
  **Environment settings**" (with a hint such as `module load texlive`, shown, never
  written for the user), and **Check again**. When TeX Live is there but latexmk is
  not, it names the missing package.
- **No Typst on this host.** **Install Typst (17 MB)**, the same way.
- **No plugin at all** (the file is `.tex`, the LaTeX plugin is not installed or not on
  here): the platform's default viewer opens it as text, with one line above it:
  "Install the LaTeX plugin (verified) to build this" or "Switch LaTeX on here".
- **A PDF already sits beside the source** (`report.pdf` next to `report.tex`): show it,
  with "built elsewhere; may be older than the source" when it is older.
- **Agents** get the same facts from `compile_latex` (`no_engine`, what was searched,
  how the user can fix it).

### TinyTeX and Typst as side programs

Both are declared downloads in the plugins' manifests, installed by the platform on a
click ([tools](plugin-platform-plan.md#8-tools-side-programs-a-plugin-installs)).

- **TinyTeX** is TeX Live, current, trimmed to the common packages, portable,
  maintained by the RStudio (Posit) team and released about monthly. Its default bundle
  is about 150 MB for Linux x86_64; its release page also lists Linux arm64, a musl
  build and macOS. The manifest pins one fixed monthly release per platform, never the
  moving `daily` tag TinyTeX's own installer uses, with a sha256 the reviewer takes from
  the artifact (the releases publish no checksums). The setup steps add `latexmk` (and
  biber where TeX Live ships it for the platform) with `tlmgr`. It unpacks under the
  plugin's tools folder with no admin rights, and nothing outside that folder changes:
  unlike TinyTeX's installer, the platform never runs `tlmgr path add` or edits PATH.
- **Typst** is one static binary per platform (16.7 MB for Linux x86_64 at 0.15.1),
  pinned the same way.
- **Updates** come with plugin releases: a new TinyTeX month or Typst version is a new
  plugin version, reviewed before the lock pins it. Within a TeX Live year, `tlmgr`
  updates packages inside the plugin's TinyTeX; a new TeX Live year is a new bundle,
  and the plugin re-adds the packages it installed (it keeps their list in durable
  state).
- **Why not in the binary:** every host would carry about 170 MB it may never use (the
  chimaera binary is under 40 MB), and every deploy over ssh would move it. Installing
  on the host that builds, when a document needs it, gives the same result.

### The LaTeX command

The plugin starts one job per build, with the main file's folder as the working
directory:

```
latexmk -pdf | -xelatex | -lualatex
        -interaction=nonstopmode -file-line-error -synctex=1 -recorder
        -outdir=<out> -norc [-r ~/.latexmkrc] [-r <project rc, if trusted>]
        main.tex
```

`<out>` is the document's build folder (`output:<key>/`,
[below](#where-build-output-goes)) as the absolute path the platform's `roots` gives.
Plus `max_print_line=10000`, `error_line=254`, `half_error_line=238` and
`TEXMFOUTPUT=<out>` in the job's environment. Kpathsea lets an environment variable
override any `texmf.cnf` value, so TeX stops wrapping log lines at 79 characters (the
main source of log-parser bugs), and `openout_any = p` allows writes into the absolute
build folder.

- **Always name the main file.** Given none, latexmk builds every `.tex` in the
  folder.
- **Always `-norc`.** latexmk reads rc files from the system, the user, and the
  *current folder*, and they are Perl. `-norc` is scanned before any rc file is read.
  The user's own rc is added back with `-r`; a project rc only after trust.
- **No `-halt-on-error`** and no `-silent` (which switches to batch mode): nonstop mode
  keeps going and reports several errors, and the parser ranks the first one.
- **No hand-driven passes.** The plugin never runs `pdflatex`, `bibtex` or `biber` itself.
  latexmk already emulates an aux folder on TeX Live (its `$emulate_aux` default),
  knows when to run biber (a `.bcf` exists) or bibtex (`\bibdata` in the `.aux`), and
  handles the output-folder quirks.

### Typst

`typst` from the user's PATH, else the plugin's copy (0.15.1 is current):

```
typst compile main.typ <out>/main.pdf --root <workspace> --diagnostic-format short
      --jobs 2 --deps <out>/deps.json --deps-format json
```

`--root` is the workspace root, so a document can read figures and data anywhere in
the workspace and, symlinks aside ([security](#security)), nothing outside it.
`--jobs 2` stops Typst from using every core of a 64-core login node. `--deps` writes
the list of files the build read, which becomes the watch set. `<out>` is as for
LaTeX.

A long-lived `typst watch` process would make recompiles near instant, but it is one
resident process per open document on a shared node, and Typst has no memory limit of
its own. Start with one-shot `typst compile` per save and measure a real 20-page report
in L1 (no published benchmark exists); reach for `watch` only if it takes over a
second (the platform's jobs are bounded, not resident, so `watch` would need a platform
change first). If font discovery on a network filesystem turns out slow,
`--ignore-system-fonts` plus the project's own fonts is the knob.

### Missing LaTeX packages

The most common LaTeX error on a small TeX Live is "`siunitx.sty` not found".

- **With the plugin's TinyTeX**, the build fixes it: the log parser names the missing
  file, a `tlmgr search --global --file` job finds the package that provides it, a
  `tlmgr install` job adds it (one package at a time, in the platform's queue, logged in
  the plugin's activity), and the build reruns once. The status says "installed
  siunitx". A setting turns this off, and then the error offers an **Install siunitx**
  button instead. It needs the network; offline, the error says so.
- **Signed packages.** The plugin asks `tlmgr` to require TeX Live's signature on its
  package list (`--verify-repo=main`), which needs `gpg` on the host. Without `gpg`,
  `tlmgr` can check only the packages' hashes from the same mirror, so packages then
  install only on a click, and the button says the signature could not be checked.
- **With the user's own TeX Live**, the plugin never changes it. The error names the
  package and how to get it: ask the admins, load a fuller TeX Live module, or switch
  this workspace to the plugin's TinyTeX.
- **Agents** see the same through `compile_latex`.

## 2. The document view

### Opening

- The plugin's file view is a platform `split`: the `editor` component (the shared
  editor, its buffers, saves and merges exactly as for any file) and the `pdf`
  component, with a **source | split | PDF** toggle, remembered per file. Split is the
  default when the pane is wide enough and an engine exists; otherwise PDF.
- LaTeX highlighting is already in the tree (`@codemirror/language-data`'s lazy `stex`
  legacy mode). Typst highlighting is `codemirror-lang-typst` (Apache-2.0, a WASM-free
  Lezer grammar, about 38 KB gzipped), loaded lazily by the editor for `.typ`; the
  richer Lezer LaTeX grammar on npm is AGPL, so it stays out. (A grammar is a core
  editor asset, like every other language the editor highlights.)
- Opening a document with no current build builds it once, so the PDF side is never
  empty for long (one exception: LuaLaTeX on an old, vulnerable LuaTeX; see
  [security](#security)). A current build in the output folder shows at once.

### Building on save

- **Triggers:** `file-saved` for a claimed file; `file-changed` for any file in the
  document's watch set while it is open (an agent editing a chapter, a regenerated
  figure, a new `refs.bib` entry), debounced by the platform (800 ms for LaTeX, 300 ms
  for Typst); the **Build** button; an agent's `compile_latex`.
- **One job at a time per plugin**, in the platform's daemon-wide queue. A trigger
  during a build sets a pending flag in the plugin's state, and the build reruns once
  when the job finishes.
- **No cancel on a newer save.** Killing LaTeX mid-pass leaves half-written aux files
  and costs more than it saves; timeouts still kill (the platform's limits: 180 s for
  LaTeX, 60 s for Typst, both settings).
- **A per-document switch** turns building off; **Build on open** and **Build when an
  agent edits** are workspace settings (on).
- **Status**, through the `output` surface: `building…`, `built 1.2 s · 14 pages`,
  `2 errors · 5 warnings`, `built by <agent name>`.

### Where build output goes

- The plugin's output folder, `output:<key>/`, where `<key>` is a short hash of the main
  file's path plus its stem (`3f9a2c1e-report/`). It holds the PDF, log, aux, `.fls`,
  `.synctex.gz`, the dependency list, and a `prev/` copy of the previous good build's
  PDF, source map and source inputs (up to 8 MB), for changes against the previous
  build.
- It survives restarts (a warm latexmk rerun is often a single pass) and never lands
  in the repository. The platform's quota applies (1 GB by default), with **Clear** in
  Settings.
- **Save PDF beside source** is the platform's `save-to-workspace` action, on the user's
  click; a per-document **keep a copy beside the source** switch does it after every
  good build. **Download** uses the existing download ticket.

### Refreshing the PDF over a slow tunnel

The platform's `pdf` component swaps a changed output in place (no flash, the scroll
anchored to the same page and fraction) and loads only the byte ranges visible pages
need, as `PdfView` already does. A failed build keeps the last good PDF with an error
bar; **show partial output** switches to the partial PDF LaTeX often leaves.

### Errors as editor marks

- **The parser is the plugin's** (Rust, compiled into its component), run in the
  `job-finished` handler. It streams the log through `output-read`, 1 MiB at a time
  with a 16 MB scan cap, and publishes `diagnostics/1`. It follows the TeX file
  stack through parentheses, reads `-file-line-error` prefixes, and recognizes
  `! LaTeX Error`, `! Undefined control sequence` with its `l.<n>` context line,
  `Missing $ inserted`, `Runaway argument`, `Emergency stop`, missing files and
  packages, `LaTeX Warning: Reference/Citation … undefined`, overfull and underfull
  boxes, package warnings, and `biber`/`bibtex` messages from the `.blg`. Typst's
  short diagnostic format (`file:line:col: error: message`) needs almost no parsing
  and carries exact columns.
- **Output**: the platform's `diagnostics/1` shape, at most 200 per file plus counts,
  keyed by workspace path (the plugin maps the log's paths with `roots`). An error
  inside a class or package file maps to the nearest user file on the stack.
- **Heuristics are ported, not invented.** LaTeX Workshop's log parser (MIT) has
  years of edge cases: one pattern covers both the `file:line:` and the `!` error
  forms, and it tracks the file stack by counting parentheses. texlab's parser
  re-joins lines of exactly 79 characters, kept as a fallback for a site whose
  configuration ignores `max_print_line`. A fixture corpus of real logs (pdflatex,
  xelatex, lualatex, biber, bibtex; Typst's in its own plugin) lives in the plugin's
  repository and runs as a plain native `cargo test`, since the parser is ordinary
  Rust.
- **In the editor**: the platform's `editor` component draws them as gutter marks and
  underlines (`@codemirror/lint`, already a dependency, used by the settings JSON
  editor). Marks map through later edits and clear on the next publish. Overfull and
  underfull boxes are published as `info`, hidden by default behind a filter; they are
  noise until the end.
- **Problems list**: the platform's `diagnostics` component under the PDF, grouped by
  file. Clicking an entry opens the file at the line (the documents plan's Phase 1
  "open at the spot").
- **Special cases get plain words.** A missing package is installed or explained
  ([missing packages](#missing-latex-packages)). "biber 2.19 does not match biblatex
  3.20; use the biber from the same TeX Live." "Package
  `@preview/cetz:0.3.4` is not cached and this host is offline."
- **Log text is untrusted.** It contains text from the document. The platform's
  components show it as text, never as HTML ([web-UI rules](../.claude/rules/web-ui.md)).

### Ask the agent to fix it

The platform's `diagnostics` component carries **Ask agent** on each entry: it types
one line into the current reference target, never with a newline:

```
@chapters/intro.tex#L12 LaTeX error: "Undefined control sequence \unit" at "l.12 ...\unit{mg}"
```

**Ask agent to fix all** on the status types
`Fix the 3 compile errors in @report.tex (compile_latex lists them) `. Both work for
agents without the plugin's tools too: the path, line and message are plain text.

## 3. Source and PDF: SyncTeX

### A source map per build

latexmk (`-synctex=1`) writes `main.synctex.gz` into the output folder. When the job
finishes, the plugin's `job-finished` handler (30 s) streams it through
`output-read`, decompresses as it goes (a thesis's SyncTeX can be tens of MB
uncompressed, and the instance has 64 MiB), and reduces it to the platform's
`sourcemap/1`: per source line, the boxes it produced on each page. It publishes the
map, and the platform's `pdf` component answers every jump locally, in the browser, so
a click never waits on the tunnel or on the plugin.

- **The format.** The file stores scaled points plus a unit and offsets from its
  preamble; there are 65,781.76 scaled points to a PDF point, and SyncTeX measures from
  the page's top left while PDF measures from the bottom left. Input paths are as the
  engine saw them, absolute or relative; the plugin maps them to workspace paths.
  LaTeX Workshop's `synctexjs.ts` (MIT, a port of synctex-js) is the reference for the
  format.
- **Cap.** A map over the surface's 4 MiB is dropped: jumps are off for that build,
  with a note.

### Forward: source to PDF

- **Mod+J** (the platform's jump action, bound wherever an `editor` sits beside a
  `pdf` with a source map), a toolbar button, or **follow cursor** (off by default,
  debounced about 250 ms) takes the cursor's line to its boxes in the PDF.
- The `pdf` component shows the boxes: it scrolls so the first box sits a third of the
  way down if it is not already visible, then draws a translucent accent highlight that
  fades after about 1.5 s. The page viewport's `convertToViewportPoint` turns box
  corners into screen positions, as LaTeX Workshop's viewer does, which also handles
  zoom and a non-zero MediaBox origin.

### Inverse: PDF to source

- **Cmd/Ctrl-click** on the PDF (a plain click stays text selection) maps page, x and y
  to file, line and, when SyncTeX knows it, column. The editor half of the split
  jumps there and flashes the line. A line in another member file opens that file at
  the line (documents plan Phase 1), or the split's editor switches to it in place
  without losing unsaved text (the documents plan's buffers outlive views).
- **Staleness.** SyncTeX describes the last build. The editor keeps the changes made
  since that build and maps line numbers through them, so a jump lands right even
  before the next compile.

### Selections become source references

- Selecting text on a compiled PDF takes the first and last selection rectangles,
  maps both through inverse sync, and publishes an ordinary `FileSelection`
  (`shared/reference.ts`) whose path is the **source** file and whose lines are the
  mapped range. The existing chip and composer then produce
  `@report.tex#L120-L128 "the selected text"`, unchanged.
- A selection that crosses into another file keeps the start file's lines; the quote
  disambiguates. SyncTeX is line-precise, not character-precise, so a range can be off
  by a line at its ends; the quote covers that too.
- With no sync data, the reference falls back to the PDF locator from the documents
  plan: `@…/report.pdf#page=4 "…"`.
- The documents plan's region box (Phase 5) works the same way on compiled PDFs: the
  box's corners map to source lines and the crop is attached as an image.

### Typst

Typst writes no SyncTeX, and its PDF carries no source map. The mapping lives inside
the compiler: the `typst-ide` crate's `jump_from_click` and `jump_from_cursor`, which
tinymist's preview uses. The `typst` command line does not expose them. The lean
answer is the first of three options:

1. **Text matching (L2, recommended first)**, the `pdf` component's mode for an output
   with no source map. Typst prose is very close to its source. Inverse: take the clicked or selected PDF text (pdf.js text layer) and find
   it in the member files, ignoring markup, whitespace and hyphenation; ties go to the
   file the editor shows. Forward: take the words around the cursor and find them in
   the page texts. Headings and plain paragraphs map well; math, tables and generated
   text do not. The UI says "approximate" and never pretends otherwise. L2 also
   tests whether `typst eval` (new in 0.15, replacing the deprecated `typst query`)
   can report heading positions, which would anchor the matching per section.
2. **A small companion binary (only if text matching disappoints).** Built from
   Typst's own crates, it answers exact jump queries. Cost: it pins a Typst version separate from the host's
   `typst`, compiles the document a second time itself, and needs updating with every
   Typst release. It would be one more declared program and tool of the Typst
   plugin. Linking the compiler into the plugin's component instead is ruled out by
   its 64 MiB memory cap.
3. **tinymist, when the host has it.** Its preview does exact two-way jumps, but it is
   a 32 MB, long-running server with its own web preview and its own data plane. It
   could open in the browser pane for users who already use it; it does not fit the
   platform's `pdf` component.

## 4. Multi-file projects

### Finding the main file

For a `.tex` file, first match wins:

1. **A magic comment** in the first 20 lines: `% !TEX root = ../main.tex` (any case,
   with or without the space after `%`). TeXShop, TeXstudio and LaTeX Workshop all
   honor it, so it is what agents are taught to write.
2. **The file has `\documentclass`** before `\begin{document}`: it is a main file. The
   `subfiles` class names its main file in `\documentclass[../main.tex]{subfiles}`;
   the main is built by default, with **build this part alone** as an option.
3. **The last build said so.** latexmk's `-recorder` writes a `.fls` list of every
   file the build read. The plugin keeps a map from file to main in its durable
   state, filled from recent builds (at most 256 entries per workspace).
4. **A bounded search.** `.tex` files with `\documentclass` in the file's folder and up
   to two parents (at most 200 files, first 8 KB each, through the host's `list` and
   `read`) whose `\input`, `\include`, `\subfile` or `\import` lines name this file.
   Exactly one match wins.
5. **Ask once.** Several candidates, or none: a small picker in the toolbar. The
   answer is kept in the plugin's durable state (`state-keep`), per workspace.
6. **Project files.** A trusted `latexmkrc`'s `@default_files` names its own main
   files.

For a `.typ` file: it is its own main unless a recent build's dependency list
(`--deps`) or a static scan of `#include`/`#import` in its folder shows another file
including it. Several candidates: ask once, as above.

A member file shows its main document's PDF, labelled "part of main.tex".

### Includes and figures

- **The watch set** is the main document's inputs inside the workspace: the `.fls`
  `INPUT` lines minus TeX distribution files, or Typst's dependency list. The plugin
  registers it with the platform's `watch` (at most 256 paths). The platform sweeps it
  every 5 s while the document is open, delivers an agent's write at once, and
  debounces both; a change queues a rebuild.
- **Figures** resolve relative to the main file's folder, which is the working
  directory, plus any `\graphicspath`. EPS figures need `repstopdf` through restricted
  shell escape; agents are told to write PDF or PNG instead.
- **`\include` with an output folder** needs matching subfolders in it; latexmk
  handles this.

### Bibliography

- **LaTeX**: latexmk decides between bibtex and biber on its own (from the `.aux` or
  `.bcf`) and reruns as needed. The plugin adds only diagnostics from the `.blg`, and the
  plain-words message for the most common HPC failure: a biber (often from conda)
  that does not match the TeX Live's biblatex.
- **Typst** reads `.bib` (BibLaTeX) or Hayagriva `.yml` natively with
  `#bibliography("refs.bib")`. No extra tool, no extra pass. One more reason to
  recommend Typst for new reports.
- `.bib` files open as text. Saving one recompiles its main document through the
  watch set.

## 5. Agent awareness

Where a plugin is on, its tools and its instruction paragraph reach every session in
that workspace through the chimaera MCP server, pre-allowed at spawn, as for any
plugin. Where it is off, agents see nothing new (the `agent_view` fixtures).

### `compile_latex(path, timeout_s?)` and `compile_typst(path, timeout_s?)`

- `path` may be any member file; the main file is found as in
  [section 4](#finding-the-main-file). A path is relative to the workspace root, or
  absolute inside the workspace.
- **It runs the same build a save runs** and joins one already running for the same
  inputs. It is a platform long tool: the plugin starts the job and returns `wait`;
  the platform holds the agent's call until the job ends (at most about 45 s, under
  the agents' MCP timeouts) and asks the plugin for the final answer. A longer build
  answers `still building`, and the next call picks up the same build.
- **It answers, at most 8 KB:** `status` (`ok`, `errors`, `failed`, `timeout`,
  `no_engine`, `still building`), the engine and version, the main file, the PDF's
  path, pages and size, up to 20 errors as `file:line: message` plus one context line,
  undefined references and citations (up to 10), a count of box warnings, installed
  packages, the pages that changed since the previous build, and the log's path.
- **It updates the user's view**: an open PDF refreshes and the status says which agent
  built it.
- **Log excerpts are data.** They quote the document, which may come from an untrusted
  repository; the answer says so.

### `latex_guide` and `typst_guide`, and the paragraph

- **The paragraph** (short, where the plugin is on): "LaTeX (a plugin the user switched
  on): the user reads your .tex as a PDF beside it. After editing, call compile_latex
  and fix every error it returns. Call latex_guide once for this host's engines and the
  conventions." Typst's says the same, and adds that new reports should be Typst unless
  LaTeX is required.
- **The guides** (at most 8 KB each): this host's engines and which one this workspace
  uses; one main file per report, included LaTeX files starting with
  `% !TEX root = main.tex`, Typst's one `main.typ` that `#include`s the rest; figures as
  PDF for vector plots and PNG at 300 dpi or more, relative paths, no EPS; the paper size
  set explicitly (TeX Live follows its site setting); one bibliography tool per project
  (Typst's `#bibliography("refs.bib")`, or biblatex with biber, or natbib with bibtex);
  no packages that need unrestricted shell escape (`svg`, TikZ externalization, minted
  before version 3), no `\write18`, no absolute paths, no fonts the host lacks, Typst
  `@preview` packages only with a pinned version; build output never written into the
  repository; and the loop: compile, fix errors, then undefined references and
  citations, then the page count.
- **For agents outside Chimaera**, each plugin recommends an agent-side skill pack
  (`[recommends.agent_plugins]`, the shipped **Agent-side plugin** box): the guide as an
  Agent Skill, installed through the agents' own plugin managers, never required.

### Later: `render_page(path, page)`

One page as a PNG (MCP image content) so multimodal agents can see a figure running
off the page. Typst renders PNG itself (`--format png --pages N --ppi …`); LaTeX PDFs
need `pdftoppm`. Only if agents turn out to need it.

## 6. Markdown to PDF, and Word (later)

Agents write the portable markdown dialect. Some of it should leave as a real report:
a title block, page numbers, a table of contents, numbered figures. The answer uses
what the Typst plugin already has, a Typst build, and adds no converter anywhere.

| Option | Needs on the host | Verdict |
|---|---|---|
| Browser print of the reading view | nothing | Keep for "what I see"; not typeset (no running heads, no page-aware floats). |
| **A small Typst template that reads the markdown itself** (`cmarker` 0.1.10 for markdown, `mitex` for math) | typst 0.15 or newer | **Recommended first.** A few KB of template in the Typst plugin, no parser. Typst fetches the two pinned packages on first use and caches them. Gaps: no GitHub alerts (they read as quotes), and cmarker's raw-Typst comments must be turned off in the template. |
| pandoc to Typst (`--pdf-engine=typst`, since pandoc 3.1.2) | pandoc (3.11, a 33 MB download) + typst | Offered when pandoc is present; GitHub alerts are on by default for `gfm` input. Never required. |
| Quarto (`format: typst`) | Quarto (140 MB; bundles pandoc, Typst, Deno) | Too heavy to depend on. |
| A markdown-to-Typst writer in the plugin | typst | Full control and parity with the reading view, but several hundred lines of converter. Only if the template's gaps turn out to matter. |

**Export PDF**, the Typst plugin's action on markdown files, runs one job in the
platform's queue (`typst compile -` with the template on standard input, the
markdown's path passed as `--input src=…`, and the workspace root as `--root`) into
the plugin's output folder, opens the PDF beside the markdown, and offers **Save
beside source**. Frontmatter `title`, `summary` and `updated` fill the title block;
`template: path/to/mine.typ` picks a project's own template. On a host without
network the first export needs the two packages pre-seeded in Typst's package cache,
and the error says so.

**Word, later, as its own plugin.** Word files open read-only today (`DocxView`). A
small pandoc plugin, privileged like these two, would add **Export to Word**
(`pandoc report.md -o report.docx`, with a project's `--reference-doc` when it has
one) and **Open as markdown** for a `.docx`
(`pandoc report.docx -t gfm --extract-media=figures`) on hosts that have pandoc.
Editing a `.docx` in place, styles and tracked changes kept, needs an OOXML editor that
fits neither the licence nor the footprint; it stays out.

## 7. Limits and security on a login node

### Running the engine

Every limit but two is the platform's, the same for any plugin's job
([programs](plugin-platform-plan.md#6-programs)). The plugins choose the wall time and
Typst's threads.

| Limit | Value | Who |
|---|---|---|
| Concurrency | one build at a time per plugin, two jobs daemon-wide; a trigger during a build reruns it once | the platform's queue; the plugin's pending flag |
| Priority | nice 10; idle I/O class where allowed | the platform |
| Wall time | 180 s LaTeX (Overleaf's self-hosted default), 60 s Typst; a setting, up to the platform's 600 s | the plugin asks; the platform kills the whole process group |
| CPU time | wall limit plus slack | the platform (`RLIMIT_CPU`, a backstop) |
| Memory | 4 GB address space | the platform (`RLIMIT_AS`); see below |
| File size | 256 MB per written file | the platform (`RLIMIT_FSIZE` stops a runaway `\write` loop filling the disk) |
| Build folders | 1 GB per plugin, least recently used document evicted | the platform's output quota |
| Daemon memory | engine output goes to files, not pipes; the plugin streams the log in 1 MiB reads | no whole-log reads |
| Threads | `typst --jobs 2` | the plugin, leaving cores for everyone else |

The platform runs each job in its own process group (so a timeout kills latexmk and
every pass it started), with stdin closed (so an error prompt can never wait for
input), without ever blocking the daemon's reactor.

**Why every limit is needed.** TeX has no time limit of its own: `\def\x{\x}\x` spins
forever, so only the wall clock stops it. Typst stops a `while` loop after 10,000
rounds and caps call depth, but it has **no memory limit**: an open issue shows one
expression eating tens of GB, and another a 300-page document reaching 32 to 41 GB.
On a shared login node that is an outage, so the memory cap is not optional. L1
checks that 4 GB of address space does not break normal Typst builds (its threads
reserve address space); if it does, the job asks for the platform's fallback, a user
cgroup (`systemd-run --user --scope -p MemoryMax=…`) where the host has user systemd,
and a plain wall-clock limit where it does not.

### Security

- **Privileged, and reviewed.** Both plugins run programs, so they are verified only at
  versions the lock pins, after a person reviewed the release, its provenance and the
  downloads it pins; an update that asks for more waits for the user
  ([platform trust](plugin-platform-plan.md#2-trust-and-verification)).
- **Treat a compile as running code.** Everything below narrows what a document can
  do; none of it makes compiling an untrusted document fully safe.
- **Shell escape.** Unrestricted shell escape (`-shell-escape`) is off and can only be
  turned on by the user, per project, in the UI; never by an agent and never by a file
  in the repo. TeX Live's own default,
  **restricted** shell escape, stays as the site configured it: a short list of helpers
  (`bibtex`, `kpsewhich`, `makeindex`, `repstopdf`, `latexminted` and a few more).
  Documents rely on it for EPS figures and minted code listings. That list has had
  holes (`mpost` allowed arbitrary commands until it was replaced by `r-mpost`,
  CVE-2016-10243); the maintainer kept the restricted default on 2026-09-28 because
  documents rely on it. Typst has no shell escape at all.
- **Old LuaTeX can run commands anyway.** LuaTeX 1.04 to 1.16 (TeX Live 2017 to 2022
  and the first TeX Live 2023) could run shell commands even with shell escape off
  (CVE-2023-32700) and open network sockets (CVE-2023-32668). HPC modules are often
  old. The platform reports `lualatex`'s version; below 1.17.0, a LuaLaTeX document
  never compiles on open, on an agent's write or for an agent's `compile_latex`, only
  on the user's own save or **Build**, and the status (or the tool's answer) says why.
- **Project code.** A project `latexmkrc` is Perl. It runs only after the user trusts
  it for this workspace, and the trust is tied to the file's content hash, so an edit
  (by anyone, including an agent) asks again. The view shows a callout with the file
  and a **Trust this latexmkrc** button; the plugin keeps the answer in its durable
  state, bound to the hash. Until then the build uses `-norc` plus the user's own rc.
  The environment prelude page already calls a checked-in prelude file a supply-chain
  vector; this is the same rule.
- **Writing files.** TeX Live's `openout_any = p` (its default, set explicitly in the
  compile environment) keeps TeX from writing dot files, climbing with `..`, or writing
  to absolute paths outside `TEXMFOUTPUT` (the build folder). Typst writes only the
  output file.
- **Reading files.** TeX can `\input` any file the user can read and typeset it into
  the PDF. `openin_any` no longer has any effect in current TeX Live, so TeX itself
  cannot be told otherwise. The agent could read those files anyway; it matters for
  documents from untrusted repositories whose PDFs get shared. Typst cannot read
  outside `--root`, except through a symlink inside the root (an open Typst issue).
  Real read confinement needs the operating system: Landlock (unprivileged, on newer
  kernels) or bubblewrap where user namespaces work. That is the platform's later,
  opt-in hardening step, not a default, because many HPC kernels have neither
  ([its open decisions](plugin-platform-plan.md#17-open-decisions)).
- **Network.** TeX Live's pdfTeX and XeTeX never touch the network (old LuaTeX: see
  above). Typst downloads `@preview` packages on first import; that is the engine
  fetching its own packages, allowed. Typst has no offline switch, but a cached package
  never touches the network, and a missing one on an offline host becomes a
  plain-words diagnostic. Chimaera does not sandbox the network itself, for the same
  reason as reads; the cards say what reaches it.
- **Installs.** TinyTeX and Typst are the plugins' declared tools: fixed releases from
  their official release pages over HTTPS, each checked against the sha256 in the
  reviewed manifest (TinyTeX publishes none of its own), unpacked by the platform's
  safe unpacker under `~/.chimaera/tools/<plugin>/`, on a click, in a visible terminal,
  never with sudo, never touching PATH. Missing packages come through `tlmgr` from TeX
  Live's own repository, only into the plugin's TinyTeX, one bounded job per package,
  signature-checked where the host has `gpg`.
- **Environment hygiene.** Jobs get the platform's captured prelude environment, minus
  the daemon's own variables and anything on `api::spawn_env_remove`. No token ever
  reaches an engine.
- **Routes.** The plugins add none. The platform's routes are bearer-authed, outputs
  are served through short-lived `/raw` tickets, and **Save PDF beside source** is the
  platform's action, confined to the workspace.

## 8. Changes: git differences in the source and the PDF

Reports change in two places at once: the source an agent edits and the PDF a person
reads. The generic machinery is the platform's, because every document plugin (and
every text file) benefits: the `editor` component's change bars, which compare words
within paragraphs so a rewrapped paragraph is not marked whole, and the `pdf`
component's change marks beside the lines the bars mark
([screens](plugin-platform-plan.md#3-screens-in-the-chimaera-format)). What the plugins
add:

- **The base.** A **Changes** toggle in the document's toolbar with a base picker,
  remembered per document: **last commit** (the default when the document is in git),
  **staged**, **a branch** (its merge base, for "what this branch changes"), **a
  commit** from the file's history, or **the previous build**, from the `prev/` copy the
  plugin keeps (no git needed, so it works for a report nobody has committed). The git
  bases use the platform's `rev=` diff route and file log.
- **Marks on the PDF.** LaTeX's source map places them; Typst's come from text
  matching. Deleted text shows as a small mark in the margin whose hover shows the
  removed words. The page indicator gains "3 changed pages" and ‹ › buttons.
- **Where changes show up elsewhere.** The `output` surface carries the pages that
  changed since the previous build; the platform shows "thesis.pdf · 3 pages changed"
  on the Timeline turn that wrote the source, and the source-control panel's row for a
  `.tex` or `.typ` file gains **Changes in PDF**.
- **Pointing at a change.** Selecting inside a mark makes the usual
  `@thesis.tex#L120-L128 "…"` reference; the hover's **Ask agent** types one capped
  line with both sides:
  `@thesis.tex#L120-L128 changed since HEAD: "the effect held" → "the effect held in 3 of 4 cohorts" `.

**Later, if asked for:** a side-by-side before-and-after of two builds (a second build
of the base revision, whose inputs the platform writes into the output folder with
`git show`), and a **changes PDF** for co-authors: a `latexdiff --flatten` job writes a
marked-up `.tex` from the old and new sources and the same pipeline builds
`thesis-changes.pdf`, insertions underlined and deletions struck (latexdiff stumbles on
some tables and custom macros, and the status would say so). Typst has no latexdiff.

## 9. Settings

Declared in the manifests, drawn by the platform in Settings → Plugins → LaTeX (and
Typst), searchable, and linked from each card:

| Setting | Scope | Default |
|---|---|---|
| Build when a document opens | workspace | on |
| Build when an agent edits a document | workspace | on |
| Engine (pdfLaTeX, XeLaTeX, LuaLaTeX) when no magic comment says | workspace | pdfLaTeX |
| TeX Live: **automatic** (yours, else the plugin's TinyTeX) or **the plugin's TinyTeX** | workspace | automatic |
| Install missing packages automatically (the plugin's TinyTeX only; signature-checked, so it needs `gpg`) | host | on |
| Unrestricted shell escape (with a plain warning) | workspace | off |
| Wall time for a build | host | 180 s (Typst 60 s) |

Plus the platform's own rows for every plugin: the output folder's use with **Clear**,
the tools (TinyTeX's version and size, **Update**, **Remove**), and the activity log.
Trusted `latexmkrc` files are listed in the plugin's card section, each with
**Revoke**.

## What lives where

| Where | What |
|---|---|
| `chimaera-plugin-latex` (its own repository) | `src/plan.rs` (main file, engine choice, the latexmk arguments, the old-LuaTeX gate, the `latexmkrc` consent), `src/log.rs` (the log parser and its fixture corpus under `tests/logs/`, run by plain `cargo test`), `src/fls.rs` (inputs → the watch set), `src/synctex.rs` (the streaming reduction to `sourcemap/1`), `src/packages.rs` (missing packages → `tlmgr` jobs), `src/view.rs` (the file view's tree), `src/tools.rs` (`compile_latex`, `latex_guide`), `plugin.toml` with the TinyTeX artifacts, and the agent-side skill in `.claude-plugin/` and `.codex-plugin/` |
| `chimaera-plugin-typst` (its own repository) | the same shape: the command, the short diagnostic format, `--deps`, text-matching hints, the markdown **Export PDF** action and its template, `compile_typst`, `typst_guide`, the Typst binaries in `plugin.toml` |
| this repository | nothing LaTeX- or Typst-specific: the platform ([what changes where](plugin-platform-plan.md#14-what-changes-where)), the `codemirror-lang-typst` grammar as an editor asset, and the lock entries once the first releases are reviewed |

### How this fits the documents plan

The documents plan shipped in martinappberg/chimaera#159, so what this plan leans on
is there:

- **Open a file at a line** (its Phase 1) is what the problems list and cross-file
  inverse search use.
- **Buffers that outlive views** (its Phase 0) let the split switch member files
  without losing an unsaved chapter.
- **Embeds** (its Phase 4): an embed of `report.typ#page=2` can show the compiled
  page once the embed card reads the `output` surface to find a document's PDF (a
  later step).
- **Point at anything** (its Phase 5): the region box and file references work on
  compiled PDFs, with source lines attached.
- **The dialect and `check_document`** (its Phase 6) stay markdown's. The plugins
  bring their own tools for their formats, so `check_document` is unchanged.

## Phases

The plugins need the platform's screens, surfaces and file kinds (its P7) and its
programs and tools (its P8). Their own work:

### L1: build, errors, agents

**Built and released (2026-09-29): 0.1.0 of each, pinned in `plugins/plugins.lock`; LaTeX
0.1.1 the same day (a rebuild after a failed build runs latexmk with `-g`, so an installed
missing package builds; the install offer names this host's download size).**
Both plugins as repositories of their own
([chimaera-plugin-latex](https://github.com/martinappberg/chimaera-plugin-latex),
[chimaera-plugin-typst](https://github.com/martinappberg/chimaera-plugin-typst)), verified live against a
headless daemon and in the browser, light and dark: the view (a status pill,
Split / Source / PDF, Build, Save PDF, Log, **Ask agent to fix**; a notice bar only
when something needs the user; the PDF kept through a rebuild and refreshed in
place; the problems list for this document only), builds on open, save and agent
edits, the LaTeX log parser against real TeX Live 2026 logs (pdfLaTeX, XeLaTeX,
LuaLaTeX, biber) with the failing control sequence marked, Typst's diagnostics,
parts found by `% !TEX root`, the recorder file or Typst's deps, TinyTeX and Typst
installed on one click with progress, a missing package looked up and installed
with `tlmgr` (verified on macOS with the network: 0.1.1 then rebuilds with `-g`), `compile_latex` / `compile_typst` and the guides. Where it differs: a
failed build shows the partial PDF LaTeX left (no `prev/` copy yet); the user's
`~/.latexmkrc` isn't read (`-norc` only); the old-LuaTeX gate is not built. Typst
highlights in the editor through `codemirror-lang-typst`'s Lezer grammar
(`web-ui/src/lib/previews/languages.ts`: its parser and editing aids in the app's
`--syn-*` colors, not its own styles or syntax linter; about 29 KB gzipped, loaded
for `.typ` only). A file the plugin claims where it is installed but off offers
**Turn on** in the file bar. Verified on a Sherlock login node (2026-09-29, a musl
daemon under `$SCRATCH`): with `module load system` and `module load texlive` (TeX Live
2019, latexmk 4.65; the two must be separate commands there) in the host's Environment
prelude a build runs the module's TeX Live (`from: path`), and still does once TinyTeX
is installed; TinyTeX's Linux `.tar.xz` (152 MB, 415 MB unpacked) installed in 59 s with
the daemon's RSS flat (110 MB before and at its peak); an endless document stopped at a
15 s limit with nothing of its group left running. A claude and a codex session each ran
the compile-and-fix loop (macOS, 2026-09-29).

Both plugins with the file view, building on open, save and agent writes, the output
folder, both log parsers and their corpus, the `diagnostics` and `output` surfaces,
**Ask agent**, `compile_latex` / `compile_typst` and the guides, TinyTeX and Typst as
tools, missing packages, the settings, **Save PDF beside source**.

**Verification.** In each plugin repository: native tests for the main-file rules, the
engine choice, the log corpus and the Typst diagnostics. In chimaera's tests, against
the privileged fixture: the job limits. Live, on the isolated preview over a real
tunnel, on Chromium and WebKit: install the LaTeX plugin through its verified card;
open a `.tex` on a machine with no TeX Live and install TinyTeX from the empty state;
build a document that needs a package TinyTeX lacks and watch it install and rebuild;
break a macro and send the error to an agent; let a claude and a codex session each
run the compile-and-fix loop. On a login node with `module load texlive`: the host's
TeX Live wins over an installed TinyTeX; a thesis-shaped `\include` project with biber
builds; an infinite-loop document times out; a Typst document that allocates without
bound hits the memory cap. Record what the research could not find: prelude capture
time, Typst time and memory for a 20-page report, and both agents' MCP tool-call
timeouts.

### L2: jumps

The SyncTeX reduction, the `sourcemap` surface, Cmd-click and follow-cursor, selections
to `@file.tex#Lx-Ly`, and text matching for Typst.

### L3: multi-file depth

The watch set from `.fls` and Typst's deps, remembered main files in durable state, the
bibliography messages, switching the split's editor between member files.

### L4: changes

The Changes toggle and its bases, the previous-build copy, the changed-pages field for
the Timeline chip and the source-control action.

### L5: markdown to PDF

The Typst plugin's **Export PDF** action and template.

| Phase | What | Size |
|---|---|---|
| L1 | Build, errors, agents, TinyTeX and Typst installs | large |
| L2 | Jumps both ways, source references | medium |
| L3 | Multi-file depth | small |
| L4 | Changes | small (the platform does the heavy part) |
| L5 | Markdown to PDF | small |

L1 needs the platform's P7 and P8. L2 and L3 need L1 and can run in parallel; L4 needs
L2 (the source map); L5 needs L1. Later, each only when asked for: before-and-after,
the changes PDF, `render_page`, Word through a pandoc plugin, a Typst jump companion.

## Open decisions

Both were decided on 2026-09-29, as recommended:

1. **The default TinyTeX bundle** (about 150 MB on Linux, the common packages already
   in): fewer builds wait on a package download. TinyTeX-1 (about 54 MB) was the
   smaller alternative.
2. **Missing packages install automatically** into the plugin's TinyTeX (where the host
   has `gpg`, so TeX Live's signature is checked): a build that fails for a missing
   `.sty` fixes itself, which is what an agent's compile-and-fix loop needs.

The platform's own open decisions (signing the revocation list, provenance,
privileged updates only through the lock, how long 0.1 stays served, operating-system
confinement for jobs) are in its [plan](plugin-platform-plan.md#17-open-decisions).

## Out of scope

- **LaTeX or Typst in core.** Everything specific lives in the plugins; core gets only
  generic platform pieces.
- **A language server, completion or refactoring** (texlab, tinymist): the DESIGN.md
  non-goal.
- **A WASM engine in the browser**, and **bundling** an engine in the binary.
- **Tectonic** ([why](#latex-the-hosts-tex-live-or-tinytex)).
- **Changing the user's own TeX Live.** The plugin installs packages only into its own
  TinyTeX.
- **Committing, staging or reverting from the changes views.** Git stays read-only
  here.
- **Editing a `.docx` in place**; Word as an output is a later pandoc plugin.
- **Collaborative editing** of a report by several people at once.

## Appendix: what the code does today

From reads of the tree (last at `4b2f9a1`). The plugin host is traced in more detail
in the [platform plan's appendix](plugin-platform-plan.md#appendix-what-the-code-does-today).

- **File kinds.** `.tex` and `.typ` fall through `viewKindFor` to `text` and open in
  `CodeView` (`web-ui/src/lib/previews/files.ts:1014`). LaTeX gets highlighting from
  `@codemirror/language-data`'s lazy `stex` legacy mode; Typst gets none. `.docx`
  opens read-only in `DocxView.svelte` (docx-preview, sanitized, in a shadow root).
- **Split view.** `SplitEditPreview.svelte` owns only geometry. The editor is always
  the first child; turning split off unmounts the preview half. `HtmlView.svelte` is
  the only host.
- **PDF view.** `PdfView.svelte` takes a `path`, opens it with `disableAutoFetch` and
  `disableStream` (`:291-297`, since martinappberg/chimaera#159), has find, an
  outline, links and `scrollToPagePoint` (`:236`), keeps per-path scroll and zoom
  memory, caps rasters and text layers, and remounts on a new file version. It has
  no API to draw a highlight or report a click position.
- **Editor hooks.** `CodeView.svelte` accepts host extensions in an `extra`
  compartment (`:62`, `:99`) and reports the live text through `onDoc`.
  `@codemirror/lint` is a dependency, used by `SettingsJson.svelte:20`.
- **References.** `shared/reference.ts` defines `FileSelection {path, startLine,
  endLine, text}` (`:19`) and `composeFileReference`, which produces
  `@path#Lx-Ly "excerpt" ` and never a newline (`:169`).
- **MCP.** `mcp.rs` `INSTRUCTIONS` (`:121`) covers linked terminals and
  `DOCUMENTS_INSTRUCTIONS` (`:146`) the portable dialect; the base tools are
  `list_terminals`, `run_in_terminal`, `read_terminal`, `document_guide` and
  `check_document` (`:477`), the last two always allowed (`:104`); the Mastermind
  tier adds workspace tools. Claude gets the server through a generated
  `--mcp-config` (`agents.rs`), Codex through `-c mcp_servers.chimaera.url`
  (`launcher.rs`).
- **Plugins** (at `c91b3ca`). A workbench plugin is a WASM component run by the
  wasmtime host in `crates/chimaera-server/src/plugins/`: `runtime.rs` (one engine,
  one instance per plugin and workspace, one call at a time, `CALL_BUDGET` 5 s and
  `KNOWLEDGE_BUDGET` 30 s, `HOST_GRACE` 2 s, `MEMORY_CAP` 64 MiB, at most 64
  instances, `RESULT_MAX` 256 KiB), `hostfns.rs` (the bounded imports), `tools.rs`
  (plugin tools and paragraphs only where active: `owner`, `offered`, `call`),
  `installed.rs` and `releases.rs` (installs from releases or directories, checksums,
  updates, rollback). The WIT world is `chimaera:plugin@0.1.0` in
  `crates/chimaera-plugin-api/wit/chimaera.wit`, with no process or watch import.
  The daemon embeds `plugins/plugins.lock` and no plugin bytes; the two first-party
  plugins live in their own repositories. `provides.views` parses and renders
  nothing; `settings` and `commands` are not manifest keys. The Extensions tab renders
  any manifest through `web-ui/src/lib/plugins/PluginCard.svelte`. The plugin-free
  agent view is pinned by `crates/chimaera-server/src/tests/agent_view.rs`.
- **Core document tools.** `mcp.rs` gives every tier `document_guide` (the fixed text
  of `doc_guide.md`) and `check_document` (`doc_check.rs`, the same checker as the
  reading view's issues chip, which checks any path it is given as markdown), both in
  `ALWAYS_ALLOWED_TOOLS` with `notify`; `DOCUMENTS_INSTRUCTIONS` is the documents
  paragraph every session gets at `initialize`.
- **Git.** `GET /api/v1/git/diff?workspace_id=&path=&mode=` returns two full blobs for
  `unstaged`, `staged` or `head` (each capped at 2 MB; binary detected, never sent);
  `DiffView.svelte` diffs them in the browser with `@codemirror/merge`'s `MergeView`.
  The same package exports `unifiedMergeView` (gutter, inline highlights, optional
  merge controls), `goToNextChunk` and `updateOriginalDoc`. The change colors are the
  `--git-added`, `--git-modified` and `--git-deleted` tokens. There is no route for an
  arbitrary revision or a file's log, and a PDF in git shows as "binary".
- **Timeline and turns.** Each agent turn is a Timeline `episode` entry that knows the
  files the turn wrote; the chat's turn-end block shows a figure strip and file chips.
- **PDF find.** `previews/pdfFind.ts` builds a page's text (`pageText`) and maps
  text offsets to text-layer items (`itemRanges`), which is what the find highlights
  use.
- **Quick-open index.** `quickopen.rs` keeps a bounded, cached file index per
  workspace, served stale and refreshed behind, with `workspace_index_if_free` for
  callers that must never start a walk.
- **Running tools safely.** `compute.rs` has the pattern to copy: `run_checked` with a
  timeout, `kill_on_drop` and capped output (`:678`), a login-shell PATH probe walked
  by `find_on_path` instead of `command -v` (`:340`), and the `CHIMAERA_SLURM_BINDIR`
  test knob.
- **Preludes.** `environment::materialize_prelude` writes the host ⊕ workspace ⊕ launch
  text to `runtime_dir()/preludes/<id>.sh`; `launcher::wrap_login_shell` (`:855`)
  sources it in a login shell before `exec`.
- **Managed installs.** `runtimes.rs` installs agent CLIs from official artifacts with
  checksums, as a visible shell session, under `~/.chimaera/agents/`.
- **Watching and serving.** `fs_watch.rs` caps each window at 64 files and 64 folders
  (`:25-26`). `fs.rs` runs filesystem work behind an eight-permit semaphore (`:97`) and
  serves `/raw/{ticket}` with byte ranges and a 600 s ticket life.
- **Runtime directory.** `chimaera_core::runtime_dir` is `$XDG_RUNTIME_DIR/chimaera`,
  else `/tmp/chimaera-$UID`.

## Sources

Checked 2026-09-25 to 2026-09-29. Several official doc sites were unreachable from the research
environment, so some facts come from the projects' source files and release pages
instead; those links are what is cited.

- **Tectonic** (why it is not supported): [changelog](https://github.com/tectonic-typesetting/tectonic/blob/release/CHANGELOG.md)
  (0.17.0, bundle history, SyncTeX paths),
  [README](https://github.com/tectonic-typesetting/tectonic/blob/master/README.md) (XeTeX
  only), [`-X compile`](https://github.com/tectonic-typesetting/tectonic/blob/master/docs/src/v2cli/compile.md)
  and [`-X build`](https://github.com/tectonic-typesetting/tectonic/blob/master/docs/src/v2cli/build.md)
  flags, [`Tectonic.toml`](https://github.com/tectonic-typesetting/tectonic/blob/master/docs/src/ref/tectonic-toml.md),
  [bundle sources](https://github.com/tectonic-typesetting/tectonic/blob/master/crates/bundles/src/lib.rs),
  [`TECTONIC_CACHE_DIR`](https://github.com/tectonic-typesetting/tectonic/pull/884),
  [biber 2.17 pin, issue 1267](https://github.com/tectonic-typesetting/tectonic/issues/1267),
  [0.17.0 release assets](https://github.com/tectonic-typesetting/tectonic/releases/tag/tectonic%400.17.0).
- **latexmk and TeX Live**: [latexmk.pl 4.88](https://github.com/TeX-Live/texlive-source/blob/trunk/texk/texlive/linked_scripts/latexmk/latexmk.pl)
  (rc order, `-norc`, default files, `$emulate_aux`, bibtex and biber rules),
  [texmf.cnf](https://github.com/TeX-Live/texlive-source/blob/trunk/texk/kpathsea/texmf.cnf)
  (`shell_escape`, the restricted list, `openout_any`, `openin_any`, `max_print_line`,
  environment overrides), [web2c manual](https://github.com/TeX-Live/texlive-source/blob/trunk/texk/web2c/doc/web2c.texi)
  (`-recorder`), texlab running a project's rc:
  [latexmkrc.rs](https://github.com/latex-lsp/texlab/blob/master/crates/parser/src/latexmkrc.rs).
- **Typst**: [0.15.0 release notes](https://github.com/typst/typst/releases/tag/v0.15.0),
  [0.15.1 assets](https://github.com/typst/typst/releases/tag/v0.15.1),
  [CLI arguments](https://github.com/typst/typst/blob/v0.15.1/crates/typst-cli/src/args.rs),
  [jump helpers](https://github.com/typst/typst/blob/v0.15.1/crates/typst-ide/src/jump.rs),
  [loop limit](https://github.com/typst/typst/blob/v0.15.1/crates/typst-eval/src/flow.rs),
  [depth limits](https://github.com/typst/typst/blob/v0.15.1/crates/typst-library/src/engine.rs),
  [no memory limit, issue 3150](https://github.com/typst/typst/issues/3150),
  [300-page memory, issue 8611](https://github.com/typst/typst/issues/8611),
  [symlinks escape the root, issue 5454](https://github.com/typst/typst/issues/5454),
  [package cache and network](https://github.com/typst/packages/blob/main/README.md).
- **tinymist**: [releases](https://github.com/Myriad-Dreamin/tinymist/releases),
  [preview](https://github.com/Myriad-Dreamin/tinymist/blob/main/docs/tinymist/feature/preview.typ).
- **Browser engines**: [typst.ts](https://github.com/Myriad-Dreamin/typst.ts) and its
  [compiler package](https://www.npmjs.com/package/@myriaddreamin/typst-ts-web-compiler)
  (sizes measured from the npm tarball),
  [TeXlyre BusyTeX](https://github.com/TeXlyre/texlyre-busytex),
  [LibrePaper bundles](https://github.com/LibrePaper/wasm-latex/blob/main/docs/bundles.md),
  [BusyTeX](https://github.com/busytex/busytex),
  [SwiftLaTeX releases](https://github.com/SwiftLaTeX/SwiftLaTeX/releases).
- **SyncTeX**: [`synctex` command](https://github.com/TeX-Live/texlive-source/blob/trunk/texk/web2c/synctexdir/synctex_main.c),
  [file format, synctex(5)](https://github.com/TeX-Live/texlive-source/blob/trunk/texk/web2c/synctexdir/man5/synctex.5),
  LaTeX Workshop's [SyncTeX glue](https://github.com/James-Yu/LaTeX-Workshop/blob/master/src/locate/synctex.ts),
  [JS parser](https://github.com/James-Yu/LaTeX-Workshop/tree/master/src/locate/synctex)
  and [viewer mapping](https://github.com/James-Yu/LaTeX-Workshop/blob/master/viewer/components/synctex.ts).
- **Log parsing**: LaTeX Workshop's [latexlog.ts](https://github.com/James-Yu/LaTeX-Workshop/blob/master/src/parse/parser/latexlog.ts),
  texlab's [build_log.rs](https://github.com/latex-lsp/texlab/blob/master/crates/parser/src/build_log.rs).
- **Markdown to PDF**: pandoc's [changelog](https://github.com/jgm/pandoc/blob/main/changelog.md)
  (Typst writer, `alerts`) and [3.11 release](https://github.com/jgm/pandoc/releases/tag/3.11),
  [pandoc-ext/diagram](https://github.com/pandoc-ext/diagram),
  Quarto's [bundled versions](https://github.com/quarto-dev/quarto-cli/blob/main/configuration)
  and [releases](https://github.com/quarto-dev/quarto-cli/releases),
  [cmarker 0.1.10](https://github.com/typst/packages/tree/main/packages/preview/cmarker/0.1.10),
  [mitex 0.2.7](https://github.com/typst/packages/tree/main/packages/preview/mitex/0.2.7).
- **Security**: [CVE-2023-32700](https://www.cvedetails.com/cve/CVE-2023-32700/)
  (LuaTeX shell commands), [CVE-2023-32668](https://github.com/advisories/GHSA-hm67-jh95-48xh)
  (LuaTeX sockets), [CVE-2016-10243](https://ubuntu.com/security/CVE-2016-10243)
  (`mpost` in the restricted list); Overleaf's self-hosted
  [compile timeout default](https://github.com/overleaf/overleaf/blob/main/services/web/config/settings.defaults.js).
- **Editor**: [`@codemirror/legacy-modes`](https://www.npmjs.com/package/@codemirror/legacy-modes),
  [`codemirror-lang-latex`](https://www.npmjs.com/package/codemirror-lang-latex) (AGPL),
  [`codemirror-lang-typst`](https://www.npmjs.com/package/codemirror-lang-typst).

- **Changes and conversion**: [latexdiff on CTAN](https://ctan.org/pkg/latexdiff)
  (`--flatten`, the markup options), [mitex](https://github.com/mitex-rs/mitex) (the
  Rust converter behind the Typst package, Apache-2.0; it converts math and basic text
  commands, not packages or whole documents).
- **TinyTeX**: [release repository](https://github.com/rstudio/tinytex-releases) (the
  bundles and their sizes per platform, the release cadence, `tlmgr` for missing
  packages; checked 2026-09-29; its homepage was unreachable from the research
  environment), its [install script](https://github.com/rstudio/tinytex/blob/main/tools/install-bin-unix.sh)
  (the `daily` tag, no checksum, `tlmgr path add`), and
  [tlmgr](https://github.com/TeX-Live/installer/blob/master/texmf-dist/scripts/texlive/tlmgr.pl)
  (package hashes, `--verify-repo`, signatures only with `gpg`).
- **The platform** these plugins stand on: the [plugin platform plan](plugin-platform-plan.md)
  and its [sources](plugin-platform-plan.md#sources) (Zed, VS Code, browser extensions,
  JetBrains, Raycast, Adaptive Cards, attestations, installers).

**Not verified, measured in L1 instead**: Typst's time and memory for a
20-page report; how long a login shell with `module load texlive` takes on a busy login node; whether
`typst eval` can report heading positions.
