# LaTeX and Typst reports: the plan

Dated 2026-09-25. A plan, not a record: nothing here has shipped. It covers
compiling LaTeX and Typst documents on the host, showing the PDF beside the source,
jumping between the two, turning compile errors into editor marks an agent can fix,
teaching agents to write reports here, and turning markdown into a polished PDF. It
was split out of the documents plan (`docs/document-workbench-plan.md`, still on its
own branch; its "Out of scope" section hands this effort over) and built from a read
of the current tree (commit `f44a8b7`) plus a survey of Tectonic, TeX Live, latexmk,
Typst, SyncTeX, pandoc and the editors that already do this (Overleaf, VS Code
LaTeX Workshop, tinymist, the Typst web app). Claims about the current code are
traced in the [appendix](#appendix-what-the-code-does-today); outside facts are in
[sources](#sources).

## The short version

- **Compile on the host, never in the browser.** The files, the figures and the
  agents all live on the host. The daemon runs the engine there as a small, limited
  child process. Only the PDF crosses the tunnel, and only the pages you look at.
- **Use the host's engines, found through the environment prelude.** LaTeX:
  Tectonic if present, else latexmk from the host's TeX Live. Typst: `typst` if
  present. Whatever `module load texlive` puts on PATH in a terminal is what compiles
  here. Chimaera does not bundle an engine; later it offers an opt-in, visible
  install of Typst and Tectonic, the same way it installs agent CLIs today.
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
- **Agents check their own work.** The chimaera MCP server every session loads gains a
  short documents paragraph, a `document_guide` tool and a `compile_document` tool
  that returns the errors. Agents compile, fix and compile again before handing over.
- **Markdown to PDF through Typst.** The portable markdown dialect becomes a polished
  report PDF through a Typst template. Pandoc is optional, never required.
- **Safe on a login node.** No unrestricted shell escape, no project Perl without
  trust, no network except an engine fetching its own packages, hard limits on time,
  memory, CPU priority and output size.

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
- **Why capture instead of a login shell per compile.** `module load texlive` through
  lmod can take seconds on a busy login node. Paying it once per prelude change,
  instead of on every save, keeps compile-on-save fast.
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

1. **Workspace override** (`auto | tectonic | latexmk | off`), set from the document's
   toolbar and remembered per workspace.
2. **Project signals.** A `Tectonic.toml` at or above the main file means Tectonic's
   project mode. A project `latexmkrc` means latexmk (behind the
   [trust gate](#7-limits-and-security-on-a-login-node)).
3. **Engine hints.** A `% !TEX program = lualatex` (or `pdflatex`) magic comment
   means latexmk with that engine, because Tectonic is XeTeX only.
4. **Tectonic** if on PATH.
5. **latexmk** if on PATH together with the needed engine.
6. **None** (below).

Tectonic first matches the request and has real advantages for agent reports: one
self-contained binary, automatic reruns, bibtex built in, intermediates kept in
memory so no aux files land anywhere, and shell escape off by default. Its limits
decide the exceptions: it is XeTeX only, it needs an external `biber` of the matching
version for biblatex documents, and its package bundle tracks a fixed TeX Live
snapshot rather than the site's TeX Live. When a document hits one of those, the
status chip says so and offers the switch to latexmk in one click. Whether Tectonic
should instead come *after* latexmk on hosts that have a full TeX Live is an
[open decision](#open-decisions).

The latexmk command, run with the main file's folder as the working directory:

```
latexmk -pdf | -xelatex | -lualatex
        -interaction=nonstopmode -file-line-error -synctex=1 -recorder
        -outdir=<build dir> -norc [-r ~/.latexmkrc] [-r <project rc, if trusted>]
        main.tex
```

Plus `max_print_line=10000`, `error_line=254` and `half_error_line=238` in the
environment, so TeX stops wrapping log lines at 79 characters (the main source of
log-parser bugs). No `-halt-on-error`: nonstop mode keeps going and reports several
errors, and the parser ranks the first one. Chimaera never drives `pdflatex`,
`bibtex` or `biber` by hand, because latexmk already knows the output-directory
quirks (bibtex refusing to write outside the working directory, `\include` subfolders,
`BIBINPUTS`).

Tectonic: `tectonic -X compile main.tex --outdir <build dir> --synctex --keep-logs`
(or `tectonic -X build` in a `Tectonic.toml` project).

### The Typst ladder

1. Workspace override.
2. `typst` on PATH: `typst compile main.typ <build dir>/main.pdf --root <root>
   --diagnostic-format short --jobs 2 [--deps <build dir>/deps.json]`.
   `--root` is the workspace root when the file is inside it, else the file's folder.
   `--jobs 2` stops Typst from using every core of a 64-core login node.
3. None.

A long-lived `typst watch` process would make recompiles near instant, but it is one
resident process per open document on a shared node. Start with one-shot
`typst compile` per save and measure; reach for `watch` only if a real report takes
over a second.

### When there is no engine

The document still opens and edits exactly like any text file today. The PDF side
shows a calm empty state instead of an error:

- **No LaTeX engine on this host.** "Chimaera compiles with the tools on
  *sherlock*. It looked for Tectonic and latexmk on the PATH your terminals get,
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

- **In the binary**: Typst and Tectonic are each tens of MB. Every host would carry
  them, including hosts that never compile a report, and every self-update would
  re-download them over the tunnel. TeX Live (several GB) is out of the question.
- **Managed install** (Phase F): the curated-installer pattern in `runtimes.rs`
  already installs agent CLIs from official release artifacts with checksums, as a
  visible shell session, never with sudo. The same pattern installs `typst` and
  `tectonic` under `~/.chimaera/tools/<tool>/<version>/`, and detection appends
  `~/.chimaera/tools/bin` to the end of PATH, so the host's own engine always wins.
  Typst first: one small static binary, no bundle, instant compiles. Tectonic second:
  its bundle cache grows under the user's home, which matters on quota'd HPC homes.
- **A WASM engine in the browser** (typst.ts for Typst; SwiftLaTeX or BusyTeX for
  LaTeX): rejected as the main path. The engine, fonts and packages must be
  downloaded into every browser, through the tunnel; every source file and figure
  must cross the tunnel before each cold compile; and agents on the host could not
  use it at all, so `compile_document` would be impossible. The numbers are in
  [sources](#sources). It could make sense for a future local-only, no-host mode;
  that is out of scope here.

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
  loads the legacy `stex` mode). Typst has no CodeMirror package in the tree; a small
  stream tokenizer (headings, `#` code, `$` math, strings, comments, markup) covers
  it until a maintained grammar exists.
- Opening a document with no current build compiles it once, so the PDF side is never
  empty for long. A current build in the cache is shown straight away.

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
  that survives also keeps LaTeX warm: a warm latexmk rerun is usually one pass, a
  cold one three or four.
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
- **Only the pages you look at cross the tunnel.** `PdfView` calls
  `getDocument({ url })` with defaults today, which streams the whole file. For
  compiled documents, pass `disableAutoFetch` and `disableStream` so pdf.js fetches
  byte ranges for visible pages only; `/raw` already serves ranges. A 20 MB report
  with heavy figures then costs a few hundred KB per rebuild, not 20 MB.
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
  years of edge cases. A shared fixture corpus of real logs (pdflatex, xelatex,
  lualatex, Tectonic, biber, bibtex, Typst) runs in the Rust suite, like
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
  `Fix the 3 compile errors in @report.tex (compile_document lists them) `.
- It works for agents that do not have the chimaera MCP server too: the path, line,
  message and log location are all plain text any agent on the host can use.

## 3. Source and PDF: SyncTeX

### Where the data comes from

latexmk (`-synctex=1`) and Tectonic (`--synctex`) write `main.synctex.gz` into the
build folder. It maps typeset boxes to source file and line.

### Parse it in the browser

- A TypeScript port of the SyncTeX parser (LaTeX Workshop ships one, MIT) runs in a
  Web Worker. It loads `main.synctex.gz` through a `/raw` ticket on the first sync
  action, decompresses with the browser's own `DecompressionStream`, and is cached
  per build version.
- **Why the browser.** Sync must feel instant, and every daemon round trip costs about
  two tunnel round trips ([remote perf plan](perf-remote-plan.md), F2). Parsing a
  thesis-sized SyncTeX file would also take tens of MB of daemon memory. The `synctex`
  command-line tool is not an option either, since Tectonic-only hosts do not have it.
- **Cap.** Over 16 MB compressed, sync turns off with a note.
- **Paths.** SyncTeX records input paths as the engine saw them, absolute or relative
  to the working directory. They are normalized against the main file's folder and
  mapped to workspace paths.

### Forward: source to PDF

- **Mod+J** (a new action in the keybinding registry), a toolbar button, or
  **follow cursor** (off by default, debounced about 250 ms) takes the cursor's line
  to its boxes in the PDF.
- `PdfView` gains `showBoxes(page, boxes)`: scroll so the first box sits a third of the
  way down if it is not already visible, then draw a translucent accent highlight that
  fades after about 1.5 s. Boxes are in PDF points from the page's top left; the page
  viewport converts them, including a non-zero MediaBox origin.

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

Typst writes no SyncTeX. The mapping lives inside the compiler (the `typst-ide` crate
has click-to-source and cursor-to-position helpers, which tinymist and the Typst web
app use), and the `typst` command line does not expose it. Two options:

1. **Text matching (Phase C).** Typst prose is very close to its source. Inverse:
   take the clicked or selected PDF text (pdf.js text layer) and find it in the member
   files, ignoring markup, whitespace and hyphenation; ties go to the file the editor
   shows. Forward: take the words around the cursor and find them in the page texts.
   Headings and plain paragraphs map well; math, tables and generated text do not.
   The UI says "approximate" and never pretends otherwise.
2. **A small companion binary (later, decision).** Built from Typst's own crates, it
   would answer exact jump queries. Cost: it pins a Typst version separate from the
   host's `typst`, recompiles the document itself, and needs updating with every
   Typst release. See [open decisions](#open-decisions).

## 4. Multi-file projects

### Finding the main file

For a `.tex` file, first match wins:

1. **A magic comment** in the first 20 lines: `% !TEX root = ../main.tex` (any case,
   with or without the space after `%`). TeXShop, TeXstudio, LaTeX Workshop and
   Overleaf all honor it, so it is what agents are taught to write.
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
  at 256 paths. While the document is open, the daemon stats the set every 2 s and
  also reacts at once to recognized agent writes (the hooks already call
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
- **Tectonic** runs bibtex itself; biblatex documents need an external biber of the
  matching version, or fall back to latexmk.
- **Typst** reads `.bib` (BibLaTeX) or Hayagriva `.yml` natively with
  `#bibliography("refs.bib")`. No extra tool, no extra pass. One more reason to
  recommend Typst for new reports.
- `.bib` files open as text. Saving one recompiles its main document through the
  watch set.

## 5. Agent awareness

Every Claude and Codex session Chimaera spawns already loads the chimaera MCP server
(`mcp.rs`). Its `instructions` today cover linked terminals only. It is the one
channel that reaches every agent without touching the repository.

### A short paragraph in the MCP instructions

Appended for every tier (about 600 characters):

> Chimaera documents: the user reads your reports as PDFs beside their source. For a
> new polished report, write Typst (.typ) unless LaTeX is required (a journal
> template, an existing .tex project). After editing a report, call
> compile_document and fix every error it returns before you hand over. Call
> document_guide once for this host's engines and the conventions here (main file,
> figures, bibliography, what is not allowed).

The paragraph is static. The live facts (which engines this host has) come from
`document_guide`, because `initialize` must not trigger a login-shell detection.

### `document_guide(kind?)`

`kind` is `typst`, `latex` or `markdown`. Returns at most 8 KB of text:

- **This host**: the engines found and their versions, and which one this workspace
  will use (or why none, and how the user can fix it).
- **Structure**: one main file per report; included LaTeX files start with
  `% !TEX root = main.tex`; Typst has one `main.typ` that `#include`s the rest.
- **Build output**: never write aux files or PDFs into the repo; the build folder is
  managed; use `copy_to` when the user wants the PDF beside the source.
- **Figures**: PDF for vector plots, PNG at 300 dpi or more for rasters, relative paths,
  under `figures/`. No EPS.
- **Bibliography**: Typst `#bibliography("refs.bib")`; LaTeX biblatex with biber, or
  natbib with bibtex, one per project.
- **Not allowed here**: packages that need shell escape (minted, svg, some tikz
  externalization) unless the user enabled it; `\write18`; absolute paths; fonts that
  are not on the host (use the engine's defaults); Typst `@preview` packages without a
  pinned version, and any at all on a host without network.
- **Skeletons**: a minimal Typst report and a minimal LaTeX report that compile here.
- **The loop**: compile, fix errors, then undefined references and citations, then
  check the page count and figures.

### `compile_document(path, engine?, copy_to?, timeout_s?)`

- `path` may be any member file; the main file is found as in
  [section 4](#finding-the-main-file). Relative paths resolve against the agent's
  working directory, then the workspace root.
- **Waits for the result**, up to `timeout_s` (default 120, cap 600), joining a running
  job for the same inputs instead of starting a second one.
- **Returns text, at most 8 KB**: `status` (`ok`, `errors`, `failed`, `timeout`,
  `no_engine`, `busy`), engine and version, main file, PDF path, pages, size and
  duration; up to 20 errors as `file:line: message` plus one context line; undefined
  references and citations (up to 10); a count of box warnings; the log path.
- **Updates the user's view.** Same build folder, same events frame: an open PDF
  refreshes and the chip says which agent built it.
- **Bounded like everything else.** Same queue, same limits, at most one pending rerun
  per document, no shell-escape argument. `copy_to` must resolve inside the workspace
  and may only replace a PDF.
- **Base tier**, not Mastermind-only: every agent writes reports.
- **Log excerpts are data.** They quote the document, which may come from an untrusted
  repository; the tool text says so.

### Later: `render_page(path, page)`

Returns one page as a PNG (MCP image content) so multimodal agents can see a figure
running off the page or a table that overflows. Typst renders PNG itself
(`--format png` with a page selection); LaTeX PDFs use `pdftoppm` when the host has
it, else the tool is not offered. Caps: one page per call, about 1.5 megapixels,
1 MB.

## 6. Markdown to PDF

Agents write the portable markdown dialect (the documents plan, Phase 6). Some of it
should leave as a real report: title block, page numbers, table of contents, numbered
figures, a bibliography.

| Option | Needs on the host | Verdict |
|---|---|---|
| Browser print of the reading view | nothing | Keep for "what I see"; not typeset (no running heads, no page-aware floats). |
| pandoc to LaTeX to PDF | pandoc + TeX Live | Slow, rarely installed, plain without a custom template. |
| pandoc to Typst (`--pdf-engine=typst`) | pandoc + typst | Good output, but pandoc is rarely on HPC hosts and its templates are a second language. Offer when present, never require. |
| A Typst package that parses markdown inside Typst (`cmarker`, with `mitex` for math) | typst | Zero daemon code; but another parser with its own gaps, and alerts, frontmatter and fragment embeds need hooks. Good prototype. |
| **Chimaera writes Typst from its own markdown tree** | typst | **Recommended.** |

**The recommendation.** The documents plan keeps comrak (already in the daemon) as
the reference renderer and the export path. Add a comrak-to-Typst writer for exactly
the portable dialect:

- Frontmatter `title`, `summary`, `status`, `audience`, `updated` fill the template's
  title block and abstract.
- GitHub alerts become styled callouts; tables, footnotes, task lists and code blocks
  map one to one.
- Math `$…$` and `$$…$$` goes through `mitex` (LaTeX math inside Typst). Its package
  files ship inside Chimaera's template bundle, so export never needs the network.
- Image embeds become numbered figures with their alt text as the caption; `#page=`
  and `#xywh=` fragments map to Typst's image options where it has them.
- Mermaid is rendered to SVG by the browser at export time when a window is open;
  otherwise it stays a code block with a note.
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
| Wall time | 120 s LaTeX, 60 s Typst (setting, cap 600 s) | a timer, then SIGTERM and SIGKILL to the whole process group |
| CPU time | wall limit plus slack | `RLIMIT_CPU`, a backstop |
| Memory | 4 GB address space | `RLIMIT_AS`, a backstop for runaway macros, not a tuning knob |
| File size | 256 MB per written file | `RLIMIT_FSIZE` stops a runaway `\write` loop filling the disk |
| Build folder | 512 MB per document, 1 GB total | checked after each build, least recent evicted |
| Daemon memory | engine output goes to files, not pipes; the daemon reads a 64 KB tail and streams the log parse | no whole-log reads |
| Threads | `typst --jobs 2` | leave cores for everyone else |

Each compile runs in its own process group (so a timeout kills latexmk and every pass
it started), with stdin closed (so an error prompt can never wait for input), through
Tokio's async child handling like `compute.rs`'s capped runner, so no reactor thread
ever blocks on it.

### Security

- **Shell escape.** Unrestricted shell escape (`-shell-escape`, Tectonic's
  `-Z shell-escape`) is off and can only be turned on by the user, per project, in the
  UI; never by an agent and never by a file in the repo. TeX Live's own default,
  restricted shell escape (a short list of helper programs such as `repstopdf` and
  `kpsewhich`), stays as the site configured it. Typst has no shell escape at all.
- **Project code.** A project `latexmkrc` is Perl. It runs only after the user trusts
  it for this workspace, and the trust is tied to the file's content hash, so an edit
  (by anyone, including an agent) asks again. Until then the build uses `-norc` plus
  the user's own `~/.latexmkrc`. The environment prelude page already calls a
  checked-in prelude file a supply-chain vector; this is the same rule.
- **Writing files.** TeX Live's `openout_any = p` (its default, set explicitly in the
  compile environment) keeps TeX from writing dot files or outside the working and
  output folders. Typst writes only the output file.
- **Reading files.** TeX can `\input` any file the user can read and typeset it into
  the PDF. That is how TeX works, and the agent could read those files anyway; it only
  matters for documents from untrusted repositories that get shared. A per-workspace
  "paranoid reads" switch sets `openin_any = p`. Typst cannot read outside `--root`.
- **Network.** TeX Live engines never touch the network (only through shell escape).
  Tectonic downloads bundle files on a cache miss and Typst downloads `@preview`
  packages on first import; both are the engine fetching its own packages, allowed. An
  **offline** setting (for compute nodes) passes Tectonic's `--only-cached` and turns a
  missing Typst package into a clear diagnostic. Chimaera does not try to sandbox the
  network itself: unprivileged network namespaces are usually disabled on HPC kernels.
- **Environment hygiene.** Compiles get the captured prelude environment, minus the
  daemon's own variables and anything on `api::spawn_env_remove`. No token ever reaches
  an engine.
- **Routes.** Every new route is bearer-authed. Build PDFs and SyncTeX files are served
  through the existing short-lived `/raw` tickets. `copy_to` and **Save PDF beside
  source** are confined to the workspace.

## What changes where

### Daemon (`crates/chimaera-server/src/compile/`)

| File | What |
|---|---|
| `mod.rs` | routes, the events frame, the module's map line in the server `AGENTS.md` |
| `engines.rs` | prelude environment capture, PATH walk, versions, the ladders |
| `job.rs` | queue, coalescing, single-flight, limits, process groups, build folders and eviction |
| `root.rs` | main-file detection, `.fls` and Typst dependency lists, the watch set |
| `latexlog.rs`, `typstdiag.rs` | the parsers, with the fixture corpus |
| `md2typ.rs` | the comrak-to-Typst writer and the embedded templates (Phase E) |

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
| `files.ts` | the `document` view kind |
| `DocumentView.svelte` | source, split and PDF; toolbar, chip, problems list, empty states |
| `SplitEditPreview.svelte` | the three-state `show`, both halves always mounted |
| `PdfView.svelte` | in-place reload, ranged loading, `showBoxes`, Cmd-click and selection hooks |
| `doc/compile.svelte.ts` | per-document status store, the events frame |
| `doc/diagnostics.ts` | diagnostics to `@codemirror/lint` |
| `doc/synctex.ts`, `doc/synctex.worker.ts` | the parser and its queries |
| `doc/typstMode.ts` | the Typst stream tokenizer |
| `../shared/reference.ts` | the compile-error composer |

## Phases

### Phase A: the compile service and agents (daemon only)

Engine detection through the prelude, the job runner with every limit, build folders,
main-file detection (magic comment, `\documentclass`, picker route), both parsers with
the fixture corpus, the routes and events frame, and the MCP paragraph,
`document_guide` and `compile_document`.

Agents get value before any UI exists: they can build a report and fix it.

**Verification.** Rust tests with stand-in engines (`CHIMAERA_DOC_BINDIR`): the queue,
coalescing, single-flight, timeouts killing a whole process group, the file-size
limit, eviction, env scrubbing. Live on a real login node with `module load texlive`
and a real `typst`: an article, a thesis-shaped `\include` project with biber, an
infinite `\loop` document (the timeout), and an agent running the compile-fix loop
through MCP.

### Phase B: the document view

`DocumentView`, compile on open, save and agent writes, the in-place PDF swap with
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

The comrak-to-Typst writer, the templates, `mitex` bundled, **Export PDF** and
**Open as Typst**, pandoc as an option when present.

### Phase F: installs and extras

Managed Typst and Tectonic installs, `render_page`, and, if chosen, the Typst jump
companion.

| Phase | What | Size |
|---|---|---|
| A | Compile service, parsers, MCP tools | medium |
| B | Document view, compile loop, error marks | large |
| C | SyncTeX both ways, source references | medium |
| D | Multi-file depth | medium |
| E | Markdown to PDF through Typst | medium |
| F | Managed installs, page renders | medium |

A comes first; B needs A; C and D need B and can run in parallel. E needs only A (the
queue) and the documents plan's comrak work. F can land any time after A.

### How this fits the documents plan

- **Phase 1 there** (open a file at a line) is what the problems list and cross-file
  inverse search use. Until it lands, they open the file without the jump.
- **Phase 0 there** (buffers that outlive views) is what lets the split switch member
  files without losing an unsaved chapter.
- **Phase 4 there** (embeds): an embed of `report.typ#page=2` can show the compiled
  page, because the embed card resolves a document to its build PDF.
- **Phase 5 there** (point at anything): the region box and the reference basket work
  on compiled PDFs, with source lines attached.
- **Phase 6 there** (the dialect and `check_document`): `document_guide` extends the
  same guide rather than starting a second one.

## Open decisions

1. **Engine order on hosts with both.** Tectonic first (as asked; reproducible, no aux
   files, shell escape off) or latexmk first on hosts with a full TeX Live (matches
   what co-authors and journals use)? Recommend Tectonic first, with the automatic
   exceptions above and a one-click switch.
2. **Build folder default.** `~/.cache/chimaera/build` with a 1 GB cap (recommended),
   the runtime directory, or a scratch path?
3. **Compile on open and on agent writes.** Recommend on for both, with the per-document
   toggle. Or only on the user's own saves?
4. **Restricted shell escape.** Keep TeX Live's restricted default (recommended, since
   documents rely on `repstopdf` for EPS), or pass `-no-shell-escape` everywhere?
5. **Pre-allow the document tools** in the generated agent settings, so the
   compile-fix loop does not ask permission every time? Recommend yes for
   `document_guide` and `compile_document`, which are bounded and cannot run shell
   escape.
6. **Typst jump precision.** Text matching only, or also build the companion binary?
7. **Typst as the recommended format** for new agent reports in the MCP paragraph. A
   product stance; recommend yes.
8. **Managed installs.** Offer Typst and Tectonic installs at all? Recommend Typst yes,
   Tectonic after measuring its cache growth on a real home quota.

## Out of scope

- **A language server, completion or refactoring** for LaTeX or Typst (texlab,
  tinymist): the DESIGN.md non-goal.
- **A WASM engine in the browser**, for the reasons above.
- **Bundling TeX Live** or managing TeX packages (`tlmgr`). The host's admins and the
  user's prelude own the TeX installation.
- **Word output.** Pandoc can make `.docx` when present; designing that belongs with
  the documents plan's Office work.
- **Collaborative editing** of a report by several people at once.

## Appendix: what the code does today

- **File kinds.** `.tex` and `.typ` fall through `viewKindFor` to `text` and open in
  `CodeView` (`web-ui/src/lib/previews/files.ts:597-613`). LaTeX gets highlighting from
  `@codemirror/language-data`'s lazy `stex` legacy mode; Typst gets none.
- **Split view.** `SplitEditPreview.svelte` owns only geometry. The editor is always
  the first child; turning split off unmounts the preview half. `HtmlView.svelte` is
  the only host.
- **PDF view.** `PdfView.svelte` takes only a `path`, calls `pdfjs.getDocument({ url })`
  with default options (`:115`), keeps per-path scroll and zoom memory, caps rasters
  and text layers, and remounts on a new file version. It has no API to scroll to a
  spot, draw a highlight, or report a click position.
- **Editor hooks.** `CodeView.svelte` accepts host extensions in an `extra`
  compartment (`:77`, `:104`) and reports the live text through `onDoc`.
  `@codemirror/lint` is a dependency, used by `SettingsJson.svelte:186-187`.
- **References.** `shared/reference.ts` defines `FileSelection {path, startLine,
  endLine, text}` (`:19`) and `composeFileReference`, which produces
  `@path#Lx-Ly "excerpt" ` and never a newline (`:140`).
- **MCP.** `mcp.rs` `INSTRUCTIONS` (`:101`) covers linked terminals only; the base tools
  are `list_terminals`, `run_in_terminal` and `read_terminal` (`:389`); the Mastermind
  tier adds workspace tools. Claude gets the server through a generated
  `--mcp-config` (`agents.rs`), Codex through `-c mcp_servers.chimaera.url`
  (`launcher.rs`).
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
  (`:21-22`). `fs.rs` runs filesystem work behind an eight-permit semaphore (`:82`) and
  serves `/raw/{ticket}` with byte ranges and a 600 s ticket life.
- **Runtime directory.** `chimaera_core::runtime_dir` is `$XDG_RUNTIME_DIR/chimaera`,
  else `/tmp/chimaera-$UID`.

## Sources

Being verified against current releases; see the next revision of this section.

- Tectonic: [site](https://tectonic-typesetting.github.io/),
  [repository](https://github.com/tectonic-typesetting/tectonic).
- latexmk: [CTAN](https://ctan.org/pkg/latexmk).
- Kpathsea (`openin_any`, `openout_any`, `shell_escape`):
  [manual](https://tug.org/texinfohtml/kpathsea.html).
- Typst: [repository](https://github.com/typst/typst).
- SyncTeX: [repository](https://github.com/jlaurens/synctex).
- LaTeX Workshop (log parser, SyncTeX port):
  [repository](https://github.com/James-Yu/LaTeX-Workshop).
- tinymist: [repository](https://github.com/Myriad-Dreamin/tinymist); typst.ts:
  [repository](https://github.com/Myriad-Dreamin/typst.ts).
- pandoc: [manual](https://pandoc.org/MANUAL.html).
- cmarker: [repository](https://github.com/SabrinaJewson/cmarker.typ); mitex:
  [repository](https://github.com/mitex-rs/mitex).
- pdf.js: [repository](https://github.com/mozilla/pdf.js).
