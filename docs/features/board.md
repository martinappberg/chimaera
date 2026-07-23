# Board

Chimaera's visual composition surface: agents write ordinary `*.board.json`
files (decks, cards, figures, quick result charts), the daemon renders them
server-side, the BoardView pane shows the pixels and lets the human move
things, and the agent reads the gestures back through `describe` and the
semantic journal. The design source of truth is
[docs/board-plan.md](../board-plan.md); **all planned slices 0–5 are
implemented** (slice 6 is opportunistic by design: its native `c:chart` arm
ships as an opt-in exporter mode — see Exports — and `equation` ships its
picture arm while the OMML arm is deliberately not built; in-place rich-text
editing ships as a plain-text edit op, not cosmic-text-wasm; hayro PDF import
exists but only behind the non-default `pdf-import` cargo feature — see CLI).

**Where it lives.** Engine: `crates/chimaera-board/` — see its
[map](../../crates/chimaera-board/AGENTS.md) for the module-by-module layout
(schema, pretty, normalize, slots, theme, chart, colormap, diagram,
composites, layout, render, show, describe, lint, arrange, cvd, presets,
journal, imginfo, export/{pptx,svg,pdf}). CLI verbs:
`crates/chimaera-board/src/cli.rs` (the `cli` cargo feature; mounted by the
`chimaera` binary and, pre-Tauri, by the native app binary). Daemon routes:
`crates/chimaera-server/src/board.rs`. Pane:
`web-ui/src/lib/previews/BoardView.svelte` (+ `boardInteract.ts`,
`BoardRail`, `BoardPresentChrome`), registered as the `board` `FileViewKind`
on the full `.board.json` suffix. Chat card:
`web-ui/src/lib/chat/ShownCard.svelte`. Skill:
`.claude/skills/board/SKILL.md` (+ `.agents/skills/board/` bridge).

## The format

- Points only (960×540 for 16:9), origin top-left; slug ids are the diff
  anchor, Edit anchor, journal subject, and merge key; byte-stable canonical
  serialization (a semantically identical save is byte-identical); lenient
  parsing — unknown fields round-trip, unknown/malformed objects are
  preserved-but-not-drawn.
- Five primitives (`text`, `shape`, `connector`, `image`, `group`) + `table`
  (cells are the same `Paragraph` text model; header row, relative column
  widths, equal row split; exports as a native editable `a:tbl`) +
  `equation` (LaTeX math — see below) +
  composites: `chart`, `diagram`, `panelLabel`, `scalebar`, `sigBracket`,
  `legend`, `colorbar`, `callout`, `inset` — each expands deterministically
  to primitives at render/export time.
- **Slots**: 12 named layouts per canvas, selected by a pure function of
  `page.intent.kind` + measured content; explicit geometry always wins;
  resolution happens at render (files never churn). Anchors
  (`above`/`below`/`left-of`/…/`center-of` + offset) resolve against slot
  frames.
- **Charts**: marks `bar` (grouped/stacked/interval via `x2`/`y2`), `line`
  (+KM step), `point`, `area` (+`y2` ribbons, stacking), `rect` heatmaps
  through bundled matplotlib colormaps, `errorbar` (+asymmetric `lo`/`hi`),
  `rule`, `tick`, `text`, and `box` sugar over five-number summaries; linear
  / log (decade ticks) / ordinal / temporal scales (calendar tick labels);
  inline values or CSV/TSV(.gz) `source` binding with sha256 staleness
  (stale is loud and draws no marks); the required `data.origin` chip, plus
  optional `data.trace` (how computed values were produced — clamped 2 KiB)
  and `data.inputs` (the files read) so provenance survives into later
  sessions; histogram/pie/second-y refused.
- **Diagrams**: nodes/edges/lanes under a deterministic Sugiyama-lite layout
  (neighbor-mean refinement straightens chains; an optional per-node `at` pin
  wins verbatim and the rest flows around it) with rounded **orthogonal edge
  routing** — per-edge border ports, one horizontal track per inter-layer
  channel, loop-backs/rank-skips around the columns on stacked side lanes, so
  edges never share a segment or cut through a node; edge labels ride
  surface-colored chips nudged clear of nodes and each other; themed node
  paint (raised process boxes, accent-tinted decision diamonds, `@axis`
  lines/arrowheads) holds contrast on all three bundled themes. Mermaid
  flowcharts import converted-once (`board import`, `show --mermaid` — the
  shown card auto-sizes to the flowchart) with the source kept in provenance.
- **Images**: PNG/JPEG placed with `srcRect` crops; SVG sanitized through a
  usvg round-trip then inlined; `tint` for monochrome sources; effective-DPI
  reporting against preset floors.
- **Equations** (`math` cargo feature, **on by default**): `tex` (LaTeX math,
  display style) + optional `emSize` pt + **required `alt`** carrying the
  LaTeX source — the plan's one named C6 exception (notation, not prose;
  parse fails into the preserved-Unknown treatment without `alt`, and lint
  never counts an equation as verified text). Typeset in-process by mathtex
  (a pure-Rust xetex core; no TeX install) over a bundled STIX Two Math
  (OFL), scaled to fit the frame aspect-preserved and centered, inked with
  the theme foreground. A TeX error renders the standard placeholder with the
  error in diagnostics and is a lint finding. PPTX fate is `raster` — PNG at
  2× placed size **plus** the `svgBlip` vector beside it, `alt` →
  `p:cNvPr/@descr`; the native OMML arm is deliberately not built (slice 6
  opportunistic). A `--no-default-features` build refuses with the exact
  flag to rebuild with.
- Themes: `talk-dark` / `talk-light` / `figure-light` bundled (`@token`
  palettes, role type scale with per-role `minPt`, Okabe–Ito ramp,
  WCAG-checked in tests); workspace themes under `.chimaera/board/themes/`.
- **Presets carry four axes** (geometry / floors / page furniture / rules):
  `talk-16x9`, `design-review`, `exec-update`, `teaching`, `readme-image`,
  `poster-a0`, `pub-nature-single`, `pub-cell`, `pub-plos`. Furniture (page
  number/footer/logo) renders from the preset, suppressed on covers.

## CLI

`chimaera board show` (stdin spec → card under the self-ignoring
`.chimaera/board/shown/`; `--table`, `--mermaid`, `--id` update handle; the
spec's `chart` value takes the **full chart vocabulary** — stated `marks`
incl. `box` over precomputed five-number rows pass through untouched by the
sort/flip sugars, a singular `mark` is one-layer sugar, and top-level
`trace`/`inputs` land in `data`; `--file PATH` cards an **existing** board in
place: validate, render page 1 beside it, print the `shown … → path` line the
chat card mounts on) · `guide` (the complete embedded manual —
`src/cli/GUIDE.md`, printed to stdout so an agent in any workspace learns the
whole tool in one call instead of exploring `--help`/the source; its examples
are test-pinned runnable) ·
`new` · `render` · `describe` (positions + slot resolutions + journal
summary + chart provenance: `source` digest-verified fresh/stale, `inputs`,
a `trace` excerpt) · `journal [--since N]` · `lint [--target PRESET] [--style]
[--strict] [--fix]` (tier census; near-miss alignment and the narrow §3.5
set; mechanical fixes; `--style` also nudges — warning, never an error —
when command/agent-produced inline values carry neither `source` nor
`trace`) · `arrange --op align-*|distribute-*|grid` (journals
as actor `agent`) · `import` (mermaid / SVG / PNG figures into
`.chimaera/board/assets/` with provenance) · `adopt` (promote a shown card)
· `export --format pptx|pdf|svg|svg-outlined` · `theme-export --format
mplstyle|json` · `rescheme` (recolor an existing SVG onto a theme) ·
`validate-theme` (WCAG + OKLCH + Machado-2009 CVD all-pairs ΔE with the
computed safe series cap) · `merge <base> <ours> <theirs> [--check] [-o]`
(the git merge driver — see Merging below).

**PDF-panel import** (`board import fig.pdf [--pdf-page N] [--dpi D]`) is
feature-flagged: builds with `--features pdf-import` rasterize one page
(1-based, default 1; dpi default 300, capped 600; refuses >200-page
documents and >12 Mpx renders) via hayro into a PNG asset, with the source
PDF's sha256 + `path#page=N` in provenance for staleness. Off by default —
including CI and every release — because hayro's interpreter stack is real
weight on the static musl binary; the mainline path stays "export SVG or
PNG from your plotting code". A default build refuses `*.pdf` with the
exact flag to rebuild with.

## Merging

`chimaera board merge <base> <ours> <theirs>` is a per-object three-way merge
on slug ids (engine: `crates/chimaera-board/src/merge.rs`), shaped as a git
merge driver. Wire it up per repo:

```gitattributes
# .gitattributes
*.board.json merge=board
```

```sh
git config merge.board.name "chimaera board merge"
git config merge.board.driver "chimaera board merge %O %A %B"
```

Exit-code contract: **0** for a clean merge, **1** with conflicts — the
result still overwrites `%A` (ours) with the ours-wins best effort, and the
report (one human-readable line per conflict) goes to stderr. `--check`
prints the report without writing; `-o OUT` writes elsewhere. Semantics:
objects keyed by id globally (page restructuring never orphans them),
one-side changes win silently, field-level three-way when both sides touched
one object (both-different → ours + a conflict line), delete-vs-modify keeps
the modified side, page membership follows the mover, theirs-only pages
insert after their nearest surviving predecessor, output through the
canonical byte-stable writer. Merges never touch the journal — a driver runs
from bare/index contexts with no live session.

## Exports

Pure-Rust PPTX (native editable text/shapes/connectors, custGeom with
arc→cubic flattening, charts/diagrams/composites as grouped shapes with real
text, media embedding + svgBlip, notes, generated theme; deterministic bytes;
python-pptx oracle-validated) · multi-page PDF (svg2pdf chunks in one
pdf-writer document, embedded subsetted fonts) · SVG in text and outlined
variants — all off the single `page_svg` emission. Per-object export tiers
(`native`/`grouped`/`vector`/`raster`) with reason strings, gated by preset
`exportFloor` in `lint --target`.

Charts default to grouped editable shapes; `export --format pptx --charts
native` opts into real `c:chart` parts with an embedded minimal workbook
behind Edit Data, for charts that map cleanly (plain/grouped/stacked bars,
lines, scatters on category or linear axes — anything else falls back
per-chart with the reason in its fate line). Native stays **opt-in**: the
board plan gates default-on behind a hand-verified "double-click → Edit Data
opens" pass in desktop PowerPoint that has not run yet, and Google Slides
flattens `c:chart` to a non-editable object either way.

## Daemon routes

Bearer-authed `POST /api/v1/board/render` (content-addressed cache — keyed by
content *and* the render engine's version/epoch, so an upgraded daemon never
serves the old engine's pixels — + diagnostics sidecar → `/raw` ticket),
`/board/describe`, `/board/edit`
(move/resize/text ops by object id; canonical save; appends actor-`human`
journal events; returns `X-Mtime` + `journalSeq`), and `/board/export`
(`{path, format, page?, chartsNative?}` → `{ticket, filename, pageCount}` +
per-object `objects` fates for pptx; `chartsNative` is the CLI's
`--charts native` and answers 422 off pptx; the ticket rides
`GET /download/{ticket}`, a multi-page SVG export ticketing a directory that
downloads as a zip). Blocking work under the
shared fs semaphore; render cache capped at 256, atomic writes;
`.chimaera/board/{renders,exports,journal,shown}` excluded from quick-open
by parent path.

## The pane and chat

BoardView: server-rendered raster stage, outline rail, numeric inspector,
page navigator, click-select, drag-move, corner resize handles, **actor-aware
undo** (⌘Z never reverts agent work — mismatched entries drop with a toast),
**present mode** (fullscreen, keyboard nav, `n` presenter notes, auto-hiding
chrome), and **agent-edit attribution** (external changes flash an accent
outline; own writes don't). Export lives in the pane too: a pagebar chip
opens a popover (pptx/pdf/svg/svg-outlined + the pptx-only native-charts
toggle) whose pptx path shows the §11 fidelity preflight — the per-object
fates from the very export the download button then hands over, never a
second export. Gestures commit through `/board/edit`; agent
edits arrive via the 2 s disk watch and re-render in place. Chat renders a
**ShownCard** under completed tool calls whose output carries the
`board show` signature line (client-detected v1; the daemon `shown` event can
replace the detection later). Discovery is wired, not hoped for: chat spawns
with a workspace get an injected board note (`launcher::BOARD_SYSTEM_PROMPT`
— claude `--append-system-prompt`, codex `developer_instructions`) that is
zero-shot: it carries a complete runnable example, so the agent's first board
action is the `chimaera board show` pipe itself, with no `--help` or source
exploration — and it routes everything richer (boxplots, decks, persistent
boards, provenance) to `chimaera board guide`, the manual embedded in the
binary. The daemon writes a `chimaera` shim (an exec of its own binary)
into the shims dir on every session's PATH, so agents in arbitrary workspaces
can run it without a user install — and that holds in the native app too,
where the daemon IS the GUI binary: it answers `board` argv before any Tauri
init (`crates/chimaera-app/src/main.rs`), so the shim never silently launches
a window instead of the CLI.

## The journal

Seq-first append-only JSONL per board under `.chimaera/board/journal/`
(path-derived key), kebab-case §6.3/6.3b vocabulary (`move`, `resize`,
`text-edited`, `object-added/removed`, `page-*`, `intent-changed`,
`brief-changed`), actor `human`/`agent`/`daemon`, no wall clock, size-capped
seq-preserving compaction. Human gestures append from `/board/edit`; agent
`arrange` appends as `agent`; `board journal --since N` is the read-back.

## Not built (deliberately)

Slice 6's OMML equation arm (`equation` ships the picture arm only —
opportunistic, gated on the fidelity matrix) and native `c:chart` by default;
cosmic-text-wasm in-place editing (the `/board/edit` text
op covers plain-text edits); hayro PDF-panel import in *default* builds
(implemented, but only behind the non-default `pdf-import` feature — see
CLI); the daemon-injected `shown` journal event (the ShownCard detects
client-side for now); server-side per-path edit serialization (client
commits are chained; concurrent multi-client edits are last-writer-wins like
`PUT /fs/file`).

## Intent

*Recorded from the maintainer, 2026-07-22 (verbatim in
[board-plan.md §16](../board-plan.md#16-intent-why-this-exists)):* make it
super easy to work with your agents' outputs and present their ideas to other
people well; visualization is something the workbench should have natively;
what's missing elsewhere is the **editing** that turns generated output into
good, usable, exportable artifacts — and a big usage is the agent showing you
results mid-work, not only deck building. Core bet: the human's gestures on
the surface become structured data the agent reads back, over plain files on
whatever host owns the work.
