# LaTeX and Typst reports: the plan

Dated 2026-09-25, revised 2026-09-26. A plan, not a record: nothing here has
shipped. It covers compiling LaTeX and Typst documents on the host, showing the PDF
beside the source, jumping between the two, turning compile errors into editor marks
an agent can fix, teaching agents to write reports here, turning markdown into a
polished PDF and, later, Word. It was split out of the documents plan
(`docs/document-workbench-plan.md`, since shipped as martinappberg/chimaera#159; its
"Out of scope" section hands this effort over) and built from a read of the current
tree (commit `f44a8b7`, re-checked at `6ae68b9` after the documents work and the
plugin seam of martinappberg/chimaera#162 landed) plus a survey of Tectonic, TeX
Live, latexmk, Typst, SyncTeX, pandoc and the tools that already do this (VS Code's
LaTeX Workshop, texlab, tinymist, Overleaf's compile limits). The revision makes the
whole effort a pair of [workbench plugins](#the-plugin-shape) on one new
contribution point, adds [Word](#word-and-editing-what-is-not-markdown), and says
[how plugins could live in their own
repositories](#packaging-plugins-in-their-own-repositories). Claims about the
current code are traced in the [appendix](#appendix-what-the-code-does-today);
outside facts are in [sources](#sources).

## The short version

- **Two plugins on one build point.** LaTeX and Typst ship as workbench plugins
  ([the plugin shape](#the-plugin-shape)): two TOML manifests on a new `build`
  contribution point, off by default, saying on their card what they add. The
  runner, the parsers, SyncTeX and the document view are first-party code the
  manifests name; markdown to PDF and Word export are the third and fourth manifests
  on the same point. Nothing changes for an agent in a workspace where neither
  plugin is on.
- **Compile on the host, never in the browser.** The files, the figures and the
  agents all live on the host. The daemon runs the engine there as a small, limited
  child process. Only the PDF crosses the tunnel, and only the pages you look at.
- **Use the host's engines, found through the environment prelude.** LaTeX:
  latexmk from the host's TeX Live if present, else Tectonic. (The request proposed
  Tectonic first; its bundle is frozen at TeX Live 2022, it is XeTeX only, and its
  biblatex needs exactly biber 2.17, so the plan
  [recommends the flip](#the-latex-ladder).) Typst: `typst` if present. Whatever
  `module load texlive` puts on PATH in a terminal is what compiles here. Chimaera
  does not bundle an engine; later it offers an opt-in, visible install of Typst and
  Tectonic, the same way it installs agent CLIs today.
- **Source and PDF side by side.** `.tex` and `.typ` open as source | split | PDF.
  Saving compiles (debounced, one job at a time, niced, time-limited, output capped).
  So does an agent's write to any file of the document while it is open. Build
  output goes to a cache folder outside the repo; the PDF lands beside the source
  only when you ask.
- **Errors become editor marks.** The daemon parses the log into a short list of
  `file:line: message` diagnostics. They show as marks in the editor and a problems
  list under the PDF. Each has **Ask agent**, which types one precise reference into
  the agent's composer.
- **SyncTeX both ways.** Cmd-click the PDF to open the source line; a shortcut or
  follow-cursor highlights the source line in the PDF. Selecting PDF text makes a
  normal `@report.tex#L120-L128 "quote"` reference, so compiled documents speak the
  documents plan's locator grammar. Typst gets a lighter text-matching version first.
- **Multi-file projects just work.** The main file comes from a `% !TEX root`
  comment, a `\documentclass`, or the last build's list of inputs. Editing a chapter,
  a `.bib` file or a figure recompiles the main document.
- **Agents check their own work.** The chimaera MCP server every session loads
  already carries `document_guide` (the portable markdown dialect) and
  `check_document`. Where a build plugin is on, the guide grows an engines and
  conventions section and a `compile_document` tool returns the errors. Agents
  compile, fix and compile again before handing over.
- **Markdown to PDF through Typst.** The portable markdown dialect becomes a polished
  report PDF through a Typst template. Pandoc is optional, never required.
- **Safe on a login node.** No unrestricted shell escape, no project Perl without
  trust, no network except an engine fetching its own packages, hard limits on time,
  memory, CPU priority and output size. TeX can still read any file the user can;
  only the operating system could stop that, so the plan says so plainly.

## Principles

1. **Compile where the work is.** Sources, figures, bibliography and agents are on the
   host. Moving them to the browser to compile costs tunnel bytes and leaves agents
   unable to compile.
2. **The host's toolchain is the truth.** The same TeX Live the user gets in a
   terminal, through the same prelude, so a document that builds here builds for
   their co-authors and their cluster jobs. The prelude text stays opaque: Chimaera
   never parses it ([environment](features/environment.md)).
3. **Never litter the repo.** Aux files, logs and PDFs live in a build cache. The repo
   only changes when the user or an agent asks for a PDF beside the source.
4. **The daemon stays small.** Engines are child processes under hard limits. Logs are
   streamed and capped. SyncTeX is parsed in the browser, not in the daemon
   ([daemon rules](../.claude/rules/daemon.md)).
5. **Opening a file never runs project code without consent.** A project `latexmkrc`
   is Perl; unrestricted shell escape is arbitrary commands. Both need an explicit,
   per-workspace yes.
6. **Not an IDE.** No language server, no completion, no refactoring
   ([DESIGN.md](../DESIGN.md#scope-philosophy-and-non-goals)). Compile, show, jump,
   point, fix.
7. **One pipeline for people and agents.** The agent's `compile_document` and the
   user's save run the same queue, the same limits and the same parser, and update the
   same preview.

## The plugin shape

Since martinappberg/chimaera#162 an opt-in capability is a **workbench plugin**
([plugins](features/plugins.md), [authoring guide](agent-guides/plugins.md)): a TOML
manifest embedded in the binary, switched on per workspace, saying on its card in
words what it adds, with its behaviour in first-party code behind capabilities the
manifest names. With no plugin active, nothing an agent sees changes, pinned by the
`agent_view` fixtures. The authoring guide named LaTeX the next plugin and sketched
it on the specified-but-unbuilt `commands` and `settings` points, where a build is a
visible terminal session. This plan replaces that sketch: compile-on-save cannot be
a terminal per save (a new rail entry every time an agent writes a chapter, no time
or memory limit, no build folder), and a terminal cannot hand back diagnostics, an
output file and sync data. So the plugin's contribution point is a new one, `build`,
and sections 1 to 7 describe what that point does.

### One point, four manifests

A `build` section names the files a plugin builds, the tools it needs, the command,
the output and, by name, the first-party pieces that turn a child process into a
document view. Everything a third party could reasonably vary is data; everything
that must be careful on a login node is code.

```toml
id = "latex"
name = "LaTeX"
summary = "Build .tex to PDF and read it beside the source, with errors as editor marks."

[build]
sources = ["*.tex", "*.ltx"]        # what opens as a document where this plugin is on
root = "tex"                        # named main-file finder (section 4)
inputs = "fls"                      # named watch-set source: fls | typst-deps | none
diagnostics = "latex-log"           # named log parser (section 2)
sync = "synctex"                    # named source-to-PDF mapping (section 3)
output = "{build_dir}/{stem}.pdf"
debounce_ms = 800
wall_s = 180

[[build.engines]]                   # the ladder, first found wins (section 1)
name = "latexmk"
tools = ["latexmk"]
run = "latexmk -pdf -interaction=nonstopmode -file-line-error -synctex=1 -recorder -outdir={build_dir} -norc {root}"
env = { max_print_line = "10000", TEXMFOUTPUT = "{build_dir}" }

[[build.engines]]
name = "tectonic"
tools = ["tectonic"]
run = "tectonic -X compile {root} --outdir {build_dir} --synctex --keep-logs"
env = { TECTONIC_UNTRUSTED_MODE = "1" }

[adds]
ui = [".tex opens as source | split | PDF · compile on save · errors as editor marks · Cmd-click the PDF to jump to the line"]
agents = ["1 tool for every agent here: compile_document · document_guide learns this host's engines"]
```

Typst is the same shape with `sources = ["*.typ"]`, `root = "self"`,
`inputs = "typst-deps"`, `diagnostics = "file-line-col"`, `sync = "text"`, one engine
(`typst compile {root} {output} --root {workspace} --diagnostic-format short --jobs 2
--deps {build_dir}/deps.json --deps-format json`) and `wall_s = 60`. Markdown to PDF
(section 6) adds one field, `transform = "md-typst"`, a named first-party step that
writes the Typst source the engine then builds. Word export
([below](#word-and-editing-what-is-not-markdown)) is `pandoc {root} -o {output}` with
`output = "{build_dir}/{stem}.docx"`, `diagnostics = "file-line-col"` and no sync.
That is the guide's rule of two, met four times over by one point.

| In the manifest (data) | In Chimaera (first-party code, named by the manifest) |
|---|---|
| which files, which tools, the command and its environment, the output path, timeouts, debounce, the Adds lines | the queue and every limit (section 7), the prelude environment capture and PATH walk, build folders and eviction, the events frame |
| the engine ladder as an ordered list | the parsers (`latex-log`, `file-line-col`, `json`), main-file finders (`tex`, `self`), watch-set sources (`fls`, `typst-deps`), sync (`synctex`, `text`), transforms (`md-typst`) |
| the paragraph `document_guide` adds for this plugin | `DocumentView`, the editor marks, the problems list, **Ask agent**, the PDF swap, `compile_document` |

Placeholders (`{root}`, `{build_dir}`, `{output}`, `{stem}`, `{workspace}`) are
substituted by the daemon and shell-quoted, so a file name can never change the
command. A project's own config still wins over the manifest (a `Tectonic.toml`, a
trusted `latexmkrc`), as the guide requires.

### What "on" means for a build plugin

- **On is active.** The manifests carry no `detect` footprint, like Agent notes: an
  agent should get `document_guide`'s engine facts *before* it writes the first
  `.tex`, and a glob detect would need a walk. The card's **Here** line instead
  reports what the quick-open index already knows when it is warm ("12 .tex files ·
  latexmk 4.88 on sherlock") and never starts a walk of its own.
- **File kinds follow the switch.** `.tex` opens as a `document` only in a workspace
  where a build plugin claims it; elsewhere it opens as text, exactly as today.
  `viewKindFor` consults the active plugins' `build.sources` (the plugin store's
  reactive `workspacePlugins`), so switching the plugin off returns every `.tex` pane
  to the plain editor.
- **The agent tools ride only where the plugin is on.** The first draft of this plan
  appended the documents paragraph and `compile_document` for every session. The
  plugin rule is stricter and better: the tool and the paragraph join `tools/list`
  and `initialize` through the existing `plugins::spawn_allow` and `tools.rs` seam
  only in workspaces where a build plugin is active, and the `agent_view` fixtures
  stay byte-identical. An agent in a workspace with the plugin off can still run
  `latexmk` in a terminal; it just gets no compile tool and no engine facts.
- **`DocumentView` is core, not a plugin view.** It is the generic view for "a file
  with a build", parameterized by the manifest, so the specified `provides.views`
  registry stays unbuilt for now.

### What it installs

Nothing silently, and by default nothing at all:

| Where | What | How |
|---|---|---|
| Chimaera | nothing: the runner, parsers, sync and view ship in the binary with the manifests | versions with the daemon |
| the host toolchain | nothing by default; whatever the prelude provides (`module load texlive`, a `typst` in `~/.local/bin`) | detection through the prelude (section 1) |
| managed tools (Phase F) | `typst`, `tectonic`, later `pandoc`: official release artifacts, checksums where the release publishes them, a visible shell session, never sudo, under `~/.chimaera/tools/<tool>/<version>/` | the `runtimes.rs` pattern; **Install** on the empty PDF state |
| the agents | nothing required: the MCP tool and guide reach every agent without an install. Optional: a `report-writing` skill pack (the guide's conventions as an Agent Skill, for agents that run outside Chimaera) as a claude and codex plugin, installed through the agents' own plugin managers in a visible terminal | a new `recommends.agent_plugins` block, the shape of `requires`, shown as optional on the card, so the plugin works with zero installs |
| the workspace | nothing: build output lives in the cache folder; the repo changes only on **Save PDF beside source** | section 2 |

## 1. Engines

### Finding them

Detection runs lazily: the first time a `.tex` or `.typ` opens, or an agent calls a
document tool. Never at boot, so users who never write a report pay nothing.

- **Through the prelude.** The daemon composes the host ⊕ workspace prelude (the
  existing `environment::materialize_prelude` text, no launch scope), runs it once in
  the user's login shell with stdin closed and a 15 s timeout, and captures the
  resulting environment (`env -0`, capped at 64 KB). The fish path already does the
  same one-shot bash capture for fish terminals. The captured environment, minus
  Chimaera's own session variables, is what every compile runs with.
- **Why capture instead of a login shell per compile.** A login shell plus
  `module load texlive` can be slow on a busy login node (Phase A measures it). Paying
  it once per prelude change, instead of on every save, keeps compile-on-save fast.
- **Walking PATH, not `command -v`.** Same reason as Slurm detection in `compute.rs`:
  clusters wrap tools in shell functions, and a PATH walk works the same under bash,
  zsh and fish. Look for `tectonic`, `latexmk`, `pdflatex`, `xelatex`, `lualatex`,
  `biber`, `bibtex`, `synctex`, `typst`, `pandoc`, and later `pdftoppm`. Each found
  tool reports its version (`--version`, 5 s timeout, capped output).
- **Cached** in memory per prelude text hash. A `PUT /api/v1/environment` or a
  **Check again** click invalidates it. Nothing is persisted.
- **Test knob.** `CHIMAERA_DOC_BINDIR` points at stand-in engines, like
  `CHIMAERA_SLURM_BINDIR`, so the whole flow runs in CI without TeX.

### The LaTeX ladder

For a given main file, first match wins:

1. **Workspace override** (`auto | latexmk | tectonic | off`), set from the document's
   toolbar and remembered per workspace.
2. **Project signals.** A `Tectonic.toml` at or above the main file means Tectonic's
   project mode. A project `latexmkrc` means latexmk (its Perl runs only behind the
   [trust gate](#security)).
3. **Engine hints.** A `% !TEX program = xelatex | lualatex | pdflatex` magic comment
   picks latexmk's engine flag.
4. **latexmk** if on PATH together with the needed engine: the host's TeX Live.
5. **Tectonic** if on PATH: the user's own, or later a Chimaera-managed one.
6. **None** (below).

**Why latexmk first, although the request proposed Tectonic first.** Tectonic is
maintained (0.17.0 shipped 2026-07-27) but slowly, and its facts decide it:

- Its package bundle was last updated to **TeX Live 2022**. The host's TeX Live is
  what the user's co-authors, journal templates and cluster jobs use.
- It is **XeTeX only**: no pdfTeX, no LuaTeX.
- Its bundle ships **biblatex 3.17**, which needs exactly **biber 2.17**; any newer
  biber on PATH fails (Tectonic issue 1267, open).
- It defaults to **US letter** paper.

Its advantages mostly vanish here: Chimaera puts every build in a cache folder anyway,
so "no aux files" no longer matters, and shell escape can be turned off for latexmk
too. What remains is decisive only where there is **no TeX Live at all**: most
laptops and fresh cloud machines. There, a 10 MB static Tectonic binary is the best
LaTeX available, so it is the fallback, not the default. The status chip names the
engine, explains a failure that the other engine would avoid (a biber mismatch, a
LuaTeX-only package), and switches in one click. Confirming the flip is
[open decision 1](#open-decisions).

The latexmk command, run with the main file's folder as the working directory:

```
latexmk -pdf | -xelatex | -lualatex
        -interaction=nonstopmode -file-line-error -synctex=1 -recorder
        -outdir=<build dir> -norc [-r ~/.latexmkrc] [-r <project rc, if trusted>]
        main.tex
```

Plus `max_print_line=10000`, `error_line=254`, `half_error_line=238` and
`TEXMFOUTPUT=<build dir>` in the environment. Kpathsea lets an environment variable
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
- **No hand-driven passes.** Chimaera never runs `pdflatex`, `bibtex` or `biber` itself.
  latexmk already emulates an aux folder on TeX Live (its `$emulate_aux` default),
  knows when to run biber (a `.bcf` exists) or bibtex (`\bibdata` in the `.aux`), and
  handles the output-folder quirks.

Tectonic: `tectonic -X compile main.tex --outdir <build dir> --synctex --keep-logs`
with `TECTONIC_UNTRUSTED_MODE=1` in the environment, which disables shell escape
whatever else asks for it. Never `-Z deterministic-mode`, which breaks SyncTeX. A
`Tectonic.toml` project uses `tectonic -X build`, which writes to the project's own
`build/` folder (the project chose that) and has no SyncTeX flag, so those projects
get no sync until that is checked.

### The Typst ladder

1. Workspace override.
2. `typst` on PATH (0.15.1 is current):

   ```
   typst compile main.typ <build dir>/main.pdf --root <root> --diagnostic-format short
         --jobs 2 --deps <build dir>/deps.json --deps-format json
   ```

   `--root` is the workspace root when the file is inside it, else the file's folder.
   `--jobs 2` stops Typst from using every core of a 64-core login node. `--deps`
   writes the list of files the build read, which becomes the watch set.
3. None.

A long-lived `typst watch` process would make recompiles near instant, but it is one
resident process per open document on a shared node, and Typst has no memory limit of
its own. Start with one-shot `typst compile` per save and measure a real 20-page report
in Phase A (no published benchmark exists); reach for `watch` only if it takes over a
second. If font discovery on a network filesystem turns out slow,
`--ignore-system-fonts` plus the project's own fonts is the knob.

### When there is no engine

The document still opens and edits exactly like any text file today. The PDF side
shows a calm empty state instead of an error:

- **No LaTeX engine on this host.** "Chimaera compiles with the tools on
  *sherlock*. It looked for latexmk and Tectonic on the PATH your terminals get,
  including your environment prelude." Actions: **Open Environment settings** (with a
  hint such as `module load texlive`, shown, never written for them), **Check again**,
  and later **Install Typst** / **Install Tectonic**.
- **A PDF already sits beside the source** (`report.pdf` next to `report.tex`): show
  it, with a banner "built elsewhere; may be older than the source" when its mtime is
  older than the source's.
- **Agents** get the same facts as text from `compile_document`
  (`status: no_engine`, what was searched, and how the user can fix it), never a
  failure without words.

### Should Chimaera bundle an engine?

**No, not in the binary. Yes, as an opt-in managed install later.**

- **In the binary**: the static musl downloads are 16.7 MB for Typst 0.15.1 and
  9.7 MB for Tectonic 0.17.0, compressed. Every host would carry them, including hosts
  that never compile a report, and every deploy and update over ssh would move them.
  Typst releases every few months, so the bundled one would also lag the user's. TeX
  Live (several GB) is out of the question.
- **Managed install** (Phase F): the curated-installer pattern in `runtimes.rs`
  already installs agent CLIs from official release artifacts with checksums, as a
  visible shell session, never with sudo. The same pattern installs `typst` and
  `tectonic` under `~/.chimaera/tools/<tool>/<version>/`, and detection appends
  `~/.chimaera/tools/bin` to the end of PATH, so the host's own engine always wins.
  Typst first: one small static binary, no bundle, fast compiles. Tectonic second:
  its bundle cache (`~/.cache/Tectonic`, moved with `TECTONIC_CACHE_DIR`) grows by
  tens of MB per family of documents, which matters on quota'd HPC homes.
- **A WASM engine in the browser**: rejected as the main path.
  - typst.ts's compiler is 28 MB (11 MB gzipped), and it fetches fonts and packages
    from the internet on top.
  - The live LaTeX ports are heavier: TeXlyre's BusyTeX build is about 32 MB of WASM
    plus 90 to 400 MB of TeX data (and AGPL); LibrePaper's needs an 18 MB core bundle
    before any package. SwiftLaTeX has had no release since February 2022.
  - Every byte crosses the tunnel into every browser, then every source file and
    figure must follow before a cold compile.
  - Agents on the host could not use it at all, so `compile_document` would be
    impossible.

  It could make sense for a future local-only, no-host mode; that is out of scope
  here.

## 2. The editing loop

### Opening

- `.tex`, `.ltx` and `.typ` get a new view kind, `document`, in `files.ts`
  (`.sty`, `.cls`, `.bib` and `.bst` stay plain text). `DocumentView.svelte` hosts
  a **source | split | PDF** toggle, like `HtmlView`'s preview | split | edit.
- **Split** is the default when the pane is at least about 900 px wide and an engine
  exists; otherwise **PDF**. The mode is remembered per file.
- `SplitEditPreview.svelte` gains a three-state `show` (`editor | both | preview`)
  so both halves stay mounted across every toggle. Today the preview half unmounts
  when split turns off; for a PDF that means a full pdf.js re-parse.
- LaTeX highlighting is already in the tree (`@codemirror/language-data` lazily
  loads the legacy `stex` mode, under 2 KB gzipped). The richer Lezer LaTeX grammar
  on npm is AGPL, so it stays out. Typst: `codemirror-lang-typst` (Apache-2.0) has a
  WASM-free Lezer grammar for Typst 0.15 syntax at about 38 KB gzipped, loaded lazily.
  It calls itself experimental, so pin the version; a small stream tokenizer is the
  fallback if it breaks.
- Opening a document with no current build compiles it once, so the PDF side is never
  empty for long (one exception: LuaLaTeX on an old, vulnerable LuaTeX; see
  [security](#security)). A current build in the cache is shown straight away.

### Compile on save

- **Triggers.** A save of the main file or any member file from the editor; a disk
  change to any file in the document's watch set while the document is open (an
  agent editing a chapter, regenerating a figure, or adding to `refs.bib`); the
  **Build** button. Closed documents never compile in the background; only an
  agent's explicit `compile_document` does.
- **Debounce.** About 300 ms for Typst and 800 ms for LaTeX after the last trigger,
  because agents often write several files in a burst.
- **One job at a time.** A daemon-wide queue runs one compile at once. Each document
  has at most one running job and one pending rerun; triggers during a run just mark
  the pending flag. At most eight documents wait; beyond that a request is refused
  with "busy". An agent's request for a document whose inputs have not changed since
  the running job started joins that job instead of queueing another
  (single-flight, as `git status` already does).
- **No cancel on a newer save.** Killing LaTeX mid-pass leaves half-written aux files
  and costs more than it saves. The rerun simply follows. Timeouts still kill.
- **Auto-compile toggle** in the toolbar, on by default, remembered per document.
- **Status chip**: `compiling…`, `built 1.2 s · 14 pages`, `2 errors · 5 warnings`,
  `built by <agent name>` when an agent's call produced the PDF.

### Where build output goes

- **Default: `$XDG_CACHE_HOME/chimaera/build/<key>/`** (else `~/.cache/…`), where
  `<key>` is a short hash of the main file's canonical path plus its stem, for
  example `3f9a2c1e-report/`. It holds the PDF, log, aux, `.fls`, `.synctex.gz`,
  the parsed diagnostics and the dependency list.
- **Why not the repo:** aux litter, git noise, and agents reading stale `.aux` and
  `.log` files as if they were sources.
- **Why not the runtime directory:** on systemd hosts it is often RAM-backed tmpfs,
  it is night-scrubbed, and `/tmp` on login nodes is small and node-local. A cache
  that survives also keeps LaTeX warm: a warm latexmk rerun is often a single pass,
  a cold one with a bibliography three or four.
- **Bounded.** 512 MB per document and 1 GB in total by default, evicted by
  least-recent build after each compile (a bounded walk of one folder). A setting
  points the build root elsewhere, for example `$SCRATCH`.
- **Getting the PDF out.** **Save PDF beside source** writes `report.pdf` next to
  `report.tex` (atomic copy; refuses to overwrite anything that is not a PDF).
  A per-document **keep a copy beside the source** switch (off by default) does it
  after every good build. **Download** uses the existing download ticket.
  `compile_document` has a `copy_to` argument for agents.

### Refreshing the PDF, over a slow tunnel

- A finished compile sends a small `/ws/events` frame
  (`{"type":"doc","root":…,"state":…,"version":…}`, additive). The view then asks
  for the status and the new PDF. No polling, and no fs-watch slot is spent on the
  build folder.
- **Swap, don't remount.** Today a new file version remounts `PdfView` (a flash,
  and scroll restored by pixel offset although page heights may have changed).
  `PdfView` gains an in-place reload: open the new document in the background,
  render the visible pages offscreen, swap in one frame, keep the scroll anchored to
  the same page and fraction, then destroy the old document.
- **Only the pages you look at cross the tunnel.** Since martinappberg/chimaera#159
  `PdfView` opens every document with `disableAutoFetch` and `disableStream`, so
  pdf.js fetches byte ranges for visible pages only over `/raw`. A report with heavy
  figures then costs roughly the visible pages' objects per rebuild instead of the
  whole file; Phase B measures how much that saves after an in-place swap.
- **A failed build keeps the last good PDF** with an error bar. LaTeX often produces a
  partial PDF despite errors; **show partial output** switches to it.

### Errors as editor marks

- **The parser is in the daemon** (it also feeds `compile_document`), in Rust,
  streaming the log line by line with a 16 MB scan cap. It follows the TeX file
  stack through parentheses, reads `-file-line-error` prefixes, and recognizes
  `! LaTeX Error`, `! Undefined control sequence` with its `l.<n>` context line,
  `Missing $ inserted`, `Runaway argument`, `Emergency stop`, missing files and
  packages, `LaTeX Warning: Reference/Citation … undefined`, overfull and underfull
  boxes, package warnings, and `biber`/`bibtex` messages from the `.blg`. Typst's
  short diagnostic format (`file:line:col: error: message`) needs almost no parsing
  and carries exact columns.
- **Output**: at most 200 diagnostics plus counts, each
  `{severity, file, line, column?, end_line?, message, context, origin, log_line}`,
  with `file` relative to the main file's folder. An error inside a class or package
  file maps to the nearest user file on the stack.
- **Heuristics are ported, not invented.** LaTeX Workshop's log parser (MIT) has
  years of edge cases: one pattern covers both the `file:line:` and the `!` error
  forms, and it tracks the file stack by counting parentheses. texlab's parser
  re-joins lines of exactly 79 characters, which is still needed for Tectonic: it does
  not use kpathsea, so `max_print_line` may not reach it. A shared fixture corpus of
  real logs (pdflatex, xelatex, lualatex, Tectonic, biber, bibtex, Typst) runs in the
  Rust suite, like `mathBlocks.fixture.json` does for math.
- **In the editor**: `@codemirror/lint` (already a dependency, used by the settings
  JSON editor) shows gutter marks and underlines through `CodeView`'s `extra`
  compartment. Marks map through later edits automatically and clear on the next
  build. Overfull and underfull boxes are hidden by default behind a filter; they are
  noise until the end.
- **Problems list** under the PDF, grouped by file. Clicking an entry opens the file at
  the line (the documents plan's Phase 1 "open at the spot").
- **Special cases get plain words.** "`siunitx.sty` not found: this TeX Live does not
  have it; load a fuller TeX Live in your prelude, or ask your admins." "biber 2.19
  does not match biblatex 3.20; use the biber from the same TeX Live." "Package
  `@preview/cetz:0.3.4` is not cached and this host is offline."
- **Log text is untrusted.** It contains text from the document. It is shown as text,
  never as HTML ([web-UI rules](../.claude/rules/web-ui.md)).

### Ask the agent to fix it

- **Ask agent** on each diagnostic (hover card and problems list) types one line into
  the current reference target, through the existing reference handler. A new pure
  composer in `shared/reference.ts` builds it; like every composer there, it never
  adds a newline, so it can never auto-submit:

  ```
  @chapters/intro.tex#L12 LaTeX error: "Undefined control sequence \unit" at "l.12 ...\unit{mg}" (log: ~/.cache/chimaera/build/3f9a2c1e-report/report.log#L345)
  ```

- **Ask agent to fix all** on the status chip:
  `Fix the 3 compile errors in @report.tex (compile_document lists them) `.
- It works for agents that do not have the chimaera MCP server too: the path, line,
  message and log location are all plain text any agent on the host can use.

## 3. Source and PDF: SyncTeX

### Where the data comes from

latexmk (`-synctex=1`) and Tectonic (`--synctex`) write `main.synctex.gz` into the
build folder. It maps typeset boxes to source file and line.

### Parse it in the browser

- A TypeScript SyncTeX parser runs in a Web Worker: LaTeX Workshop's `synctexjs.ts`,
  an MIT port of synctex-js, is the starting point. It loads `main.synctex.gz` through
  a `/raw` ticket on the first sync action, decompresses with the browser's own
  `DecompressionStream`, and is cached per build version. LaTeX Workshop already uses
  its JS parser alone for inverse search, because the `synctex` binary mishandles
  some non-ASCII paths.
- **Why the browser.** Sync must feel instant, and every daemon round trip costs about
  two tunnel round trips ([remote perf plan](perf-remote-plan.md), F2). Parsing a
  thesis-sized SyncTeX file would also take tens of MB of daemon memory. The `synctex`
  command-line tool is not an option either, since Tectonic-only hosts do not have it.
- **Cap.** Over 16 MB compressed, sync turns off with a note.
- **Units and paths.** The file stores scaled points plus a unit and offsets from its
  preamble; there are 65,781.76 scaled points to a PDF point, and SyncTeX measures
  from the page's top left while PDF measures from the bottom left. Input paths are as
  the engine saw them (Tectonic writes absolute paths since 0.8.1); they are
  normalized against the main file's folder and mapped to workspace paths.

### Forward: source to PDF

- **Mod+J** (a new action in the keybinding registry), a toolbar button, or
  **follow cursor** (off by default, debounced about 250 ms) takes the cursor's line
  to its boxes in the PDF.
- `PdfView` gains `showBoxes(page, boxes)`: scroll so the first box sits a third of the
  way down if it is not already visible, then draw a translucent accent highlight that
  fades after about 1.5 s. The page viewport's `convertToViewportPoint` turns box
  corners into screen positions, as LaTeX Workshop's viewer does, which also handles
  zoom and a non-zero MediaBox origin.

### Inverse: PDF to source

- **Cmd/Ctrl-click** on the PDF (a plain click stays text selection) maps page, x and y
  to file, line and, when SyncTeX knows it, column. The editor half of the split
  jumps there and flashes the line. A line in another member file opens that file at
  the line (documents plan Phase 1); once the documents plan's buffer store exists,
  the split's editor can switch to that file in place without losing unsaved text.
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
tinymist's preview uses. The `typst` command line does not expose them. Three options:

1. **Text matching (Phase C, recommended first).** Typst prose is very close to its
   source. Inverse: take the clicked or selected PDF text (pdf.js text layer) and find
   it in the member files, ignoring markup, whitespace and hyphenation; ties go to the
   file the editor shows. Forward: take the words around the cursor and find them in
   the page texts. Headings and plain paragraphs map well; math, tables and generated
   text do not. The UI says "approximate" and never pretends otherwise. Phase C also
   tests whether `typst eval` (new in 0.15, replacing the deprecated `typst query`)
   can report heading positions, which would anchor the matching per section.
2. **A small companion binary (later, decision).** Built from Typst's own crates, it
   answers exact jump queries. Cost: it pins a Typst version separate from the host's
   `typst`, compiles the document a second time itself, and needs updating with every
   Typst release. Linking the compiler into the daemon instead is ruled out by the
   memory budget.
3. **tinymist, when the host has it.** Its preview does exact two-way jumps, but it is
   a 32 MB, long-running server with its own web preview and its own data plane. It
   could open in the browser pane for users who already use it; it does not fit
   `PdfView`.

See [open decisions](#open-decisions).

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
   file the build read. The daemon keeps a small in-memory map from file to main,
   filled from recent builds (capped at 512 entries per workspace).
4. **A bounded search.** `.tex` files with `\documentclass` in the file's folder and up
   to two parents (at most 200 files, first 8 KB each, under the filesystem semaphore,
   off the reactor) whose `\input`, `\include`, `\subfile` or `\import` lines name this
   file. Exactly one match wins.
5. **Ask once.** Several candidates, or none: a small picker in the toolbar. The
   answer is remembered per workspace in a small capped JSON file under
   `~/.chimaera`.
6. **Project files.** A trusted `latexmkrc`'s `@default_files`, and `Tectonic.toml`
   projects, name their own main files.

For a `.typ` file: it is its own main unless a recent build's dependency list
(`--deps`) or a static scan of `#include`/`#import` in its folder shows another file
including it. Several candidates: ask once, as above.

A member file shows its main document's PDF, labelled "part of main.tex".

### Includes and figures

- **The watch set** is the main document's inputs inside the workspace: the `.fls`
  `INPUT` lines minus TeX distribution files, or Typst's dependency list. It is capped
  at 256 paths. While the document is open, the daemon stats the set every 5 s (a
  gentler pace than the 2 s view watcher, since the set is larger) and also reacts at
  once to recognized agent writes (the hooks already call
  `git::mark_path_dirty`). A change queues a recompile. This is separate from the
  per-window fs watcher, whose 64-file cap is meant for visible views.
- **Figures** resolve relative to the main file's folder, which is the working
  directory, plus any `\graphicspath`. EPS figures need `repstopdf` through restricted
  shell escape; agents are told to write PDF or PNG instead.
- **`\include` with an output folder** needs matching subfolders in it; latexmk
  handles this, and the runner creates them from the last `.fls` if needed.

### Bibliography

- **LaTeX**: latexmk decides between bibtex and biber on its own (from the `.aux` or
  `.bcf`) and reruns as needed. Chimaera adds only diagnostics from the `.blg`, and the
  plain-words message for the most common HPC failure: a biber (often from conda)
  that does not match the TeX Live's biblatex.
- **Tectonic** runs bibtex itself (ported to Rust in 0.15). biblatex documents need an
  external biber, and its bundled biblatex 3.17 accepts only biber 2.17; with any
  other biber the chip offers latexmk.
- **Typst** reads `.bib` (BibLaTeX) or Hayagriva `.yml` natively with
  `#bibliography("refs.bib")`. No extra tool, no extra pass. One more reason to
  recommend Typst for new reports.
- `.bib` files open as text. Saving one recompiles its main document through the
  watch set.

## 5. Agent awareness

Every Claude and Codex session Chimaera spawns already loads the chimaera MCP server
(`mcp.rs`). Since martinappberg/chimaera#159 its `instructions` carry a documents
paragraph, and every tier has `document_guide` (the portable markdown dialect) and
`check_document`. It is the one channel that reaches every agent without touching
the repository. This plan adds to it only where a build plugin is on
([what "on" means](#what-on-means-for-a-build-plugin)), through the plugin seam
(`plugins/tools.rs`: `instructions`, `defs`, `call`).

### A short paragraph in the MCP instructions

Appended where a build plugin is active (about 600 characters):

> Chimaera documents: the user reads your reports as PDFs beside their source. For a
> new polished report, write Typst (.typ) unless LaTeX is required (a journal
> template, an existing .tex project). After editing a report, call
> compile_document and fix every error it returns before you hand over. Call
> document_guide once for this host's engines and the conventions here (main file,
> figures, bibliography, what is not allowed).

The paragraph is static. The live facts (which engines this host has) come from
`document_guide`, because `initialize` must not trigger a login-shell detection.

### `document_guide(kind?)`

Extends the tool that exists: with no `kind`, or `markdown`, it returns today's
guide unchanged; `latex` and `typst` return at most 8 KB of text:

- **This host**: the engines found and their versions, and which one this workspace
  will use (or why none, and how the user can fix it).
- **Structure**: one main file per report; included LaTeX files start with
  `% !TEX root = main.tex`; Typst has one `main.typ` that `#include`s the rest.
- **Build output**: never write aux files or PDFs into the repo; the build folder is
  managed; use `copy_to` when the user wants the PDF beside the source.
- **Figures**: PDF for vector plots, PNG at 300 dpi or more for rasters, relative paths,
  under `figures/`. No EPS.
- **Paper size**: set it explicitly (`a4paper` or `letterpaper` in the class options;
  `#set page(paper: "a4")` in Typst). Tectonic defaults to US letter, TeX Live to its
  site setting, so leaving it out gives different PDFs on different hosts.
- **Bibliography**: Typst `#bibliography("refs.bib")`; LaTeX biblatex with biber, or
  natbib with bibtex, one per project.
- **Not allowed here**: packages that need unrestricted shell escape (`svg`, TikZ
  externalization, minted before version 3) unless the user enabled it; `\write18`;
  absolute paths; fonts that are not on the host (use the engine's defaults); Typst
  `@preview` packages without a pinned version, and any at all on a host without
  network.
- **Skeletons**: a minimal Typst report and a minimal LaTeX report that compile here.
- **The loop**: compile, fix errors, then undefined references and citations, then
  check the page count and figures.

### `compile_document(path, engine?, copy_to?, timeout_s?)`

- `path` may be any member file; the main file is found as in
  [section 4](#finding-the-main-file). Relative paths resolve against the agent's
  working directory, then the workspace root.
- **Waits for the result**, up to `timeout_s` (default: the engine's wall limit, 180 s
  for LaTeX and 60 s for Typst; cap 600), joining a running job for the same inputs
  instead of starting a second one.
- **Returns text, at most 8 KB**: `status` (`ok`, `errors`, `failed`, `timeout`,
  `no_engine`, `busy`), engine and version, main file, PDF path, pages, size and
  duration; up to 20 errors as `file:line: message` plus one context line; undefined
  references and citations (up to 10); a count of box warnings; the log path.
- **Updates the user's view.** Same build folder, same events frame: an open PDF
  refreshes and the chip says which agent built it.
- **Bounded like everything else.** Same queue, same limits, at most one pending rerun
  per document, no shell-escape argument. `copy_to` must resolve inside the workspace
  and may only replace a PDF.
- **Every tier** where a build plugin is on, not Mastermind-only: every agent writes
  reports.
- **Log excerpts are data.** They quote the document, which may come from an untrusted
  repository; the tool text says so.

### Later: `render_page(path, page)`

Returns one page as a PNG (MCP image content) so multimodal agents can see a figure
running off the page or a table that overflows. Typst renders PNG itself
(`--format png --pages N --ppi …`); LaTeX PDFs use `pdftoppm` when the host has
it, else the tool is not offered. Caps: one page per call, about 1.5 megapixels,
1 MB.

## 6. Markdown to PDF

Agents write the portable markdown dialect (the documents plan, Phase 6). Some of it
should leave as a real report: title block, page numbers, table of contents, numbered
figures, a bibliography.

| Option | Needs on the host | Verdict |
|---|---|---|
| Browser print of the reading view | nothing | Keep for "what I see"; not typeset (no running heads, no page-aware floats). |
| pandoc to LaTeX to PDF | pandoc + TeX Live | Slow, and plain without a custom template. |
| pandoc to Typst (`--pdf-engine=typst`, since pandoc 3.1.2) | pandoc (3.11, a 33 MB download) + typst | Good output; GitHub alerts are on by default for `gfm` input. But pandoc is rarely on HPC hosts, and its templates are a second language. Offer when present, never require. |
| Quarto (`format: typst`) | Quarto (140 MB; bundles pandoc, Typst, Deno) | Too heavy to depend on. |
| A Typst package that parses markdown inside Typst (`cmarker` 0.1.10, with `mitex` for math) | typst 0.15 or newer | Zero daemon code. But it is another parser (pulldown-cmark) with its own gaps, it has no GitHub alerts, and its raw-Typst comments are on by default and must be turned off. Good prototype. |
| **Chimaera writes Typst from its own markdown tree** | typst | **Recommended.** |

**The recommendation.** The documents plan keeps comrak (already in the daemon) as
the reference renderer and the export path. Add a comrak-to-Typst writer for exactly
the portable dialect:

- Frontmatter `title`, `summary`, `status`, `audience`, `updated` fill the template's
  title block and abstract.
- GitHub alerts become styled callouts; tables, footnotes, task lists and code blocks
  map one to one.
- Math `$…$` and `$$…$$` goes through `mitex` (LaTeX math inside Typst, a WASM plugin
  of about 185 KB). Its package files ship inside Chimaera's template bundle and are
  passed with `--package-path`, so export never needs the network (license to confirm
  before vendoring).
- Every piece of text is escaped on the way out, so nothing in a markdown file can
  inject Typst code into the template.
- Image embeds become numbered figures with their alt text as the caption; `#page=`
  and `#xywh=` fragments map to Typst's image options where it has them.
- Mermaid is rendered to SVG by the browser at export time when a window is open;
  otherwise it stays a code block with a note. (pandoc's route needs `mmdc`, a
  headless Chromium; Typst-native Mermaid plugins exist but are not yet evaluated.)
- Two or three curated templates (report, memo, article) ship in the binary as a few
  KB of Typst. Frontmatter `template: path/to/mine.typ` picks a project template.
- **Export PDF** runs through the same queue into the build folder. **Open as Typst**
  writes the generated `report.typ` beside the markdown so the user or an agent can
  keep going in Typst.
- The writer is tested against the documents plan's parity corpus: every construct
  gets a Typst snapshot.

## 7. Limits and security on a login node

### Running the engine

| Limit | Default | How |
|---|---|---|
| Concurrency | 1 compile daemon-wide; 1 pending rerun per document; 8 documents queued | a semaphore and a small queue |
| Priority | nice 10; idle I/O class where allowed | `setpriority` and `ioprio_set` before exec |
| Wall time | 180 s LaTeX (Overleaf's self-hosted default), 60 s Typst; a setting, cap 600 s | a timer, then SIGTERM and SIGKILL to the whole process group |
| CPU time | wall limit plus slack | `RLIMIT_CPU`, a backstop |
| Memory | 4 GB address space | `RLIMIT_AS`; see below |
| File size | 256 MB per written file | `RLIMIT_FSIZE` stops a runaway `\write` loop filling the disk |
| Build folder | 512 MB per document, 1 GB total | checked after each build, least recent evicted |
| Daemon memory | engine output goes to files, not pipes; the daemon reads a 64 KB tail and streams the log parse | no whole-log reads |
| Threads | `typst --jobs 2` | leave cores for everyone else |

Each compile runs in its own process group (so a timeout kills latexmk and every pass
it started), with stdin closed (so an error prompt can never wait for input), through
Tokio's async child handling like `compute.rs`'s capped runner, so no reactor thread
ever blocks on it.

**Why every limit is needed.** TeX has no time limit of its own: `\def\x{\x}\x` spins
forever, so only the wall clock stops it. Typst stops a `while` loop after 10,000
rounds and caps call depth, but it has **no memory limit**: an open issue shows one
expression eating tens of GB, and another a 300-page document reaching 32 to 41 GB.
On a shared login node that is an outage, so the memory cap is not optional. Phase A
checks that 4 GB of address space does not break normal Typst builds (its threads
reserve address space); if it does, the fallback is a user cgroup
(`systemd-run --user --scope -p MemoryMax=…`) where the host has user systemd, and a
plain wall-clock limit where it does not.

### Security

- **Treat a compile as running code.** Everything below narrows what a document can
  do; none of it makes compiling an untrusted document fully safe.
- **Shell escape.** Unrestricted shell escape (`-shell-escape`, Tectonic's
  `-Z shell-escape`) is off and can only be turned on by the user, per project, in the
  UI; never by an agent and never by a file in the repo. TeX Live's own default,
  **restricted** shell escape, stays as the site configured it: a short list of helpers
  (`bibtex`, `kpsewhich`, `makeindex`, `repstopdf`, `latexminted` and a few more).
  Documents rely on it for EPS figures and minted code listings. That list has had
  holes (`mpost` allowed arbitrary commands until it was replaced by `r-mpost`,
  CVE-2016-10243), which is why [open decision 4](#open-decisions) asks whether to
  pass `-no-shell-escape` instead. Tectonic always runs with
  `TECTONIC_UNTRUSTED_MODE=1`. Typst has no shell escape at all.
- **Old LuaTeX can run commands anyway.** LuaTeX 1.04 to 1.16 (TeX Live 2017 to 2022
  and the first TeX Live 2023) could run shell commands even with shell escape off
  (CVE-2023-32700) and open network sockets (CVE-2023-32668). HPC modules are often
  old. Detection records the LuaTeX version; below 1.17.0, a LuaLaTeX document never
  compiles on open or on an agent's write, only on the user's own save or **Build**,
  and the chip says why.
- **Project code.** A project `latexmkrc` is Perl. It runs only after the user trusts
  it for this workspace, and the trust is tied to the file's content hash, so an edit
  (by anyone, including an agent) asks again. Until then the build uses `-norc` plus
  the user's own rc. The environment prelude page already calls a checked-in prelude
  file a supply-chain vector; this is the same rule.
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
  kernels) or bubblewrap where user namespaces work. That is a later, opt-in hardening
  step, not a default, because many HPC kernels have neither.
- **Network.** TeX Live's pdfTeX and XeTeX never touch the network (old LuaTeX: see
  above). Tectonic downloads bundle files on a cache miss and Typst downloads `@preview`
  packages on first import; both are the engine fetching its own packages, allowed. An
  **offline** setting (for compute nodes) passes Tectonic's `--only-cached`. Typst has
  no offline switch, but a cached package never touches the network, and a missing one
  on an offline host becomes a plain-words diagnostic. Chimaera does not sandbox the
  network itself, for the same reason as reads.
- **Environment hygiene.** Compiles get the captured prelude environment, minus the
  daemon's own variables and anything on `api::spawn_env_remove`. No token ever reaches
  an engine.
- **Routes.** Every new route is bearer-authed. Build PDFs and SyncTeX files are served
  through the existing short-lived `/raw` tickets. `copy_to` and **Save PDF beside
  source** are confined to the workspace.

## What changes where

### Daemon (`crates/chimaera-server/src/build/`, plus the plugin seam)

| File | What |
|---|---|
| `plugins/manifests/latex.toml`, `typst.toml` | the two manifests, registered in `MANIFESTS`; `md-pdf.toml` (Phase E) and `docx.toml` (Phase G) later |
| `plugins/mod.rs` | the `[build]` section of `Manifest` (`deny_unknown_fields`), the `recommends` block, the lookups the build module and the UI use |
| `plugins/tools.rs` | `compile_document` and the guide paragraph, served where a build plugin is active |
| `build/mod.rs` | routes, the events frame, the module's map line in the server `AGENTS.md` |
| `build/engines.rs` | prelude environment capture, PATH walk, versions, the ladder from the manifest's engine list |
| `build/job.rs` | queue, coalescing, single-flight, limits, process groups, build folders and eviction |
| `build/root.rs` | the named main-file finders, `.fls` and Typst dependency lists, the watch set |
| `build/latexlog.rs`, `build/typstdiag.rs` | the named parsers, with the fixture corpus |
| `build/md2typ.rs` | the `md-typst` transform: the comrak-to-Typst writer and the embedded templates (Phase E) |

New routes, all bearer-authed and additive:

- `GET /api/v1/doc/engines` (`?refresh=true` re-detects)
- `POST /api/v1/doc/compile {path, reason}` → `202 {root, version}`
- `GET /api/v1/doc/status?path=` → engine, state, PDF, SyncTeX path, counts,
  diagnostics
- `PUT /api/v1/doc/root {path, root}` (remember a main-file choice)
- `POST /api/v1/doc/export {path, template?}` (markdown to PDF, Phase E)
- `/ws/events` frame `{"type":"doc", …}`
- MCP: `document_guide`, `compile_document`, later `render_page`

### Web UI (`web-ui/src/lib/previews/`)

| File | What |
|---|---|
| `files.ts` | the `document` view kind, chosen from the active plugins' `build.sources` |
| `../plugins/store.ts` | the `build` section on the wire types, and which sources are claimed in the active workspace |
| `DocumentView.svelte` | source, split and PDF; toolbar, chip, problems list, empty states; parameterized by the manifest (core, not a plugin view) |
| `SplitEditPreview.svelte` | the three-state `show`, both halves always mounted |
| `PdfView.svelte` | in-place reload, ranged loading, `showBoxes`, Cmd-click and selection hooks |
| `doc/compile.svelte.ts` | per-document status store, the events frame |
| `doc/diagnostics.ts` | diagnostics to `@codemirror/lint` |
| `doc/synctex.ts`, `doc/synctex.worker.ts` | the parser and its queries |
| `doc/typstLang.ts` | lazy `codemirror-lang-typst` (Lezer grammar), stream-tokenizer fallback |
| `../shared/reference.ts` | the compile-error composer |

## Phases

### Phase A: the compile service and agents (daemon only)

The `build` contribution point in `plugins/mod.rs` and the latex and typst manifests
(their cards render on the Plugins tab through the generic card, with the Adds
lines), engine detection through the prelude, the job runner with every limit, build
folders, main-file detection (magic comment, `\documentclass`, picker route), both
parsers with the fixture corpus, the routes and events frame, and, where a plugin is
on, the MCP paragraph, the `document_guide` extension and `compile_document`.

Agents get value before any UI exists: they can build a report and fix it.

**Verification.** Rust tests with stand-in engines (`CHIMAERA_DOC_BINDIR`): the queue,
coalescing, single-flight, timeouts killing a whole process group, the file-size
limit, eviction, env scrubbing, the old-LuaTeX gate; the `agent_view` fixtures
unchanged with both plugins off, and `src/tests/plugins.rs` showing
`compile_document` offered and callable only where one is on. Live on a real login
node with `module load texlive` and a real `typst`: an article, a thesis-shaped `\include`
project with biber, an infinite-loop document (the timeout), a Typst document that
allocates without bound (the memory cap, and whether 4 GB of address space breaks
normal Typst builds), and an agent running the compile-fix loop through MCP. Record
the numbers the research could not find: prelude capture time, Typst time and memory
for a 20-page report, Tectonic's memory and cache growth.

### Phase B: the document view

`DocumentView`, compile on open, on save, and on agent writes to the open file or
its main file (Phase D widens this to every input), the in-place PDF swap with
ranged loading, error marks, the problems list, **Ask agent**, the status chip, empty
states, **Save PDF beside source**.

**Verification.** Driven live in the isolated preview on Chromium and WebKit, against a
remote daemon over a real tunnel: type, save, watch the PDF swap without a flash; let
an agent edit a chapter and watch the PDF follow; break a macro and send the error to
the agent. A `scripts/perf/` scenario measures bytes per rebuild of a 20 MB report.

### Phase C: SyncTeX and references

Both directions for LaTeX, follow cursor, selections to `@file.tex#Lx-Ly`, the text
matching version for Typst.

### Phase D: multi-file depth

The watch set from `.fls` and Typst dependencies, remembered main files, the
bibliography messages, and switching the split's editor between member files (after
the documents plan's buffer store lands).

### Phase E: markdown to PDF

The comrak-to-Typst writer, the templates, `mitex` vendored after a license check, **Export PDF** and
**Open as Typst**, pandoc as an option when present.

### Phase F: installs and extras

Managed Typst and Tectonic installs, the optional `report-writing` skill pack behind
`recommends`, `render_page`, and, if chosen, the Typst jump companion.

### Phase G: Word and other outputs

The `docx` manifest (pandoc when present, later a managed install), **Export to
Word** on markdown documents, **Open as markdown** for a `.docx`, and later a Marp
deck to `.pptx` on the same point. See
[Word](#word-and-editing-what-is-not-markdown).

| Phase | What | Size |
|---|---|---|
| A | The `build` point, compile service, parsers, MCP tools | medium |
| B | Document view, compile loop, error marks | large |
| C | SyncTeX both ways, source references | medium |
| D | Multi-file depth | medium |
| E | Markdown to PDF through Typst | medium |
| F | Managed installs, skill pack, page renders | medium |
| G | Word export and import through pandoc | small |

A comes first; B needs A; C and D need B and can run in parallel. E needs only A (the
queue) and the documents plan's comrak work. F can land any time after A. G needs A
and B (the output opens in the split) and pandoc on the host.

### How this fits the documents plan

The documents plan shipped in martinappberg/chimaera#159, so what this plan leans on
is there:

- **Open a file at a line** (its Phase 1) is what the problems list and cross-file
  inverse search use.
- **Buffers that outlive views** (its Phase 0) let the split switch member files
  without losing an unsaved chapter.
- **Embeds** (its Phase 4): an embed of `report.typ#page=2` can show the compiled
  page once the embed card resolves a document to its build PDF (Phase D here).
- **Point at anything** (its Phase 5): the region box and file references work on
  compiled PDFs, with source lines attached.
- **The dialect and `check_document`** (its Phase 6): `document_guide` exists, and
  this plan extends it rather than starting a second guide.

## Word, and editing what is not markdown

Word documents open read-only today (`DocxView`, drawn by docx-preview inside a
shadow root after sanitizing). "Editable Word, the way markdown works here" can mean
three things, and only two of them fit Chimaera:

1. **Editing a `.docx` in place**, every style, comment and tracked change
   preserved. That needs an OOXML editor in the browser. None fits: the open-source
   ones are whole servers to run (ONLYOFFICE's document server, Collabora Online:
   hundreds of MB and a service beside the daemon) or copyleft editors (SuperDoc,
   AGPL; licence to confirm), and the permissive libraries only read (`mammoth`,
   docx-preview) or only write (`docx`). This stays out until a library exists that
   could ship inside a static binary's web UI.
2. **Editing the content through markdown.** **Open as markdown** runs
   `pandoc report.docx -t gfm --extract-media=figures -o report.md` through the same
   runner, when pandoc is present, and opens the copy in the live markdown editor
   that exists today, beside the original. The card says what did not survive:
   tracked changes, comments, headers and footers, most layout. Editing from there
   is the markdown workbench, with agents, references and embeds.
3. **Word as an output**, like PDF. A `docx` build manifest turns a markdown
   document into `report.docx` with pandoc, a `--reference-doc` for the house style
   when the project has one, opened in `DocxView` beside the source and rebuilt on
   save. Agents write the portable dialect; the collaborator gets Word.

The recommendation is 2 and 3, on the `build` point, with pandoc as an optional
managed install (a 33 MB static binary, the same pattern as Typst). It is the
agent-first answer: the source of truth stays a text file an agent can write and a
diff can show, and Word is a view of it. Bringing a collaborator's edits back (their
`.docx` to markdown, then a three-way merge against the exported version) is a later
step on the same tools. Marp decks already render here; `marp --pptx` on the same
point gives PowerPoint the same way, later.

## Packaging: plugins in their own repositories

Today every manifest is embedded in the binary (`MANIFESTS` in `plugins/mod.rs`),
and "extensible" means extensible by this repository. The question is whether each
plugin should be its own repository, found through a marketplace, so that a LaTeX
plugin, a Word plugin or a Quarto plugin can come from anyone. The answer is yes
for everything that is data or agent-side, and no for code:

- **No extension host.** Third-party code in the daemon or the UI does not survive
  the daemon's constraints: a static binary at about 150 MB on a shared login node,
  bearer auth on every route, no unbounded child processes. An extension host is
  also what makes an IDE an IDE, and the split above shows it is not needed: the
  pieces that must be careful (the runner, the parsers, sync, the views) are few,
  shared by every document plugin, and better written once. Third parties extend
  Chimaera by writing manifests that name them.
- **The marketplace exists already.** A claude marketplace is a git repository with
  `.claude-plugin/marketplace.json`; a codex plugin is the same repository with
  `.codex-plugin/plugin.json`; mycelium is both at once. A Chimaera plugin
  repository is that repository plus `.chimaera-plugin/plugin.toml`, the manifest
  above. Its skills, hooks and MCP servers ride the agents' own plugin managers, as
  the authoring guide requires; its workbench half is the manifest. The Browse view
  the plugins plan defers (§6.5) lists the marketplaces the agents already have; a
  card gains a *workbench* badge when the plugin carries a Chimaera manifest.
- **Discovery: three sources, one catalog.** Embedded manifests (first-party,
  versioned with the daemon); manifests inside installed agent plugins
  (`claude plugin list --json` reports each plugin's `installPath`, which
  `agent_probe.rs` already reads to scan skills; codex's app-server reports its
  plugin paths the same way); and `~/.chimaera/plugins/<id>/`, a clone at a pinned
  commit for a plugin that is not an agent plugin, staged and reviewed the way the
  skills-manager design stages skill packs. The card shows the source. The same id
  from two sources is a conflict the card names, never a silent override.
- **Trust is the manifest, read once.** A third-party manifest is declarative only:
  it may use `build`, `detect`, `requires`, `recommends`, `setup`, `settings` and
  `commands`, and name first-party capabilities. Its command templates are shown
  verbatim when the plugin is first switched on and again whenever the manifest's
  hash changes, like a project `latexmkrc` and like codex hook trust; the daemon
  quotes every placeholder. No LLM reviews it and nothing runs before the user has
  read it. A manifest that names a capability this daemon does not have, or a
  `requires.chimaera` newer than this build, renders "needs a newer chimaera" and
  stays off.
- **Order.** In-tree first: the latex and typst manifests land embedded, because
  that is the fastest way to prove the `build` point and there is no second home for
  a manifest yet. The manifest format is designed now so that moving a plugin to its
  own repository later is a file copy plus the discovery source, not a rewrite. The
  move earns its keep when a plugin has a life outside this repository: a
  `chimaera-plugins` repository (or one per plugin) that also ships the skill packs,
  or the first third-party manifest.

## Open decisions

1. **Engine order on hosts with both.** The request proposed Tectonic first. The plan
   recommends latexmk first and Tectonic as the fallback where there is no TeX Live,
   because Tectonic's bundle is frozen at TeX Live 2022, it is XeTeX only, and it pins
   biber 2.17 ([the evidence](#the-latex-ladder)). Confirm the flip, or keep Tectonic
   first with the automatic exceptions and a one-click switch?
2. **Build folder default.** `~/.cache/chimaera/build` with a 1 GB cap (recommended),
   the runtime directory, or a scratch path?
3. **Compile on open and on agent writes.** Recommend on for both, with the per-document
   toggle. Or only on the user's own saves?
4. **Restricted shell escape.** Keep TeX Live's restricted default (recommended:
   documents rely on it for EPS figures and minted, and the site chose it), or pass
   `-no-shell-escape` everywhere and accept those breakages for a smaller surface?
5. **Pre-allow the document tools** in the generated agent settings, so the
   compile-fix loop does not ask permission every time? Recommend yes for
   `document_guide` and `compile_document`, which are bounded and cannot run shell
   escape.
6. **Typst jump precision.** Text matching only (recommended to start), or also build
   and maintain a companion binary from Typst's crates for exact jumps?
7. **Typst as the recommended format** for new agent reports in the MCP paragraph. A
   product stance; recommend yes.
8. **Managed installs.** Offer Typst and Tectonic installs at all? Recommend Typst yes,
   Tectonic after measuring its cache growth on a real home quota.
9. **Markdown to PDF.** Chimaera's own comrak-to-Typst writer (recommended), a
   `cmarker` template, or pandoc when present?
10. **Tools only where a build plugin is on** (recommended, the plugin rule), or the
    documents paragraph and `compile_document` for every session as first drafted?
11. **On is active** for build plugins (recommended: no footprint, no walk, the guide
    before the first `.tex`), or a `detect` glob answered from the quick-open index?
12. **Packaging.** Embedded manifests first and a plugin repository format
    (`.chimaera-plugin/plugin.toml` beside `.claude-plugin/` and `.codex-plugin/`)
    when a plugin has a second home (recommended), or start in a separate repository
    from day one?
13. **Word.** Export and import through pandoc on the build point (recommended), or
    look again for an in-browser `.docx` editor first?

## Out of scope

- **A language server, completion or refactoring** for LaTeX or Typst (texlab,
  tinymist): the DESIGN.md non-goal.
- **A WASM engine in the browser**, for the reasons above.
- **Bundling TeX Live** or managing TeX packages (`tlmgr`). The host's admins and the
  user's prelude own the TeX installation.
- **Editing a `.docx` in place**, styles and tracked changes preserved: no library
  with a fitting licence and footprint
  ([Word](#word-and-editing-what-is-not-markdown)). Word as an output and as an
  import is in scope (Phase G).
- **An extension host**, or any third-party code in the daemon or the UI
  ([packaging](#packaging-plugins-in-their-own-repositories)).
- **Collaborative editing** of a report by several people at once.

## Appendix: what the code does today

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
- **Plugins.** `plugins/mod.rs` embeds the manifests (`MANIFESTS`, `:42`), parses
  them with `deny_unknown_fields`, computes `active` (`:212`, on AND footprint
  present, empty footprint meaning always) and `spawn_allow` (`:244`);
  `plugins/tools.rs` serves plugin tools and instruction paragraphs where active
  (`instructions` `:20`, `defs` `:43`, `call` `:108`). Built contribution points:
  `detect`, `requires.agent_plugins`, `setup.prompt`, `provides.knowledge`,
  `provides.mcp_tools`; `views`, `settings` and `commands` are specified only.
  `agent_probe.rs` reads each installed claude plugin's `installPath` (`:310`,
  `:794`). The UI renders any manifest generically
  (`web-ui/src/lib/plugins/InstalledView.svelte`).
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

Checked 2026-09-25. Several official doc sites were unreachable from the research
environment, so some facts come from the projects' source files and release pages
instead; those links are what is cited.

- **Tectonic**: [changelog](https://github.com/tectonic-typesetting/tectonic/blob/release/CHANGELOG.md)
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

**Not verified, measured in Phase A instead**: Typst's time and memory for a
20-page report; Tectonic's memory per compile and its official cache size; whether
Tectonic honors `max_print_line`; whether Tectonic confines absolute-path reads; how
long a login shell with `module load texlive` takes on a busy login node; whether
`typst eval` can report heading positions.
