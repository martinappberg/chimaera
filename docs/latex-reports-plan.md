# LaTeX and Typst reports: the plan

Dated 2026-09-25, revised 2026-09-26 and 2026-09-28. A plan, not a record: nothing
here has shipped. It covers compiling LaTeX and Typst documents on the host, showing
the PDF beside the source, jumping between the two, turning compile errors into
editor marks an agent can fix, showing what changed (in the source and in the PDF,
against git or against an agent's turn), teaching agents to write reports here,
turning markdown into a polished PDF and, later, Word. It was split out of the
documents plan (`docs/document-workbench-plan.md`, shipped as
martinappberg/chimaera#159; its "Out of scope" section hands this effort over) and
built from a read of the tree plus a survey of Tectonic, TeX Live, latexmk, Typst,
SyncTeX, pandoc and the tools that already do this (VS Code's LaTeX Workshop,
texlab, tinymist, Overleaf's compile limits). The 2026-09-26 revision made the
effort a set of workbench plugins. The 2026-09-28 revision re-reads the tree at
`318be45`, after the WASM plugin host shipped (martinappberg/chimaera#185 and its
fixes): it rebuilds [the plugin shape](#the-plugin-shape) on the host as it
exists, replaces the promised `exec` import with a build world, and adds
[changes](#8-changes-git-differences-in-the-source-and-the-pdf). Claims about the
current code are traced in the [appendix](#appendix-what-the-code-does-today);
outside facts are in [sources](#sources).

## The short version

- **Two WASM plugins on one build point.** LaTeX and Typst are workbench plugins in
  their own repositories (`chimaera-plugin-latex`, `chimaera-plugin-typst`), each a
  `plugin.wasm` the user installs from the Extensions tab and switches on per
  workspace ([the plugin shape](#the-plugin-shape)). The host runs the engine; the
  plugin decides what to run (`plan`) and reads what came out (`digest`), through a
  new WIT build world. The card says which programs it runs. Nothing changes for an
  agent in a workspace where neither is on.
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
- **Errors become editor marks.** The plugin parses the log into a short list of
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
- **Changes are visible where people read.** Change bars in the source against the
  last commit, a branch or the start of an agent's turn, with a word diff that
  ignores rewrapped paragraphs; the same changes marked beside the lines of the PDF;
  a before-and-after view; and for LaTeX a latexdiff changes PDF to send to
  co-authors ([section 8](#8-changes-git-differences-in-the-source-and-the-pdf)).
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
   streamed and capped, and read by the plugin inside its 64 MiB sandbox. SyncTeX
   becomes a compact source map once per build, and the browser answers jumps from it
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

This section was rewritten on 2026-09-28 against the plugin host as it shipped
(martinappberg/chimaera#185, with the fixes in martinappberg/chimaera#189,
martinappberg/chimaera#191 and martinappberg/chimaera#194). What exists
today, in one paragraph: a workbench plugin is a Rust crate in its own repository,
built for `wasm32-wasip2` into one portable `plugin.wasm` beside its `plugin.toml`,
released as a `v<version>` tag with those two files and a `SHA256SUMS`. The daemon
carries no plugin bytes; it embeds `plugins/plugins.lock` (the maintainers' list)
and installs a plugin on the user's click into `~/.chimaera/plugins/<id>/<version>/`,
from its release or from a directory. Wasmtime runs one sandboxed instance per
(plugin, workspace), one call at a time, with 64 MiB of memory, a 5 s budget per
call (30 s for `knowledge`), and WASI granting nothing: a plugin reaches only the
host's bounded imports in the WIT world `chimaera:plugin@0.1.0` (workspace-relative
`read` / `stat` / `list`, 64 KiB of `state`, `sessions`, the Timeline, `emit`,
`log`) and answers through six exports (`tools`, `instructions`, `call-tool`,
`knowledge`, `query`, `on-event`). Switched off, nothing changes for any agent,
byte for byte ([plugins](features/plugins.md), [the host's
design](plugin-system-plan.md), [writing a plugin](agent-guides/plugins.md)).

### Why a build plugin cannot simply call `exec`

The plugin plans promised WIT 0.2 would add two imports, `exec` (run a child
process) and `watch` (report file changes), and that LaTeX would call `exec` from
inside its own code. Against the shipped host that shape does not work:

- **Budgets.** A guest call gets 5 s, and a host call still waiting 2 s past the
  budget is abandoned. A LaTeX build takes up to 180 s. Lifting the budget for
  calls that wait on `exec` makes every other call to that plugin wait too.
- **One call at a time.** An instance serves one call at a time. A plugin blocked
  in a three-minute build could not answer the guide, a sync lookup or an agent's
  `compile_document` from another session until the build ended; the agent's
  `compile_document` would even deadlock if it asked the host to build and the host
  needed the same instance to plan.
- **Review.** An argv computed inside a component is invisible until it runs. The
  programs a plugin may start should be on its card before anyone switches it on.

So the host runs the build, and the plugin decides and reads. No `exec`, no `watch`:

```
save / agent write / Build / compile_document
  └─ host: debounce, resolve the declared programs on the prelude PATH
       └─ plugin build.plan(request)      5 s   → program, args, cwd, env, output, or why not
            └─ host: validate, consent checks, queue, run under every limit (section 7)
                 └─ plugin build.digest(outcome)   30 s   reads the outputs through the host
                      → diagnostics, inputs, output, source map, a line for agents
                           └─ host: store it, watch the inputs, send {"type":"doc"} on /ws/events
                                └─ UI: DocumentView, marks, PDF swap, sync, changes
```

Each plugin call is short and ordinary. The instance is free while the engine
runs. The host watches the inputs the digest names (a stat sweep plus the agent
write events it already gets), so `watch` is not needed either.

### WIT 0.2: a build world

Adding an export to the one world would break every 0.1 plugin, so build plugins
get their own world. The host serves both: a 0.1 plugin keeps working unchanged.

```wit
package chimaera:plugin@0.2.0;

// types, host and plugin: exactly as in 0.1.

/// The outputs of the build a `digest` call is about. Names are relative to
/// that build's folder; the host serves only the build in flight.
interface build-host {
    use types.{context, entry};
    output-read: func(cx: context, name: string, cap: u32) -> result<list<u8>, string>;
    output-list: func(cx: context, cap: u32) -> result<list<entry>, string>;
}

/// What a build plugin adds to the `plugin` exports.
interface build {
    use types.{context, json};
    /// How to build this file: the main file, the program and its arguments,
    /// or why not. 5 s.
    plan: func(cx: context, request: json) -> result<json, string>;
    /// What happened: diagnostics, inputs, output, a source map. 30 s.
    digest: func(cx: context, outcome: json) -> result<json, string>;
    /// The conventions document_guide returns where this plugin is on. 5 s.
    guide: func(cx: context, facts: json) -> string;
}

world build-plugin {
    import host;
    import build-host;
    export plugin;
    export build;
}
```

A **plan** request and answer (JSON, so fields can be added later):

```json
{ "path": "chapters/intro.tex", "reason": "save", "tree": "workspace",
  "programs": { "latexmk": { "version": "4.88" }, "lualatex": { "version": "1.18.0" }, "tectonic": null },
  "choice": "auto", "offline": false, "shell_escape": "restricted",
  "previous": { "main": "thesis.tex", "inputs": ["thesis.tex", "chapters/intro.tex", "refs.bib"] } }
```

```json
{ "main": "thesis.tex", "label": "latexmk · pdflatex",
  "program": "latexmk",
  "args": ["-pdf", "-interaction=nonstopmode", "-file-line-error", "-synctex=1",
           "-recorder", "-norc", "-outdir={build}", "thesis.tex"],
  "cwd": ".", "env": { "max_print_line": "10000", "TEXMFOUTPUT": "{build}" },
  "output": "thesis.pdf", "wall_s": 180, "runs_code_from": [] }
```

Or `{"refuse": {"why": …, "fix": …}}` (no engine, old LuaTeX on an automatic
build), or `{"choose_main": ["a.tex", "b.tex"]}` (the picker of section 4). A
**digest** gets the plan back with `exit`, `timed_out`, `duration_ms` and the list
of files the run left, and answers:

```json
{ "status": "errors", "output": "thesis.pdf", "pages": 42,
  "diagnostics": [ { "severity": "error", "file": "chapters/intro.tex", "line": 12,
                     "message": "Undefined control sequence \\unit", "context": "l.12 ...\\unit{mg}",
                     "log": "thesis.log", "log_line": 345 } ],
  "counts": { "errors": 1, "warnings": 14, "boxes": 22 },
  "inputs": ["thesis.tex", "chapters/intro.tex", "refs.bib", "figures/umap.pdf"],
  "sourcemap": { "format": "chimaera-sourcemap/1", "files": ["thesis.tex", "chapters/intro.tex"],
                 "boxes": [[1, 12, 3, 72.0, 118.4, 451.3, 11.9]] },
  "for_agents": "1 error: chapters/intro.tex:12 Undefined control sequence \\unit (is siunitx loaded?)" }
```

The shapes are core's: the host and the UI know `diagnostics`, `inputs` and
`chimaera-sourcemap/1` (per source line, the boxes it produced on each page, in PDF
points from the top left), never LaTeX. Every path the plugin sees or returns is
relative to the workspace (or to the base tree of [section 8](#building-the-base));
the host substitutes `{build}` for the build folder, so a plugin never handles an
absolute host path it could misuse.

**What the host checks before it runs a plan.** The program is one the manifest
declares, resolved by the host on the prelude PATH (a plugin never names a path);
arguments are a list, never a shell string; `cwd` stays inside the workspace or
the base tree; `env` may not touch the variables the host sets (limits, `PATH`,
`HOME`, the prelude's); `wall_s` is clipped to the host's cap; a file named in
`runs_code_from` (a project `latexmkrc`) must carry the user's trust for its
current content hash, else the build waits with a **Trust** button (the codex
hook-trust pattern); and an argument that turns on unrestricted shell escape
(`-shell-escape`, `--shell-escape`, `-Z shell-escape`) is refused unless the user
enabled it for this workspace. The last check is a guard for honest plugins, not a
sandbox: a program's own arguments can still run code, which is why the programs
are on the card (below).

**Budgets and caps.** `plan` and `guide` get the ordinary 5 s, `digest` the 30 s
`knowledge` already has (a thesis-sized SyncTeX file is several MB). `output-read`
stops at 8 MiB like `read`. A digest answer may be up to 4 MiB (the source map);
the host writes the source map into the build folder and the UI fetches it once per
build through a `/raw` ticket. Over the cap, sync is off for that build and the
chip says so.

### The manifest

```toml
id = "latex"
name = "LaTeX"
version = "0.1.0"
api = "0.2"
summary = "Build .tex to PDF beside the source, with errors as editor marks and jumps both ways."
description = "Compiles with the TeX Live your terminals get (module load texlive, through Settings → Environment), else Tectonic. Build files go to a cache folder, never into the repository."
homepage = "https://github.com/martinappberg/chimaera-plugin-latex"

[build]
sources = ["*.tex", "*.ltx"]      # opens as a document where this plugin is on
programs = ["latexmk", "pdflatex", "xelatex", "lualatex", "tectonic", "latexdiff"]
versions = { latexmk = ["-v"], tectonic = ["--version"] }   # default ["--version"]
sync = "sourcemap"                # the digest returns one; "text" = core's text matching
debounce_ms = 800
wall_s = 180

[recommends]
summary = "A report-writing skill for claude and codex: main files, figures, bibliography, and the compile-and-fix loop, for agents run outside Chimaera too."

[recommends.agent_plugins.claude]
id = "report-writing@chimaera-plugin-latex"
marketplace = "martinappberg/chimaera-plugin-latex"

[provides]
events = []

[adds]
ui = [".tex opens as source | split | PDF · compile on save · errors as editor marks · Cmd-click the PDF to jump to the line · changes since the last commit, marked in the PDF"]
agents = ["compile_document for every agent here · document_guide learns LaTeX and this host's engines"]

[release]
github = "martinappberg/chimaera-plugin-latex"
```

Typst is the same with `sources = ["*.typ"]`, `programs = ["typst"]`,
`sync = "text"`, `debounce_ms = 300` and `wall_s = 60`. `api = "0.2"` is the gate
that makes an older daemon list the plugin, off, with "needs a newer chimaera",
instead of choking on `[build]` (the manifest is parsed with `deny_unknown_fields`).

### Who owns what

| Core (this repository, shared by every build plugin) | The plugin (its own repository, WASM) | The manifest (data, on the card) |
|---|---|---|
| detection through the prelude; the queue and every limit; build folders and eviction; plan checks and consent; the base trees of section 8; storing digests; the events frame | the ladder and its exceptions (magic comments, `Tectonic.toml`, the old-LuaTeX gate); finding the main file; the log parser and its fixture corpus; `.fls` or Typst deps to inputs; SyncTeX to the source map; the guide text; the latexdiff plan | the files it claims; the programs it may run; debounce and wall time; sync kind; the Adds lines; the recommended skill pack |
| `DocumentView`, marks, problems list, **Ask agent**, the PDF swap, sync lookups from a source map, text matching, change bars and PDF change marks | nothing in the UI: a plugin contributes no UI code | |
| `compile_document` (the point's tool) and the `document_guide` extension | the words both return (`for_agents`, `guide`) | |

This keeps the host's rule (no plugin behaviour in the daemon) and its opposite
(no careful login-node code in a plugin): LaTeX lives only in the plugin, and
processes, limits and pixels live only in core.

### What "on" means for a build plugin

- **On is active.** No `detect` footprint, like Agent notes: an agent should get the
  engine facts before it writes the first `.tex`, and a glob would need a walk. The
  card's **Here** line reports what the quick-open index already knows when warm
  ("12 .tex files · latexmk 4.88 on sherlock") and never starts a walk.
- **File kinds follow the switch.** `viewKindFor` consults the active plugins'
  `build.sources` (the plugin store's `workspacePlugins`): `.tex` opens as a
  `document` only where a build plugin claims it, and as text everywhere else,
  exactly as today. Switching the plugin off returns every `.tex` pane to the plain
  editor.
- **The agents' view changes only where it is on.** `compile_document` is served by
  core, but offered and call-gated only in workspaces with an active build plugin,
  and pre-allowed at spawn there through `plugins::spawn_allow`. It counts as a
  plugin tool for the codex TUI rule (which gets the chimaera MCP server only while
  a plugin with tools is active). `document_guide` keeps its definition byte for
  byte where no build plugin is on; where one is, its `kind` gains `latex` or
  `typst`, answered by the plugin's `guide`. The `agent_view` fixtures stay
  unchanged with every build plugin off.
- **Why `compile_document` is core's and not the plugin's.** A plugin tool that
  builds would hold the plugin's only instance for minutes and deadlock when the
  host needs that instance to plan. Served by core, it runs the same pipeline as a
  save, joins a build already running, and formats the digest (`for_agents`, the
  diagnostics) into its answer. One tool, one schema, for LaTeX, Typst, markdown to
  PDF and Word alike.

### The card, and consent to run programs

A build plugin is the first plugin that runs programs on the host, so its card and
its preview say so before anything is installed:

- **A "Runs" line** under For you and For agents, from `[build] programs`: "Runs
  latexmk, pdflatex, xelatex, lualatex, tectonic or latexdiff on this host when a
  document builds." The preview (**Install from a repository** → **Preview**) shows
  it too.
- **First-party** (in the lock, the check badge): the switch works as for any plugin.
- **Third-party:** switching one on asks once per workspace: "<name> runs latexmk
  on this host with your permissions whenever a document builds. Switch it on only
  if you trust github.com/<repo>." The sandbox bounds what the component does; it
  cannot bound what `latexmk` does with the arguments the component chose, and the
  dialog says exactly that.

### What it installs

Nothing silently, and by default nothing at all:

| Where | What | How |
|---|---|---|
| Chimaera | the build point, the runner and the document view ship in the daemon; the LaTeX logic is `chimaera-plugin-latex`'s `plugin.wasm`, installed on the user's click from its release (the version `plugins/plugins.lock` pins) | **Install** on the Extensions card; `chimaera plugin add latex` on the host |
| the host toolchain | nothing by default; whatever the prelude provides (`module load texlive`, a `typst` in `~/.local/bin`) | detection through the prelude (section 1) |
| managed tools (later) | `typst`, `tectonic`, later `pandoc`: official release artifacts, checksums, a visible shell session, never sudo, under `~/.chimaera/tools/<tool>/<version>/` | the `runtimes.rs` pattern; **Install** on the empty PDF state |
| the agents | nothing required. Recommended: the `report-writing` skill pack, the guide as an Agent Skill for agents that run outside Chimaera, from the plugin repository's `.claude-plugin/` and `.codex-plugin/` | the shipped **Agent-side plugin** box: the agent's own CLI in a visible terminal, never needed for the plugin to work |
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
- **Managed install** (Phase G): the curated-installer pattern in `runtimes.rs`
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

- **The parser is the LaTeX plugin's `digest`** (it also feeds `compile_document`),
  in Rust compiled to WASM, reading the log through `output-read` in 8 MiB windows
  up to a 16 MB scan cap. It follows the TeX file
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
  real logs (pdflatex, xelatex, lualatex, Tectonic, biber, bibtex; Typst's in its own
  plugin) runs as plain native `cargo test` in the plugin's repository, where the
  parser is a function of bytes and never calls the host.
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

**Under the plugin model** SyncTeX is the plugin's to read and core's to show. The
LaTeX plugin's `digest` reduces the build's SyncTeX file to a generic source map;
the browser loads that map once per build and answers every jump locally, so a
click never waits on the tunnel and no LaTeX code runs in the UI.

### Where the data comes from

latexmk (`-synctex=1`) and Tectonic (`--synctex`) write `main.synctex.gz` into the
build folder. It maps typeset boxes to source file and line.

### A source map per build

- **The digest reduces SyncTeX.** It streams `main.synctex.gz` through
  `output-read`, decompresses as it goes (never the whole text in memory: a thesis's
  SyncTeX can be tens of MB uncompressed and an instance has 64 MiB), and keeps, per
  source line, the union of the boxes it produced on each page: the
  `chimaera-sourcemap/1` shape of [the plugin shape](#wit-02-a-build-world). LaTeX
  Workshop's `synctexjs.ts` (MIT, a port of synctex-js) is the reference for the
  format; LaTeX Workshop already relies on its own parser rather than the `synctex`
  binary for inverse search, because the binary mishandles some non-ASCII paths.
- **The browser queries it.** `doc/sourcemap.ts` fetches the map through a `/raw`
  ticket (cached by `ETag`, so an unchanged build costs a 304) and answers forward
  and inverse lookups in memory. The same map feeds the change marks of
  [section 8](#8-changes-git-differences-in-the-source-and-the-pdf).
- **Why not in the daemon's core, or per click.** A per-click plugin query costs about
  two tunnel round trips ([remote perf plan](perf-remote-plan.md), F2), and a LaTeX
  parser in core is what the plugin model avoids. The `synctex` command-line tool is
  not an option either, since Tectonic-only hosts do not have it.
- **Cap.** A map over 4 MiB is dropped: sync is off for that build, with a note.
- **Units and paths.** The file stores scaled points plus a unit and offsets from its
  preamble; there are 65,781.76 scaled points to a PDF point, and SyncTeX measures
  from the page's top left while PDF measures from the bottom left. Input paths are as
  the engine saw them (Tectonic writes absolute paths since 0.8.1); the digest maps
  them to workspace paths using the roots the host passes in the outcome.

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

The plugin's `plan` decides, reading files through the host; core only remembers
and asks. For a `.tex` file, first match wins:

1. **A magic comment** in the first 20 lines: `% !TEX root = ../main.tex` (any case,
   with or without the space after `%`). TeXShop, TeXstudio and LaTeX Workshop all
   honor it, so it is what agents are taught to write.
2. **The file has `\documentclass`** before `\begin{document}`: it is a main file. The
   `subfiles` class names its main file in `\documentclass[../main.tex]{subfiles}`;
   the main is built by default, with **build this part alone** as an option.
3. **The last build said so.** latexmk's `-recorder` writes a `.fls` list of every
   file the build read, and the digest turns it into `inputs`. The host keeps a
   small in-memory map from file to main, filled from recent digests (capped at 512
   entries per workspace), and passes the match to `plan` as `previous`.
4. **A bounded search.** `.tex` files with `\documentclass` in the file's folder and up
   to two parents (at most 200 files, first 8 KB each, through the host's `list` and
   `read`, which already run off the reactor behind the filesystem semaphore) whose
   `\input`, `\include`, `\subfile` or `\import` lines name this file. Exactly one
   match wins.
5. **Ask once.** Several candidates, or none: the plan answers `choose_main`, and a
   small picker appears in the toolbar. The answer is remembered per workspace in a
   small capped JSON file under `~/.chimaera` (core's, not the plugin's 64 KiB state,
   which a restart clears).
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
([what "on" means](#what-on-means-for-a-build-plugin)): the plugin's `instructions`
paragraph through the existing seam (`plugins/tools.rs`), the `document_guide`
extension answered by the plugin's `guide` export, and `compile_document`, which
core serves for every build plugin.

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
guide unchanged; `latex` and `typst` (offered only where that plugin is on) return
the plugin's `guide`, given this host's facts, at most 8 KB of text:

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
  reports. Served by core, not by the plugin
  ([why](#what-on-means-for-a-build-plugin)); the plugin supplies the words
  (`for_agents`) and the diagnostics.
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

**The recommendation.** A third plugin on the same point,
`chimaera-plugin-md-pdf`, with comrak (the parser the daemon's reading fallback
already uses, pinned to the same version) compiled into its component, writing
Typst for exactly the portable dialect. Its `plan` reads the markdown through the
host, writes the Typst, and hands it to the engine on standard input (a `stdin`
field in the plan, capped at 4 MiB; `typst compile -` reads it), so the plugin never
writes a file and nothing lands beside the markdown:

- Frontmatter `title`, `summary`, `status`, `audience`, `updated` fill the template's
  title block and abstract.
- GitHub alerts become styled callouts; tables, footnotes, task lists and code blocks
  map one to one.
- Math `$…$` and `$$…$$` is converted to Typst math inside the plugin by mitex's
  converter, which is Rust (the `mitex` Typst package wraps the same code as a
  185 KB WASM plugin), so export needs no Typst package and never the network (crate
  and licence to confirm).
- Every piece of text is escaped on the way out, so nothing in a markdown file can
  inject Typst code into the template.
- Image embeds become numbered figures with their alt text as the caption; `#page=`
  and `#xywh=` fragments map to Typst's image options where it has them.
- Mermaid is rendered to SVG by the browser at export time when a window is open;
  otherwise it stays a code block with a note. (pandoc's route needs `mmdc`, a
  headless Chromium; Typst-native Mermaid plugins exist but are not yet evaluated.)
- Two or three curated templates (report, memo, article) ship inside the plugin as a
  few KB of Typst. Frontmatter `template: path/to/mine.typ` picks a project template.
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
| The plugin's own work | `plan` and `guide` 5 s, `digest` 30 s; 64 MiB per instance; outputs read 8 MiB at a time; a digest answer ≤ 4 MiB | the plugin host's existing limits, plus `build-host`'s |

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
- **A build plugin runs programs.** The WASM sandbox bounds the component, not the
  engine it asks for. So the programs are declared in the manifest and shown on the
  card before install, the host resolves them itself and checks every plan
  ([the checks](#wit-02-a-build-world)), and a third-party build plugin needs a
  one-time consent per workspace ([the card](#the-card-and-consent-to-run-programs)).
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

## 8. Changes: git differences in the source and the PDF

Reports change in two places at once: the source an agent edits and the PDF a
person reads. The questions people ask are about both: "what did the agent just
change in my report?", "what changed since the last commit, or on this branch?",
and "can I send my co-author the changes?". Today the answer is the source-control
panel's side-by-side diff of the `.tex` (`DiffView`, from `GET /git/diff`), which
shows a rewrapped paragraph as entirely new and says nothing about the PDF. A
committed `report.pdf` shows as "binary".

### Five views, cheapest first

| View | What you see | Needs | Owner |
|---|---|---|---|
| 1. Change bars in the source | a gutter bar per changed line (added, modified, a wedge for deleted), changed words tinted inside the line | the base text of each open file | core, any text file |
| 2. Change marks on the PDF | a margin bar beside each changed passage on the page, the changed words tinted, a small mark where text was deleted; ‹ › steps through them | 1 plus the build's source map (LaTeX) or text matching (Typst) | core |
| 3. Before and after | the base build and the current build side by side, page by page, with the changed words marked on both | a build of the base (below) | core viewer, the plugin plans the build |
| 4. A changes PDF | one PDF with insertions underlined in blue and deletions struck in red, to download and send | a base tree and `latexdiff` | the LaTeX plugin |
| 5. The source diff | today's `DiffView`, with a prose mode that diffs words and ignores rewrapping | nothing new | core |

**1. Change bars in the source.** The document's editor gets a **Changes** toggle.
For code it runs CodeMirror's `unifiedMergeView` (already in the tree through
`@codemirror/merge`, which `DiffView` uses) with the base as the original, its
gutter on, inline highlights on, and no accept or reject controls: git stays
read-only here ([git](features/git.md)). Prose needs one thing code does not:
agents rewrap paragraphs, and a line diff then marks the whole paragraph. So for
`.tex`, `.typ` and `.md` the diff is ours (`doc/changes.ts`): words compared within
a paragraph (split on blank lines), ignoring where the line breaks fall, mapped
back to lines and drawn as the same kind of gutter bars and word marks through
`CodeView`'s `extra` compartment, with each deleted passage a one-line wedge that
expands on click. The same prose mode becomes a toggle in `DiffView`. The base text
comes from the existing `GET /git/diff` (two blobs, each capped at 2 MB), fetched
once per file and base and refetched on the git epoch. Colors are the existing
`--git-added`, `--git-modified` and `--git-deleted` tokens. Keys step to the next and
previous change. Hovering a change offers **Copy old text** and **Ask agent**
(below).

**2. Change marks on the PDF.** No extra build. For each changed source range from
view 1, the build's source map gives the boxes it produced; the PDF draws a margin
bar beside them (the change color, 3 px, outside the text block) and tints the
changed words where the text layer can place them (`pdfFind.ts`'s `pageText` and
`itemRanges` already map text offsets to text-layer items for find). Deleted text
has no box in the new PDF: a small mark sits in the margin at the nearest
surviving line, and hovering it shows the removed words. The page indicator gains
"3 changed pages" and ‹ › buttons. For Typst, or a LaTeX build without a source
map, the same marks come from text matching: the changed words of view 1 are found
in the page texts, the way find works today. Marks follow the last good build; when
the source moved on since, they map through the editor's changes like the sync of
[section 3](#3-source-and-pdf-synctex).

**3. Before and after.** For review before a commit or a merge: the base build on
the left, the current build on the right, scrolled together by page. Each page pair
is diffed by text in the browser (pdf.js text content of both pages, a word diff,
computed only for pages near the viewport), and the changed words are tinted on both
sides. A page-image mode (swipe and onion skin over one page, the image compare the
documents plan describes) helps for figures and layout, where text says nothing.
This view needs the base built, once, on demand.

**4. A changes PDF (LaTeX).** The classic for co-authors: `latexdiff` (part of a
full TeX Live) writes a marked-up `.tex` from the base and current sources, and the
same pipeline compiles it into `thesis-changes.pdf` in the build folder, offered
through **Download** and **Save beside source**. The LaTeX plugin plans it as two
steps, `latexdiff --flatten base/thesis.tex thesis.tex` (its output captured to the
build folder) and then latexmk on that file, with `--flatten` so `\input` and
`\include` are followed and the markup options set conservatively for math and
graphics. latexdiff stumbles on some tables and custom macros; when it fails, the
chip says so in plain words and offers view 3. Typst has no latexdiff, and view 3
is its answer; a Typst plugin writing a marked-up `.typ` itself is a later idea.

**5. The source diff** stays `DiffView`, gaining the prose toggle from view 1.

### Choosing the base

The **Changes** toggle carries a base picker, remembered per document:

- **Last commit** (HEAD). The default when the document is in a git repository.
- **Staged** (the index), for "what am I about to commit".
- **A branch**: the merge base with the branch named (default: the repository's
  default branch), for "what this branch changes", the pull-request view.
- **A commit** picked from a short list of the document's recent commits.
- **Before this turn**: the build that was current when an agent's turn started. No
  git needed, so it also works for a report nobody has committed yet.
("Since I last looked", a base the browser remembers, needs the host to keep more
than one old build; it waits until the two-generation `prev/` proves too little.)

A branch and a commit need two small git additions in core, both through the
existing bounded git runner (HEAD and the index are served today): `GET /git/diff` accepts `rev=` beside its three modes (a ref
checked with `check-ref-format` and resolved with `rev-parse --verify` before use),
and `GET /git/log?path=&limit=` lists a file's recent commits (capped at 50).

### Building the base

Views 3 and 4 need the document as it was at the base. The host, not the plugin,
makes that tree, because it involves git and the filesystem:

- **From the last digest's inputs** (the `.fls` or Typst deps, inside the
  workspace), the host writes each tracked input at the base revision into
  `<build>/base/<rev>/tree/` with `git show <rev>:<path>`, bounded: at most 512 files
  and 64 MB. An input git does not track (a generated figure, say) is copied from
  the working tree and marked "not in git; the current version was used". A file
  that did not exist at the base is simply absent, as it was.
- **The plugin plans against that tree** (`"tree": "base"` in the request), so the
  ladder and the main-file rules are the same. The build runs in the same queue,
  behind user and agent builds, and is cached by (revision, inputs): switching back
  to a base you already built is instant. Its folder counts against the document's
  512 MB and is evicted with it.
- **Before this turn** needs no tree: when a build lands, the host keeps the
  previous good build's PDF, source map and a snapshot of its source inputs (up to
  8 MB) as `prev/`, one generation. A Timeline `episode` (one per agent turn)
  records which build was current when the turn started, so "what this turn changed"
  is a diff between that build and the current one.

### Where changes show up outside the document

- **The source-control panel.** A changed `.tex` or `.typ` row gets a second action,
  **Changes in PDF**, which opens the document with Changes on and the row's mode as
  the base (unstaged against the index, staged against HEAD).
- **Timeline episodes and the chat's turn-end block.** When a turn wrote a file of a
  document that has a build, the turn gets one chip, "thesis.pdf · 3 pages changed",
  which opens the document with **Before this turn** as the base.
- **A committed PDF.** When `report.pdf` itself is in git, the diff surface stops
  saying "binary" and opens view 3 between `HEAD:report.pdf` and the working copy.
  That needs a `/raw` ticket for a git blob (`POST /fs/ticket {path, rev}`, the blob
  streamed through the bounded git runner, capped), which gives committed images the
  documents plan's compare view as well.
- **Pointing at a change.** Selecting text inside a change mark makes the usual
  `@thesis.tex#L120-L128 "…"` reference. The hover card's **Ask agent** types one
  line with both sides, capped like every quote:
  `@thesis.tex#L120-L128 changed since HEAD: "the effect held" → "the effect held in 3 of 4 cohorts" `.
- **Agents.** `compile_document`'s answer adds one line when the build changed the
  PDF: "pages changed since the previous build: 3, 7, 12" (from the source map and
  the `prev/` snapshot). Agents that want the text diff run `git diff` themselves.

### Costs

- Views 1 and 2 cost one `/git/diff` per open file and base (at most two 2 MB blobs),
  a word diff in the browser (above 20,000 lines it falls back to lines), and no
  build. Nothing polls: the git epoch refetches.
- Views 3 and 4 cost one extra build each, only when asked for, at low priority, and
  cached.
- Every git call runs through the existing runner: a timeout that kills the child,
  output and entry caps, a concurrency permit.

## What changes where

Three repositories. Nothing LaTeX-shaped lands in this one.

### This repository: the build point (core)

| Where | What |
|---|---|
| `crates/chimaera-plugin-api/wit/` | `chimaera:plugin@0.2.0`: the `build-host` and `build` interfaces and the `build-plugin` world; the Rust bindings and a `BuildPlugin` trait beside `Plugin`, with native stubs so a plugin's plan and digest logic tests with plain `cargo test` |
| `crates/chimaera-server/src/plugins/` | `[build]` in `Manifest` (`deny_unknown_fields`, the `api = "0.2"` gate); the runtime binding both worlds; `build-host` in `hostfns.rs`, bounded like `read`; the Runs line and the third-party consent on the wire (`manifest_json`) |
| `crates/chimaera-server/src/build/` | `engines.rs` (prelude environment capture, the PATH walk for declared programs, versions), `plan.rs` (the checks before a run, trust records, the shell-escape guard), `job.rs` (queue, coalescing, single-flight, limits, process groups), `folders.rs` (build folders, eviction, `prev/`, base trees), `mod.rs` (routes, the `doc` events frame, `compile_document`, the `document_guide` extension, the input watch) |
| `crates/chimaera-server/src/git/` | `rev=` on the diff route, `GET /git/log?path=`, blob tickets for `/raw` |
| `plugins/test-fixture` | a second fixture, a stand-in build plugin whose plans run stand-in engines from `CHIMAERA_DOC_BINDIR`, so the whole pipeline runs in CI without TeX |
| `web-ui/src/lib/previews/` | `files.ts` (the `document` kind from active plugins' `build.sources`); `DocumentView.svelte`; `SplitEditPreview.svelte` (three-state `show`); `PdfView.svelte` (in-place reload, boxes, Cmd-click, change marks, the compare layout); `doc/compile.svelte.ts`, `doc/diagnostics.ts`, `doc/sourcemap.ts` (lookups both ways), `doc/textSync.ts` (text matching), `doc/changes.ts` (bases, the prose word diff, mapping to PDF marks), `doc/typstLang.ts` |
| `web-ui/src/lib/plugins/` | the card's Runs line and the consent dialog |
| `web-ui/src/lib/shared/reference.ts` | the compile-error and change composers |
| docs | this plan; the plugin guide's LaTeX section and the WIT description in the plugin system plan updated to the build world; the feature pages when each phase ships |

New routes, all bearer-authed and additive: `GET /api/v1/doc/engines`
(`?refresh=true` re-detects), `POST /api/v1/doc/compile {path, reason}` →
`202 {main, version}`, `GET /api/v1/doc/status?path=` (engine, state, output,
source map ticket, counts, diagnostics), `PUT /api/v1/doc/main {path, main}`,
`POST /api/v1/doc/trust {path, file, hash}`, `POST /api/v1/doc/base {path, base}`
(build the base, section 8), `POST /api/v1/doc/export {path, template?}` (markdown to
PDF); `GET /api/v1/git/log`; `rev=` on `GET /api/v1/git/diff`; `rev` on
`POST /api/v1/fs/ticket`; the `/ws/events` frame `{"type":"doc", …}`.

### `chimaera-plugin-latex` (its own repository)

`src/plan.rs` (the ladder, magic comments, `Tectonic.toml`, the old-LuaTeX gate,
`latexmkrc` as `runs_code_from`, the latexdiff steps), `src/main_file.rs` (section 4,
reading through the host), `src/log.rs` (the parser, with the fixture corpus under
`tests/logs/`), `src/fls.rs` (inputs), `src/synctex.rs` (streaming decompress and
reduce to the source map, within 64 MiB), `src/guide.rs`, `plugin.toml`, and the
agent-side `report-writing` skill in `.claude-plugin/` and `.codex-plugin/`. CI builds
`plugin.wasm` and runs the native tests; a `v<version>` tag publishes the three
assets.

### `chimaera-plugin-typst` (its own repository)

`src/plan.rs` (the root, `--deps`), `src/diag.rs` (the short diagnostic format),
`src/deps.rs`, `src/guide.rs`, `plugin.toml`, the same skill pack for Typst.

Later, on the same point: `chimaera-plugin-md-pdf` (markdown to PDF, section 6) and
`chimaera-plugin-docx` (Word through pandoc).

## Phases

### Phase A: the build point, and agents first

In this repository: WIT 0.2's build world, `[build]` in the manifest, detection
through the prelude, the plan checks, the runner with every limit, build folders,
the `doc` routes and events frame, `compile_document` and the `document_guide`
extension, the card's Runs line and the consent, and the stand-in build fixture. In
`chimaera-plugin-latex` and `chimaera-plugin-typst`: plan, digest (without the source
map), guide, the log corpus. Both repositories get `plugins/plugins.lock` entries once
their first release is out; until then they run from local builds
(`chimaera plugin add --path`).

Agents get value before any UI exists: switched on, a workspace's agents can build a
report and fix it.

**Verification.** Rust tests against the stand-in plugin: plans refused for an
undeclared program, a `cwd` outside the tree, a host-owned `env` key, an untrusted
`latexmkrc`, a shell-escape argument; the queue, coalescing, single-flight,
timeouts killing a whole process group, the file-size limit, eviction, env
scrubbing; a 0.1 plugin still loading beside 0.2 ones; the `agent_view` fixtures
unchanged with every build plugin off; `compile_document` offered, pre-allowed and
callable only where one is on. The plugins' own native tests cover the ladder, the
main-file rules and the log corpus. Live on a real login node with
`module load texlive` and a real `typst`: an article, a thesis-shaped `\include`
project with biber, an infinite-loop document, a Typst document that allocates
without bound, and an agent running the compile-fix loop through MCP. Record what
the research could not find: prelude capture time, Typst time and memory for a
20-page report, Tectonic's memory and cache growth.

### Phase B: the document view

`DocumentView`, compile on open, on save and on agent writes to the open file or its
main file, the in-place PDF swap, error marks, the problems list, **Ask agent**, the
status chip, empty states, **Save PDF beside source**.

**Verification.** Driven live in the isolated preview on Chromium and WebKit, against a
remote daemon over a real tunnel: type, save, watch the PDF swap without a flash; let
an agent edit a chapter and watch the PDF follow; break a macro and send the error to
the agent. A `scripts/perf/` scenario measures bytes per rebuild of a 20 MB report.

### Phase C: jumps and references

The LaTeX digest's source map, lookups both ways in the browser, follow cursor,
selections to `@file.tex#Lx-Ly`, text matching for Typst.

### Phase D: multi-file depth

The watch set from the digest's inputs, remembered main files, the bibliography
messages, switching the split's editor between member files.

### Phase E: changes

Views 1 and 2 of [section 8](#8-changes-git-differences-in-the-source-and-the-pdf)
first (change bars, the prose word diff, PDF change marks; no extra build), with
**Before this turn** and the chips on Timeline episodes and the turn-end block. Then
base trees, view 3 (before and after) and the git additions (`rev=`, the file log,
blob tickets, so a committed PDF compares too). Then view 4, the latexdiff changes PDF.

**Verification.** Live: commit a chapter, let an agent rewrite a paragraph and
rewrap it, and check that the bars mark the changed words and not the whole
paragraph, that the PDF marks sit beside the right lines, and that the turn chip opens
the right comparison; review a branch against `main`; download a changes PDF.

### Phase F: markdown to PDF

`chimaera-plugin-md-pdf`: the comrak-to-Typst writer and the templates inside the
plugin, **Export PDF** and **Open as Typst**.

### Phase G: installs and extras

Managed Typst and Tectonic installs, `render_page`, and, if chosen, the Typst jump
companion.

### Phase H: Word and other outputs

`chimaera-plugin-docx` (pandoc when present, later a managed install), **Export to
Word** on markdown documents, **Open as markdown** for a `.docx`, and later a Marp
deck to `.pptx` on the same point. See
[Word](#word-and-editing-what-is-not-markdown).

| Phase | What | Size |
|---|---|---|
| A | The build world and point, the two plugins' plan and digest, MCP | large |
| B | Document view, compile loop, error marks | large |
| C | Source maps, jumps both ways, source references | medium |
| D | Multi-file depth | medium |
| E | Changes in the source and the PDF | medium |
| F | Markdown to PDF | medium |
| G | Managed installs, page renders | medium |
| H | Word export and import | small |

A comes first; B needs A; C and D need B and can run in parallel. E's first half needs
C (the source map), its second half only B. F needs only A. G can land any time after
A. H needs A and B.

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

Decided and shipped: a plugin is a Rust crate in its own repository
([plugin system plan](plugin-system-plan.md#decisions-maintainer-2026-09-26-and-2026-09-27)),
released as `plugin.wasm`, `plugin.toml` and `SHA256SUMS` under a `v<version>` tag,
with its agent-side pieces in the same repository's `.claude-plugin/` and
`.codex-plugin/`. The daemon carries the lock, never the bytes. So the LaTeX and Typst
plugins start life as `chimaera-plugin-latex` and `chimaera-plugin-typst`, run from
local builds while they are written, and join `plugins/plugins.lock` with their first
release, which is when the Extensions tab starts offering them. Third-party build
plugins install the same way, with the extra consent above.

## Open decisions

1. **Engine order on hosts with both.** The request proposed Tectonic first. The plan
   recommends latexmk first and Tectonic as the fallback where there is no TeX Live,
   because Tectonic's bundle is frozen at TeX Live 2022, it is XeTeX only, and it pins
   biber 2.17 ([the evidence](#the-latex-ladder)). Confirm the flip, or keep Tectonic
   first with the automatic exceptions and a one-click switch?
2. **WIT 0.2.** The plugin plans promised `exec` and `watch` imports. This plan
   recommends a build world instead: the host runs what a plugin's `plan` asks for
   and hands the outputs to its `digest`
   ([why](#why-a-build-plugin-cannot-simply-call-exec)). Confirm, or keep `exec` and
   lift the call budget for builds?
3. **`compile_document` is core's**, offered only where a build plugin is on
   (recommended, one tool for every build plugin and no deadlock), or each plugin's
   own tool?
4. **Third-party build plugins.** Allowed, with the Runs line and a consent dialog
   (recommended), or first-party only until the point has proven itself?
5. **Build folder default.** `~/.cache/chimaera/build` with a 1 GB cap (recommended),
   the runtime directory, or a scratch path?
6. **Compile on open and on agent writes.** Recommend on for both, with the per-document
   toggle. Or only on the user's own saves?
7. **Restricted shell escape.** Keep TeX Live's restricted default (recommended:
   documents rely on it for EPS figures and minted, and the site chose it), or pass
   `-no-shell-escape` everywhere and accept those breakages for a smaller surface?
8. **The default base for Changes.** Last commit when the document is in git
   (recommended), else Before this turn; or Before this turn always, since most
   questions are about what an agent just did?
9. **The changes PDF.** Offer latexdiff's marked-up PDF (recommended: it is what
   co-authors expect), or only the before-and-after view?
10. **Typst jump precision.** Text matching only (recommended to start), or also build
    and maintain a companion binary from Typst's crates for exact jumps?
11. **Typst as the recommended format** for new agent reports in the guide. A product
    stance; recommend yes.
12. **Managed installs.** Offer Typst and Tectonic installs at all? Recommend Typst yes,
    Tectonic after measuring its cache growth on a real home quota.
13. **Markdown to PDF.** A plugin that writes Typst from comrak's tree (recommended), a
    `cmarker` template, or pandoc when present?
14. **Word.** Export and import through pandoc on the build point (recommended), or
    look again for an in-browser `.docx` editor first?

Settled since the last revision, by the plugin system as it shipped: agent tools only
where a build plugin is on; on is active (no footprint); plugins in their own
repositories.

## Out of scope

- **A language server, completion or refactoring** for LaTeX or Typst (texlab,
  tinymist): the DESIGN.md non-goal.
- **A WASM engine in the browser**, for the reasons above.
- **Bundling TeX Live** or managing TeX packages (`tlmgr`). The host's admins and the
  user's prelude own the TeX installation.
- **Editing a `.docx` in place**, styles and tracked changes preserved: no library
  with a fitting licence and footprint
  ([Word](#word-and-editing-what-is-not-markdown)). Word as an output and as an
  import is in scope (Phase H).
- **An extension host**, or any third-party code in the daemon or the UI
  ([packaging](#packaging-plugins-in-their-own-repositories)).
- **Committing, staging or reverting from the changes views.** Git stays read-only
  here ([git](features/git.md)); the views show and point, the terminal commits.
- **Plugin UI code.** A build plugin contributes plans, digests and words; every pixel
  is core's `DocumentView`, so `provides.views` stays unbuilt for this effort.
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
- **Plugins** (at `318be45`). A workbench plugin is a WASM component run by the
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
- **Core document tools.** `mcp.rs` gives every tier `document_guide` (fixed text from
  `agent_docs`) and `check_document`, both in `ALWAYS_ALLOWED_TOOLS` with `notify`.
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

- **Changes and conversion**: [latexdiff on CTAN](https://ctan.org/pkg/latexdiff)
  (`--flatten`, the markup options), [mitex](https://github.com/mitex-rs/mitex) (the
  Rust converter behind the Typst package; crate and licence to confirm).

**Not verified, measured in Phase A instead**: Typst's time and memory for a
20-page report; Tectonic's memory per compile and its official cache size; whether
Tectonic honors `max_print_line`; whether Tectonic confines absolute-path reads; how
long a login shell with `module load texlive` takes on a busy login node; whether
`typst eval` can report heading positions.
