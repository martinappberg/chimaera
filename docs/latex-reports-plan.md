# LaTeX and Typst reports: the plan

Dated 2026-09-25, revised 2026-09-26 and 2026-09-28. A plan, not a record: nothing
here has shipped. It covers compiling LaTeX and Typst documents on the host, showing
the PDF beside the source, jumping between the two, turning compile errors into
editor marks an agent can fix, showing what changed (in the source and in the PDF),
teaching agents to write reports here and, later, markdown to PDF and Word. It was
split out of the documents plan (`docs/document-workbench-plan.md`, shipped as
martinappberg/chimaera#159) and built from reads of the tree (last at `c91b3ca`)
plus a survey of Tectonic, TeX Live, latexmk, Typst, SyncTeX, pandoc and the tools
that already do this (VS Code's LaTeX Workshop, texlab, tinymist, Overleaf's
compile limits). The 2026-09-26 revision made this a pair of workbench plugins; the
maintainer reversed that on 2026-09-28 ([decisions](#decisions-maintainer-2026-09-28)),
and this revision is the result: core, kept lean. Claims about the current code
are traced in the [appendix](#appendix-what-the-code-does-today); outside facts are
in [sources](#sources).

## Decisions (maintainer, 2026-09-28)

1. **LaTeX and Typst are core, not plugins.** They are file formats, and Chimaera's
   file support is core: Word, PowerPoint, notebooks, Parquet and slides all open
   without an install ([previews](features/files-and-previews.md)). A `.tex` or
   `.typ` file opens as source beside its PDF, with no plugin to install and nothing
   to switch on.
2. **Kept lean.** Core grows by as little as the feature needs. What that means in
   practice (one small daemon module about the size of `compute.rs`, the engines
   doing the heavy work, SyncTeX parsed in the browser, no new agent tool) is this
   plan's proposal, set out in [what core gets](#what-core-gets-and-what-it-does-not).

Also decided the same day, on the plan's open questions:

3. **`check_document` stays pre-allowed** when it builds `.tex` and `.typ`: agents
   run the check-and-fix loop without a permission prompt; the build's own limits are
   the guard ([agents](#check_document-builds-tex-and-typ)).
4. **Build files go to `~/.cache/chimaera/build`, capped**: 512 MB per document and
   1 GB in total, least recently built evicted first; a setting moves the folder
   ([build output](#where-build-output-goes)).
5. **Compile on open and on agent writes**, with a per-document toggle
   ([compile on save](#compile-on-save)).
6. **TeX Live's restricted shell escape stays** as the site configured it
   ([security](#security)).
7. **Changes compare against the last commit** by default; Before this turn when the
   file is not in git ([choosing the base](#choosing-the-base)).
8. **Typst is the recommended format for new agent reports**; LaTeX for a journal
   template or an existing project ([the guide](#document_guide-and-the-instructions)).
9. **Markdown to PDF starts with the small Typst template**; a converter in core only
   if its gaps matter ([markdown to PDF](#6-markdown-to-pdf-and-word-later)).
10. **No Tectonic.** LaTeX builds with the host's TeX Live only; a host without it
    gets clear directions instead of a second, older LaTeX
    ([why](#latex-the-hosts-tex-live)). Typst is the one engine Chimaera installs,
    with one click, like an agent CLI; the user's own Typst wins.

**Why not a plugin** (the reasoning the decision rests on). The workbench plugin model
([plugin system plan](plugin-system-plan.md)) exists for add-ons that are someone
else's framework on their own release cadence (Mycelium), or optional and
experimental (Agent notes), where a WASM sandbox makes a third party's code safe to
switch on. None of that fits LaTeX: it is a universal format, and the sandbox cannot
contain it anyway, because building a PDF means running `latexmk` with the user's
permissions. The plugin route also cost more than it saved. The careful parts (the
runner and its limits, the PDF view, marks, jumps, change highlights) were core in
both designs; the plugin would have held about 1,500 lines of parsers, and moving
them out needed a new WIT world, a plan and digest protocol, a consent dialog, two
repositories and an install step for every user. Plugins stay what they are for.

## The short version

- **Open a `.tex` or `.typ` file and it works.** Source and PDF side by side, with no
  plugin and no switch. A host without Typst is one click from it.
- **Compile on the host, never in the browser.** The daemon runs the host's own
  engine as a small, limited child process, only when a document is open or an agent
  asks. Only the PDF crosses the tunnel, and only the pages you look at.
- **The host's LaTeX, and Typst on one click.** LaTeX builds with the host's TeX Live
  through latexmk: whatever `module load texlive` puts on PATH in a terminal. No
  TeX Live, no LaTeX build, and the view says how to get it. Typst (a newer, much
  faster typesetting language with simpler syntax): your own `typst`, else one click
  installs it the way agent CLIs are installed
  ([installs](#installing-typst-like-an-agent)). No engine ships in the binary.
- **Compile on save.** Debounced, one job at a time, niced, time-limited, output
  capped. An agent's write to any file of the document recompiles it too while it is
  open. Build files go to a cache folder, never into the repository.
- **Errors become editor marks**, with **Ask agent**, which types one precise
  reference into the agent's composer.
- **Jumps both ways.** Cmd-click the PDF to open the source line; a shortcut
  highlights the source line in the PDF; selecting PDF text makes a normal
  `@report.tex#L120-L128 "quote"` reference. Typst gets text matching.
- **Multi-file projects work**: the main file comes from a `% !TEX root` comment, a
  `\documentclass`, or the last build's list of inputs.
- **Changes are visible where people read.** Change bars in the source against the
  last commit, a branch or the start of an agent's turn, with a word diff that
  ignores rewrapped paragraphs, and the same changes marked beside the lines of the
  PDF.
- **Agents check their own work with the tool they already have.** `check_document`,
  which every session has, also builds `.tex` and `.typ` files and returns the
  compile errors. `document_guide` gains a short LaTeX and Typst section.
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
3. **Lean.** The engines already rerun passes, run bibliographies and report errors;
   Chimaera runs them safely and shows the result. Every feature here earns its lines,
   and the ones that do not are [out](#what-core-gets-and-what-it-does-not).
4. **Never litter the repo.** Aux files, logs and PDFs live in a build cache. The repo
   only changes when the user asks for a PDF beside the source.
5. **The daemon stays small.** Engines are child processes under hard limits. Logs are
   streamed and capped. SyncTeX is parsed in the browser
   ([daemon rules](../.claude/rules/daemon.md)).
6. **Opening a file never runs project code without consent.** A project `latexmkrc`
   is Perl; unrestricted shell escape is arbitrary commands. Both need an explicit,
   per-workspace yes.
7. **Not an IDE.** No language server, no completion, no refactoring
   ([DESIGN.md](../DESIGN.md#scope-philosophy-and-non-goals)). Compile, show, jump,
   point, fix.
8. **One pipeline for people and agents.** An agent's `check_document` and the user's
   save run the same queue, the same limits and the same parser, and update the same
   preview.

## What core gets, and what it does not

**Daemon: one module, `crates/chimaera-server/src/build/`**, about the size of
`compute.rs` (1,250 lines with its tests):

| File | What |
|---|---|
| `mod.rs` | the routes, the `{"type":"doc"}` frame on `/ws/events`, and the `check_document` hook |
| `run.rs` | detection through the prelude, the queue, every limit, build folders and eviction |
| `latex.rs` | the ladder, finding the main file, the log parser (with its fixture corpus) |
| `typst.rs` | the command and its short diagnostics |

It copies the patterns already in the tree instead of inventing new ones:
`compute.rs`'s capped, killed-on-timeout child processes and PATH walk, the
environment prelude's materialization, the fs watcher's stat sweeps, and the `/raw`
tickets. It is dormant until a `.tex` or `.typ` file is opened or an agent checks
one: no detection at boot, no background work, no memory held.

**Web UI:** `DocumentView.svelte` (source, split, PDF), a three-state
`SplitEditPreview`, additions to `PdfView` (in-place reload, highlight boxes,
Cmd-click, change marks), a SyncTeX parser in a Web Worker, diagnostics through
`@codemirror/lint`, the change bars, and a lazily loaded Typst grammar. Every chunk
loads only when a document opens.

**Installs:** a curated Typst recipe beside the agents' in `runtimes.rs`, reusing its
layout, checksums, visible terminal and update check.

**Agents:** no new tool. `check_document` builds `.tex` and `.typ`; `document_guide`
gains a section; the documents paragraph gains one sentence.

**Deliberately not built** (each is either out of scope or a later, separate step):

- no plugin, no manifest point, no WIT change;
- no engine in the binary, no WASM engine, and no Tectonic (Typst installs on a click);
- no language server, completion or refactoring;
- no copy of SyncTeX in the daemon: the browser parses it;
- no second build of an old revision in the first version (before-and-after and the
  latexdiff changes PDF are [later](#later-before-and-after-and-a-changes-pdf));
- no markdown-to-Typst converter in core ([markdown to PDF](#6-markdown-to-pdf-and-word-later)).

A setting, **Documents → Build LaTeX and Typst** (on by default), turns the whole
thing off: `.tex` and `.typ` then open as plain text, exactly as today, and
`check_document` checks them as markdown, as it does now.

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
  zsh and fish. Look for `latexmk`, `pdflatex`, `xelatex`, `lualatex`,
  `biber`, `bibtex`, `synctex`, `typst`, `pandoc`, and later `pdftoppm`. Each found
  tool reports its version (`--version`, 5 s timeout, capped output).
- **Cached** in memory per prelude text hash. A `PUT /api/v1/environment` or a
  **Check again** click invalidates it. Nothing is persisted.
- **Test knob.** `CHIMAERA_DOC_BINDIR` points at stand-in engines, like
  `CHIMAERA_SLURM_BINDIR`, so the whole flow runs in CI without TeX.

### LaTeX: the host's TeX Live

LaTeX builds only with **latexmk and the TeX Live the host already has**. For a
given main file:

1. **A project `latexmkrc`** is honored, its Perl running only behind the
   [trust gate](#security).
2. **The engine** comes from a `% !TEX program = xelatex | lualatex | pdflatex` magic
   comment, else a per-workspace choice in the toolbar, else pdfLaTeX.
3. **latexmk and that engine on PATH** build it.
4. **Otherwise nothing builds**, and the empty state says exactly what is missing
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
is missing, Typst covers new reports, and an existing LaTeX project's authors usually
have TeX Live already.

**Is TeX Live common?** Mostly where LaTeX gets written, and rarely by default:

- **HPC and university clusters:** usually available, as a module or a system
  package; Settings → Environment is where `module load texlive` goes.
- **Linux workstations and servers:** only if someone installed it. It is one
  package-manager command with admin rights (on Debian and Ubuntu `latexmk` is its
  own package), or TeX Live's own installer into the home folder without them.
- **Macs:** not by default. People who write LaTeX usually have MacTeX (about 5 GB) or
  the smaller BasicTeX.
- **Fresh cloud machines and containers:** usually not.

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
3. **Chimaera's Typst**, installed with one click like an agent CLI.
4. **None**: the empty state offers that install.

A long-lived `typst watch` process would make recompiles near instant, but it is one
resident process per open document on a shared node, and Typst has no memory limit of
its own. Start with one-shot `typst compile` per save and measure a real 20-page report
in Phase A (no published benchmark exists); reach for `watch` only if it takes over a
second. If font discovery on a network filesystem turns out slow,
`--ignore-system-fonts` plus the project's own fonts is the knob.

### When there is no engine

The document still opens and edits exactly like any text file today. The PDF side
shows a calm empty state instead of an error:

- **No TeX Live on this host.** "Chimaera builds LaTeX with the TeX Live your
  terminals get on *sherlock*, including your environment prelude, and didn't find
  latexmk." Then the one step that fits the host: on a cluster, **Open Environment
  settings** with a hint such as `module load texlive` (shown, never written for
  them); elsewhere, how to install TeX Live on this system, with a link. When TeX
  Live is there but latexmk is not, it names the missing package. **Check again**
  re-detects.
- **No Typst on this host.** **Install Typst** (one click, below), which says what it
  installs and where before it runs.
- **A PDF already sits beside the source** (`report.pdf` next to `report.tex`): show
  it, with a banner "built elsewhere; may be older than the source" when its mtime is
  older than the source's.
- **Agents** get the same facts as text from `check_document`
  (`status: no_engine`, what was searched, and how the user can fix it), never a
  failure without words.

### Installing Typst like an agent

**Not bundled in the binary; installed with one click, the way agent CLIs are.**
Chimaera already installs claude and codex for users who do not have them: a click
on an install chip runs a curated script in a visible terminal (official release
files, checksums checked, never sudo), puts the program under `~/.chimaera`, and
labels it **chimaera** beside the user's own copies, labelled **yours**
([agents](features/agents.md#managed-runtimes--install--update--theming-shims)).
Typst gets the same treatment, with the same rule: **yours always wins**.

- **Typst** installs as one static binary (16.7 MB download for 0.15.1). That is the
  whole engine: a Chimaera-installed Typst builds any Typst document.
- **TeX Live cannot be installed this way.** It is several GB and belongs to the host's
  admins or the user's own setup, and Tectonic, the small alternative, is
  [not supported](#latex-the-hosts-tex-live).
- **The layout is the agents' layout:** `~/.chimaera/tools/typst/<version>/` behind an
  atomic symlink swap, `~/.chimaera/tools/bin` placed *after* the user's own PATH so
  their Typst wins, an **update →** chip when a newer release exists, and uninstall
  from Settings. Nothing installs without a click.
- **In the binary instead?** No: every host would carry 17 MB it may never use, and
  every deploy and update over ssh would move it. Installing on first use gives the
  same one-click result without the weight.

**A WASM engine in the browser** is rejected as the main path:
  - typst.ts's compiler is 28 MB (11 MB gzipped), and it fetches fonts and packages
    from the internet on top.
  - The live LaTeX ports are heavier: TeXlyre's BusyTeX build is about 32 MB of WASM
    plus 90 to 400 MB of TeX data (and AGPL); LibrePaper's needs an 18 MB core bundle
    before any package. SwiftLaTeX has had no release since February 2022.
  - Every byte crosses the tunnel into every browser, then every source file and
    figure must follow before a cold compile.
  - Agents on the host could not use it at all, so they could not check their own
    reports.

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
  agent's `check_document` on one does.
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

- **The parser is in the daemon** (`build/latex.rs`; it also feeds
  `check_document`), streaming the log line by line with a 16 MB scan cap. It follows the TeX file
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
  re-joins lines of exactly 79 characters, kept as a fallback for a site whose
  configuration ignores `max_print_line`. A shared fixture corpus of real logs
  (pdflatex, xelatex, lualatex, biber, bibtex, Typst) runs in the Rust suite, like
  `mathBlocks.fixture.json` does for math.
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
  `Fix the 3 compile errors in @report.tex (check_document lists them) `.
- It works for agents that do not have the chimaera MCP server too: the path, line,
  message and log location are all plain text any agent on the host can use.

## 3. Source and PDF: SyncTeX

### Where the data comes from

latexmk (`-synctex=1`) writes `main.synctex.gz` into the
build folder. It maps typeset boxes to source file and line.

### Parse it in the browser

- A TypeScript SyncTeX parser runs in a Web Worker: LaTeX Workshop's `synctexjs.ts`,
  an MIT port of synctex-js, is the starting point. It loads `main.synctex.gz` through
  a `/raw` ticket on the first sync action, decompresses with the browser's own
  `DecompressionStream`, and is cached per build version (the ticket's `ETag` makes an
  unchanged build a 304). LaTeX Workshop already uses its JS parser alone for inverse
  search, because the `synctex` binary mishandles some non-ASCII paths.
- **Why the browser.** Sync must feel instant, and every daemon round trip costs about
  two tunnel round trips ([remote perf plan](perf-remote-plan.md), F2). Parsing a
  thesis-sized SyncTeX file would also take tens of MB of daemon memory, and running
  the `synctex` command-line tool per click would cost a process and a round trip
  each time.
  The same parsed data feeds the change marks of
  [section 8](#8-changes-git-differences-in-the-source-and-the-pdf).
- **Cap.** Over 16 MB compressed, sync turns off with a note.
- **Units and paths.** The file stores scaled points plus a unit and offsets from its
  preamble; there are 65,781.76 scaled points to a PDF point, and SyncTeX measures
  from the page's top left while PDF measures from the bottom left. Input paths are as
  the engine saw them, absolute or relative; they are
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
tinymist's preview uses. The `typst` command line does not expose them. The lean
answer is the first of three options:

1. **Text matching (Phase C, recommended first).** Typst prose is very close to its
   source. Inverse: take the clicked or selected PDF text (pdf.js text layer) and find
   it in the member files, ignoring markup, whitespace and hyphenation; ties go to the
   file the editor shows. Forward: take the words around the cursor and find them in
   the page texts. Headings and plain paragraphs map well; math, tables and generated
   text do not. The UI says "approximate" and never pretends otherwise. Phase C also
   tests whether `typst eval` (new in 0.15, replacing the deprecated `typst query`)
   can report heading positions, which would anchor the matching per section.
2. **A small companion binary (only if text matching disappoints).** Built from
   Typst's own crates, it
   answers exact jump queries. Cost: it pins a Typst version separate from the host's
   `typst`, compiles the document a second time itself, and needs updating with every
   Typst release. Linking the compiler into the daemon instead is ruled out by the
   memory budget.
3. **tinymist, when the host has it.** Its preview does exact two-way jumps, but it is
   a 32 MB, long-running server with its own web preview and its own data plane. It
   could open in the browser pane for users who already use it; it does not fit
   `PdfView`.

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
   to two parents (at most 200 files, first 8 KB each, off the reactor behind the
   filesystem semaphore) whose
   `\input`, `\include`, `\subfile` or `\import` lines name this file. Exactly one
   match wins.
5. **Ask once.** Several candidates, or none: a small picker in the toolbar. The
   answer is remembered per workspace in a small capped JSON file under
   `~/.chimaera`.
6. **Project files.** A trusted `latexmkrc`'s `@default_files` names its own main
   files.

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
- **Typst** reads `.bib` (BibLaTeX) or Hayagriva `.yml` natively with
  `#bibliography("refs.bib")`. No extra tool, no extra pass. One more reason to
  recommend Typst for new reports.
- `.bib` files open as text. Saving one recompiles its main document through the
  watch set.

## 5. Agent awareness

Every Claude and Codex session Chimaera spawns already loads the chimaera MCP server
(`mcp.rs`), and every tier already has two document tools, both pre-allowed:
`document_guide` (the portable markdown dialect) and `check_document` (the checker
behind the reading view's issues chip). This plan adds no tool. It widens those two.

### `check_document` builds `.tex` and `.typ`

- **Same tool, same argument.** Given a `.tex`, `.ltx` or `.typ` path (any member
  file; the main file is found as in [section 4](#finding-the-main-file)), it runs the
  same build a save runs, through the same queue and limits, and answers with what the
  agent needs: `status` (`ok`, `errors`, `failed`, `timeout`, `no_engine`,
  `still building`), the engine and version, the main file, the PDF's path, pages and
  size, up to 20 errors as `file:line: message` plus one context line, undefined
  references and citations (up to 10), a count of box warnings, and the log's path.
  At most 8 KB.
- **It joins a build that is already running** for the same inputs instead of
  starting another, and it updates the user's view: an open PDF refreshes, and the
  chip says which agent built it.
- **It never waits past about 45 s.** Agents' MCP clients time long tool calls out
  (Codex's per-tool default is about a minute; Phase A checks both agents). A longer
  build answers `still building`, and the next call picks up the same build.
- **Its description gains one sentence**, "For a .tex or .typ file it builds the
  document and returns the compile errors instead." That changes the core tool list
  for every session once, on purpose; the `agent_view` fixtures are updated in the
  same change.
- **Still pre-allowed.** It now runs an engine, but a bounded one: no unrestricted
  shell escape, no untrusted project rc, no automatic build with an old, vulnerable
  LuaTeX ([security](#security)), output only in the cache folder.
- **The PDF stays where it is.** An agent that wants `report.pdf` beside the source
  copies it from the path the tool returns, like any file.
- **Log excerpts are data.** They quote the document, which may come from an untrusted
  repository; the answer says so.

### `document_guide` and the instructions

- **The guide gains a section** (in `doc_guide.md`, about 2 KB): when to choose Typst
  (new reports) or LaTeX (a journal template, an existing project); one main file per
  report, included LaTeX files starting with `% !TEX root = main.tex`, Typst's one
  `main.typ` that `#include`s the rest; figures as PDF for vector plots and PNG at
  300 dpi or more, relative paths, no EPS; the paper size set explicitly (TeX Live
  follows its site setting, so leaving it out gives different PDFs on different
  hosts); one bibliography tool per
  project (Typst's `#bibliography("refs.bib")`, or biblatex with biber, or natbib
  with bibtex); no packages that need unrestricted shell escape (`svg`, TikZ
  externalization, minted before version 3), no `\write18`, no absolute paths, no
  fonts the host lacks, Typst `@preview` packages only with a pinned version; build
  output never written into the repository; and the loop: check, fix errors, then
  undefined references and citations, then the page count.
- **The documents paragraph** in the MCP instructions gains one sentence: "For a
  report, run check_document on the .tex or .typ file; it builds the PDF and returns
  the compile errors." The engines on this host are not listed there: `initialize`
  must never trigger a login-shell detection. `check_document` names the engine it
  used, and `no_engine` says how the user can add one.

### Later: `render_page(path, page)`

One page as a PNG (MCP image content) so multimodal agents can see a figure running
off the page or a table that overflows. Typst renders PNG itself
(`--format png --pages N --ppi …`); LaTeX PDFs need `pdftoppm` on the host. Only if
agents turn out to need it; it would be the one new tool, with caps of one page per
call, about 1.5 megapixels and 1 MB.

## 6. Markdown to PDF, and Word (later)

Agents write the portable markdown dialect. Some of it should leave as a real report:
a title block, page numbers, a table of contents, numbered figures. The lean answer
uses what this plan already has, a Typst build, and adds no converter to core.

| Option | Needs on the host | Verdict |
|---|---|---|
| Browser print of the reading view | nothing | Keep for "what I see"; not typeset (no running heads, no page-aware floats). |
| **A small Typst template that reads the markdown itself** (`cmarker` 0.1.10 for markdown, `mitex` for math) | typst 0.15 or newer | **Recommended first.** A few KB of template in core, no parser. Typst fetches the two pinned packages on first use and caches them. Gaps: no GitHub alerts (they read as quotes), and cmarker's raw-Typst comments must be turned off in the template. |
| pandoc to Typst (`--pdf-engine=typst`, since pandoc 3.1.2) | pandoc (3.11, a 33 MB download) + typst | Offered when pandoc is present; GitHub alerts are on by default for `gfm` input. Never required. |
| Quarto (`format: typst`) | Quarto (140 MB; bundles pandoc, Typst, Deno) | Too heavy to depend on. |
| A comrak-to-Typst writer in core | typst | Full control and parity with the reading view, but several hundred lines of converter. Only if the template's gaps turn out to matter. |

**Export PDF** on a markdown document runs the template through the same queue into
the build folder (`typst compile` with the template on standard input and the
markdown's folder as `--root`), opens the PDF beside it, and offers **Save beside
source**. Frontmatter `title`, `summary` and `updated` fill the title block;
`template: path/to/mine.typ` picks a project's own template. On a host without
network the first export needs the two packages pre-seeded in Typst's package cache,
and the error says so.

**Word.** Word files open read-only today (`DocxView`). The lean path to Word is
pandoc when the host has it: **Export to Word** (`pandoc report.md -o report.docx`,
with a project's `--reference-doc` when it has one) and **Open as markdown** for a
`.docx` (`pandoc report.docx -t gfm --extract-media=figures`), both through the same
runner. Editing a `.docx` in place, styles and tracked changes kept, needs an OOXML
editor that fits neither the licence nor the footprint; it stays out.

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
- **Shell escape.** Unrestricted shell escape (`-shell-escape`) is off and can only be turned on by the user, per project, in the
  UI; never by an agent and never by a file in the repo. TeX Live's own default,
  **restricted** shell escape, stays as the site configured it: a short list of helpers
  (`bibtex`, `kpsewhich`, `makeindex`, `repstopdf`, `latexminted` and a few more).
  Documents rely on it for EPS figures and minted code listings. That list has had
  holes (`mpost` allowed arbitrary commands until it was replaced by `r-mpost`,
  CVE-2016-10243); the maintainer kept the restricted default on 2026-09-28 because
  documents rely on it. Typst has no shell escape at all.
- **Old LuaTeX can run commands anyway.** LuaTeX 1.04 to 1.16 (TeX Live 2017 to 2022
  and the first TeX Live 2023) could run shell commands even with shell escape off
  (CVE-2023-32700) and open network sockets (CVE-2023-32668). HPC modules are often
  old. Detection records the LuaTeX version; below 1.17.0, a LuaLaTeX document never
  compiles on open, on an agent's write or for an agent's `check_document`, only on
  the user's own save or **Build**, and the chip (or the tool's answer) says why.
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
  above). Typst downloads `@preview` packages on first import; that is the engine
  fetching its own packages, allowed. Typst has no offline switch, but a cached package
  never touches the network, and a missing one on an offline host becomes a
  plain-words diagnostic. Chimaera does not sandbox the
  network itself, for the same reason as reads.
- **Environment hygiene.** Compiles get the captured prelude environment, minus the
  daemon's own variables and anything on `api::spawn_env_remove`. No token ever reaches
  an engine.
- **Routes.** Every new route is bearer-authed. Build PDFs and SyncTeX files are served
  through the existing short-lived `/raw` tickets. **Save PDF beside source** is
  confined to the workspace.

## 8. Changes: git differences in the source and the PDF

Reports change in two places at once: the source an agent edits and the PDF a
person reads. People ask about both: "what did the agent just change in my
report?" and "what changed since the last commit, or on this branch?". Today the
answer is the source-control panel's side-by-side diff of the `.tex` (`DiffView`,
from `GET /git/diff`), which shows a rewrapped paragraph as entirely new and says
nothing about the PDF.

The first version does two views that need no extra build.

### Change bars in the source

- The document's editor gets a **Changes** toggle with a base picker (below).
- Prose needs one thing code does not: agents rewrap paragraphs, and a line diff then
  marks the whole paragraph. For `.tex`, `.typ` and `.md` the diff compares words
  within a paragraph (split on blank lines), ignoring where the line breaks fall,
  and maps the result back to lines (`doc/changes.ts`). The marks are gutter bars
  and word tints drawn through `CodeView`'s `extra` compartment, with each deleted
  passage a one-line wedge that expands on click. Other text files use CodeMirror's
  `unifiedMergeView`, already in the tree through `@codemirror/merge`.
- Colors are the existing `--git-added`, `--git-modified` and `--git-deleted`
  tokens. Keys step to the next and previous change. Hovering one offers **Copy old
  text** and **Ask agent**. There are no accept or reject controls: git stays
  read-only here ([git](features/git.md)).
- `DiffView` gains the same prose mode as a toggle.
- The base text comes from the existing `GET /git/diff` (two blobs, each capped at
  2 MB), once per open file and base, refetched on the git epoch.

### Change marks on the PDF

- For each changed source range, the build's SyncTeX data (already in the browser
  for jumps) gives the boxes it produced. The PDF draws a thin bar in the margin
  beside them and tints the changed words where the text layer can place them
  (`pdfFind.ts`'s `pageText` and `itemRanges` already map text to text-layer items
  for find).
- Deleted text has no box: a small mark sits in the margin at the nearest surviving
  line, and hovering it shows the removed words.
- The page indicator gains "3 changed pages" and ‹ › buttons.
- For Typst, or a LaTeX build without SyncTeX, the same marks come from text
  matching: the changed words are found in the page texts, the way find works.
- Marks follow the last good build. When the source has moved on since, they map
  through the editor's changes, as jumps do.

### Choosing the base

- **Last commit** (HEAD), the default when the document is in git; **staged** (the
  index). Both are served by `GET /git/diff` today.
- **A branch** (its merge base with this one, for "what this branch changes") or
  **a commit** from the document's recent history. These need two small additions,
  through the existing bounded git runner: `rev=` on `GET /git/diff` (a ref checked
  with `check-ref-format` and resolved with `rev-parse --verify` first), and
  `GET /git/log?path=&limit=` (at most 50 commits).
- **Before this turn**: the document as it was when an agent's turn started. No git
  needed, so it works for a report nobody has committed. When a build lands, the
  daemon keeps the previous good build's source inputs (up to 8 MB) and SyncTeX data
  beside the new ones, one generation; a Timeline `episode` (one per agent turn)
  records which build was current when the turn started.

### Where changes show up outside the document

- **The source-control panel:** a changed `.tex` or `.typ` row gets **Changes in
  PDF**, which opens the document with Changes on and the row's mode as the base.
- **Timeline episodes and the chat's turn-end block:** when a turn wrote a file of a
  document with a build, one chip, "thesis.pdf · 3 pages changed", opens it with
  **Before this turn** as the base.
- **Pointing at a change:** selecting inside a mark makes the usual
  `@thesis.tex#L120-L128 "…"` reference; the hover card's **Ask agent** types one
  capped line with both sides:
  `@thesis.tex#L120-L128 changed since HEAD: "the effect held" → "the effect held in 3 of 4 cohorts" `.
- **Agents:** `check_document`'s answer adds "pages changed since the previous
  build: 3, 7, 12" when there are some. For the text diff they run `git diff`.

### Later: before-and-after, and a changes PDF

Two heavier views wait until someone asks for them, because both need a second build
of an old revision (the daemon writing that revision's inputs into the build folder
with `git show`, bounded, then building them at low priority):

- **Before and after:** the old and new PDF side by side, page by page, with the
  changed words marked on both (a text diff per page in the browser), and a page-image
  mode for figures. It would also give a committed `report.pdf` a real comparison
  instead of "binary" (a `/raw` ticket for a git blob).
- **A changes PDF for co-authors:** `latexdiff --flatten` (part of a full TeX Live)
  writes a marked-up `.tex` from the old and new sources, and the same runner builds
  `thesis-changes.pdf`, insertions underlined and deletions struck. latexdiff
  stumbles on some tables and custom macros; the chip would say so. Typst has no
  equivalent.

### Costs

One `/git/diff` per open file and base (at most two 2 MB blobs), a word diff in the
browser (above 20,000 lines it falls back to lines), and no build. Nothing polls:
the git epoch refetches. Every git call runs through the existing runner (a timeout
that kills the child, output caps, a concurrency permit).

## What changes where

The daemon module is in [what core gets](#what-core-gets-and-what-it-does-not).
Around it:

| Where | What |
|---|---|
| `crates/chimaera-server/src/mcp.rs`, `doc_check.rs`, `doc_guide.md` | `check_document` routes `.tex`, `.ltx` and `.typ` to the build module; the guide section; the instructions sentence; the `agent_view` fixtures updated once |
| `crates/chimaera-server/src/git/` | `rev=` on the diff route and `GET /git/log` (for branch and commit bases) |
| `web-ui/src/lib/previews/` | `files.ts` (the `document` kind), `DocumentView.svelte`, `SplitEditPreview.svelte` (three-state `show`), `PdfView.svelte` (in-place reload, boxes, Cmd-click, change marks), `doc/compile.svelte.ts` (status, the events frame), `doc/diagnostics.ts`, `doc/synctex.ts` + `doc/synctex.worker.ts`, `doc/textSync.ts` (Typst), `doc/changes.ts`, `doc/typstLang.ts` |
| `web-ui/src/lib/workspace/`, timeline, chat turn-end | the **Changes in PDF** row action and the "pages changed" chip |
| `web-ui/src/lib/shared/reference.ts` | the compile-error and change composers |
| settings | **Documents → Build LaTeX and Typst**, the build folder, compile on open and agent writes |
| docs | the feature pages for files and previews, git and agents when each phase ships; this plan's status |

New routes, all bearer-authed and additive: `GET /api/v1/doc/engines`
(`?refresh=true` re-detects), `POST /api/v1/doc/compile {path, reason}` →
`202 {main, version}`, `GET /api/v1/doc/status?path=` (engine, state, PDF, SyncTeX
path, counts, diagnostics), `PUT /api/v1/doc/main {path, main}`,
`POST /api/v1/doc/trust {path, file, hash}`, `GET /api/v1/git/log`, `rev=` on
`GET /api/v1/git/diff`, and the `/ws/events` frame `{"type":"doc", …}`.

## Phases

### Phase A: agents first (daemon only)

The build module: detection through the prelude, the queue and every limit, build
folders, the main-file rules, both log parsers with their fixture corpus, the routes
and the events frame; `check_document` on `.tex` and `.typ`, the guide section and
the instructions sentence.

Agents get value before any UI exists: they can build a report and fix it.

**Verification.** Rust tests with stand-in engines (`CHIMAERA_DOC_BINDIR`): the queue,
coalescing, single-flight, timeouts killing a whole process group, the file-size
limit, eviction, env scrubbing, the untrusted-rc and old-LuaTeX gates,
`check_document`'s 45 s answer; the updated `agent_view` fixtures. Live on a real
login node with `module load texlive` and a real `typst`: an article, a thesis-shaped
`\include` project with biber, an infinite-loop document, a Typst document that
allocates without bound, and a claude and a codex session each running the
check-and-fix loop. Record what the research could not find: prelude capture time,
Typst time and memory for a 20-page report, and
both agents' MCP tool-call timeouts.

### Phase B: the document view

`DocumentView`, compile on open, on save and on agent writes to the open file or its
main file, the in-place PDF swap, error marks, the problems list, **Ask agent**, the
status chip, the empty states with the TeX Live directions and **Install Typst** (the
curated recipe in `runtimes.rs`), **Save PDF beside source**, the setting.

**Verification.** Driven live in the isolated preview on Chromium and WebKit, against a
remote daemon over a real tunnel: type, save, watch the PDF swap without a flash; let
an agent edit a chapter and watch the PDF follow; break a macro and send the error to
the agent. A `scripts/perf/` scenario measures bytes per rebuild of a 20 MB report.

### Phase C: jumps and references

The SyncTeX worker, both directions, follow cursor, selections to `@file.tex#Lx-Ly`,
text matching for Typst.

### Phase D: multi-file depth

The watch set from `.fls` and Typst's deps, remembered main files, the bibliography
messages, switching the split's editor between member files.

### Phase E: changes

Change bars with the prose word diff, PDF change marks, **Before this turn**, the
branch and commit bases, the panel action and the turn chips.

**Verification.** Live: commit a chapter, let an agent rewrite a paragraph and
rewrap it, and check that the bars mark the changed words and not the whole
paragraph, that the PDF marks sit beside the right lines, and that the turn chip opens
the right comparison; review a branch against `main`.

### Phase F: markdown to PDF

The template, **Export PDF**, pandoc when present.

| Phase | What | Size |
|---|---|---|
| A | The build module, `check_document`, the guide | medium |
| B | Document view, compile loop, error marks | large |
| C | Jumps both ways, source references | medium |
| D | Multi-file depth | small |
| E | Changes in the source and the PDF | medium |
| F | Markdown to PDF | small |

A comes first; B needs A; C and D need B and can run in parallel; E needs C (the
SyncTeX data in the browser); F needs only A. Later, each only when asked for:
before-and-after and the changes PDF, `render_page`, Word through pandoc, the Typst
jump companion.

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
- **The dialect and `check_document`** (its Phase 6): both tools exist, and this plan
  widens them rather than adding tools.

## Open decisions

None. Everything the plan asked was decided on 2026-09-28: core, not a plugin; kept
lean; and the questions listed under [decisions](#decisions-maintainer-2026-09-28),
including LaTeX through the host's TeX Live only and Typst installed with one click.
New questions will surface in Phase A's measurements; they go here.

## Out of scope

- **A plugin for LaTeX or Typst**, or any change to the plugin interface for them
  ([decisions](#decisions-maintainer-2026-09-28)).
- **A language server, completion or refactoring** (texlab, tinymist): the DESIGN.md
  non-goal.
- **A WASM engine in the browser**, **bundling** Typst or TeX Live in the binary
  ([why](#installing-typst-like-an-agent)), and **Tectonic**
  ([why](#latex-the-hosts-tex-live)).
- **Managing TeX packages** (`tlmgr`). The host's admins and the user's prelude own the
  TeX installation.
- **Committing, staging or reverting from the changes views.** Git stays read-only
  here; the views show and point, the terminal commits.
- **Editing a `.docx` in place**, styles and tracked changes preserved.
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

Checked 2026-09-25. Several official doc sites were unreachable from the research
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
  Rust converter behind the Typst package; crate and licence to confirm).

**Not verified, measured in Phase A instead**: Typst's time and memory for a
20-page report; how long a login shell with `module load texlive` takes on a busy login node; whether
`typst eval` can report heading positions.
