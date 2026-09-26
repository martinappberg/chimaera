# Writing a workbench plugin

A **workbench plugin** is an opt-in add-on that runs in Chimaera (not inside an
agent CLI) and says exactly what it adds. Design and rationale:
[timeline-knowledge-plugins-plan.md §6](../timeline-knowledge-plugins-plan.md).
This guide is the recipe — read it before adding one (the LaTeX plugin is the
next planned).

## The rules that don't bend

- **Off by default, per workspace.** A plugin is switched on for one workspace
  (`Workspace.plugins_on`, `PUT /workspaces/{id}/plugins/{pid} {on}`) from the
  Plugins tab. It is *active* there when it is on AND its `detect` footprint is
  present.
- **It says what it adds.** Every manifest carries `[adds] ui = […]` and/or
  `agents = […]` — the card's "Adds / Would add" lines, in plain words. A test
  fails a manifest that adds nothing.
- **Core never changes what agents see; a plugin changes it only where it is
  active.** MCP tools and instruction paragraphs are offered AND call-gated
  only in workspaces where the plugin is active. The plugin-free worker view
  is pinned byte-for-byte by `crates/chimaera-server/src/tests/agent_view.rs`
  — if that test fails, you changed core.
- **Data first, no third-party code.** The manifest is TOML data, embedded in
  the binary (`include_str!`). Behavior lives behind NAMED capabilities
  implemented in first-party daemon/UI code. Nothing is dynamically loaded.
- **Agent-side pieces ride open standards** (MCP, Agent Skills, the agents'
  own plugin managers) so they keep working outside Chimaera. Never
  reimplement `claude plugin` / `codex plugin`.
- **Login-node discipline:** detection is a few `stat`s off the reactor,
  cached; nothing polls; every child process is bounded.

## The manifest

`crates/chimaera-server/src/plugins/manifests/<id>.toml`, registered in
`MANIFESTS` in `plugins/mod.rs`. Parsed with `deny_unknown_fields` — a typo is
a test failure, not a silently ignored key.

```toml
id = "mycelium"                       # stable, lowercase, [a-z0-9-]
name = "Mycelium"
summary = "Project memory your agents record as they work — findings, decisions, learnings."
homepage = "https://github.com/arjunrajlaboratory/mycelium"

[detect]                              # workspace-relative; ANY present ⇒ detected
any = [".living/INDEX.md", "MYCELIUM.md"]   # empty/omitted ⇒ always present

[requires.agent_plugins.claude]       # agent-native plugins it needs, per agent
id = "mycelium@mycelium"
marketplace = "arjunrajlaboratory/mycelium"

[setup]                               # the plugin's OWN documented setup prompt
prompt = "Set up Mycelium in this repository."

[provides]
knowledge = "mycelium"                # a named knowledge reader
mcp_tools = ["knowledge_search", "knowledge_get"]
views = []                            # named first-party UI modules

[adds]
ui = ["Fills Knowledge and “Where things stand”"]
agents = ["2 read tools for every agent here: knowledge_search · knowledge_get"]
```

## Contribution points

| Point | Status | What it does | Where the code lives |
|---|---|---|---|
| `detect.any` | built | footprint → "active here" (no component may be a symlink) | `plugins::detect_blocking` |
| `requires.agent_plugins` | built | per-agent install state (asked of the agents) + an install button that runs the agent's own `plugin marketplace add` + `install`/`add` in a visible terminal | `agent_probe.rs`, `plugins::install_requirement` |
| `setup.prompt` | built | a new chat session of the user's chosen agent, sent this prompt | `plugins::setup_workspace` |
| `provides.knowledge` | built (`mycelium`) | a read-only reader feeding the Knowledge view + `GET /workspaces/{id}/knowledge` | `knowledge.rs`, `mycelium.rs` |
| `provides.mcp_tools` | built | tools served by the chimaera MCP where active, plus an instruction paragraph; pre-allowed at spawn (claude settings, codex driver auto-approve) | `plugins/tools.rs` (`defs`, `instructions`, `call`) |
| `provides.views` | **specified, not yet built** | a lazy-loaded first-party Svelte module | add `web-ui/src/lib/plugins/registry.ts` (id → `() => import(...)`) with the first plugin that needs it |
| `settings` | **specified, not yet built** | typed per-workspace values rendered generically on the card | add with the first plugin that needs it |
| `commands` | **specified, not yet built** | shell templates run as ORDINARY terminal sessions (env prelude applied, visible in the rail, exit codes flow into the Timeline for free) | add with the first plugin that needs it |
| `build` | **specified in the LaTeX plan, not yet built** | files → a bounded, quiet child process on the host (queue, limits, build folder, named parsers/sync, output shown beside the source, a `compile_document` tool where on); LaTeX and Typst are its first two manifests | [latex-reports-plan.md](../latex-reports-plan.md#the-plugin-shape) |
| `recommends.agent_plugins` | **specified in the LaTeX plan, not yet built** | the shape of `requires`, shown as optional: the plugin works with zero installs | with `build` |

"Specified, not yet built" is deliberate (rule of two): the first plugin that
needs a point adds it, following the shape in the plan (§6.1), with a unit
test, and updates this table.

## Adding a first-party plugin — the checklist

1. Manifest in `plugins/manifests/`, registered in `MANIFESTS`.
2. Named capabilities: extend `plugins/tools.rs` (`defs` + `instructions` +
   `call`) for MCP tools; a knowledge reader behind `provides.knowledge`; any
   new contribution point per the table above.
3. Tests: `plugins::tests::every_manifest_parses…` covers the manifest; add
   route/MCP tests in `src/tests/plugins.rs` (tools appear only where active,
   the call gate refuses elsewhere). Run the `agent_view` fixtures unchanged.
4. UI: the Plugins tab renders any manifest generically (card, switch,
   Adds/Needs lines). A plugin view goes through the registry (lazy import;
   the entry-bundle budget in `vite.config.ts` holds).
5. Docs: a feature page (document-feature skill) and this guide's table.
6. Verify live: switch it on in the isolated preview, watch a NEW agent session
   get exactly the advertised tools, switch it off, watch them go.

## The LaTeX plugin: the `build` point

An earlier sketch here put LaTeX on `commands` (a visible terminal session per
build) and `settings`. The [LaTeX and Typst plan](../latex-reports-plan.md#the-plugin-shape)
replaces it: compile-on-save cannot be a terminal per save (a rail entry every
time an agent writes a chapter, no time or memory limit, no build folder), and a
terminal cannot hand back diagnostics, an output file and sync data. So the
plugin's contribution point is `build` — a bounded, quiet child process run
through the environment prelude (`module load texlive` for free), with the
careful pieces (the queue and limits, the log parsers, SyncTeX, the document
view) as first-party code the manifest names:

```toml
id = "latex"
name = "LaTeX"
summary = "Build .tex to PDF and read it beside the source, with errors as editor marks."

[build]
sources = ["*.tex", "*.ltx"]   # opens as a document where this plugin is on
root = "tex"                   # named main-file finder
inputs = "fls"                 # named watch-set source
diagnostics = "latex-log"      # named log parser
sync = "synctex"               # named source-to-PDF mapping
output = "{build_dir}/{stem}.pdf"

[[build.engines]]              # the ladder, first found wins
name = "latexmk"
tools = ["latexmk"]
run = "latexmk -pdf -interaction=nonstopmode -file-line-error -synctex=1 -recorder -outdir={build_dir} -norc {root}"

[adds]
ui = [".tex opens as source | split | PDF · compile on save · errors as editor marks"]
agents = ["1 tool for every agent here: compile_document"]
```

Build plugins carry no `detect` footprint (on = active, so an agent gets the
engine facts before it writes the first `.tex`). A project's own config wins
over the manifest (a `Tectonic.toml`, a trusted `latexmkrc` — fill the gap,
never fight a choice). Typst is the same shape; markdown-to-PDF and Word export
are the third and fourth manifests on the point. The plan also says how a
manifest could later live in a plugin's own repository beside its
`.claude-plugin/` and `.codex-plugin/`
([packaging](../latex-reports-plan.md#packaging-plugins-in-their-own-repositories)).
