# Board

Chimaera's visual composition surface: agents write ordinary `*.board.json`
files (decks, cards, figures, quick result charts), the daemon renders them
server-side, the BoardView pane shows the pixels and lets the human move
things, and the agent reads the gestures back through `describe` and the
semantic journal. The design source of truth is
[docs/board-plan.md](../board-plan.md); **all planned slices 0–5 are
implemented** (slice 6 — native `c:chart`/OMML — is opportunistic by design
and not built; in-place rich-text editing ships as a plain-text edit op, not
cosmic-text-wasm; hayro PDF import was skipped).

**Where it lives.** Engine: `crates/chimaera-board/` — see its
[map](../../crates/chimaera-board/AGENTS.md) for the module-by-module layout
(schema, pretty, normalize, slots, theme, chart, colormap, diagram,
composites, layout, render, show, describe, lint, arrange, cvd, presets,
journal, imginfo, export/{pptx,svg,pdf}). CLI verbs:
`crates/chimaera/src/board.rs`. Daemon routes:
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
- Five primitives (`text`, `shape`, `connector`, `image`, `group`) +
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
  (stale is loud and draws no marks); the required `data.origin` chip;
  histogram/pie/second-y refused.
- **Diagrams**: nodes/edges/lanes with deterministic Sugiyama-lite layout and
  connector-bound edge labels; mermaid flowcharts import converted-once
  (`board import`, `show --mermaid`) with the source kept in provenance.
- **Images**: PNG/JPEG placed with `srcRect` crops; SVG sanitized through a
  usvg round-trip then inlined; `tint` for monochrome sources; effective-DPI
  reporting against preset floors.
- Themes: `talk-dark` / `talk-light` / `figure-light` bundled (`@token`
  palettes, role type scale with per-role `minPt`, Okabe–Ito ramp,
  WCAG-checked in tests); workspace themes under `.chimaera/board/themes/`.
- **Presets carry four axes** (geometry / floors / page furniture / rules):
  `talk-16x9`, `design-review`, `exec-update`, `teaching`, `readme-image`,
  `poster-a0`, `pub-nature-single`, `pub-cell`, `pub-plos`. Furniture (page
  number/footer/logo) renders from the preset, suppressed on covers.

## CLI

`chimaera board show` (stdin spec → card under the self-ignoring
`.chimaera/board/shown/`; `--table`, `--mermaid`, `--id` update handle) ·
`new` · `render` · `describe` (positions + slot resolutions + journal
summary) · `journal [--since N]` · `lint [--target PRESET] [--style]
[--strict] [--fix]` (tier census; near-miss alignment and the narrow §3.5
set; mechanical fixes) · `arrange --op align-*|distribute-*|grid` (journals
as actor `agent`) · `import` (mermaid / SVG / PNG figures into
`.chimaera/board/assets/` with provenance) · `adopt` (promote a shown card)
· `export --format pptx|pdf|svg|svg-outlined` · `theme-export --format
mplstyle|json` · `rescheme` (recolor an existing SVG onto a theme) ·
`validate-theme` (WCAG + OKLCH + Machado-2009 CVD all-pairs ΔE with the
computed safe series cap).

## Exports

Pure-Rust PPTX (native editable text/shapes/connectors, custGeom with
arc→cubic flattening, charts/diagrams/composites as grouped shapes with real
text, media embedding + svgBlip, notes, generated theme; deterministic bytes;
python-pptx oracle-validated) · multi-page PDF (svg2pdf chunks in one
pdf-writer document, embedded subsetted fonts) · SVG in text and outlined
variants — all off the single `page_svg` emission. Per-object export tiers
(`native`/`grouped`/`vector`/`raster`) with reason strings, gated by preset
`exportFloor` in `lint --target`.

## Daemon routes

Bearer-authed `POST /api/v1/board/render` (content-addressed cache +
diagnostics sidecar → `/raw` ticket), `/board/describe`, `/board/edit`
(move/resize/text ops by object id; canonical save; appends actor-`human`
journal events; returns `X-Mtime` + `journalSeq`). Blocking work under the
shared fs semaphore; render cache capped at 256, atomic writes;
`.chimaera/board/{renders,exports,journal,shown}` excluded from quick-open
by parent path.

## The pane and chat

BoardView: server-rendered raster stage, outline rail, numeric inspector,
page navigator, click-select, drag-move, corner resize handles, **actor-aware
undo** (⌘Z never reverts agent work — mismatched entries drop with a toast),
**present mode** (fullscreen, keyboard nav, `n` presenter notes, auto-hiding
chrome), and **agent-edit attribution** (external changes flash an accent
outline; own writes don't). Gestures commit through `/board/edit`; agent
edits arrive via the 2 s disk watch and re-render in place. Chat renders a
**ShownCard** under completed tool calls whose output carries the
`board show` signature line (client-detected v1; the daemon `shown` event can
replace the detection later).

## The journal

Seq-first append-only JSONL per board under `.chimaera/board/journal/`
(path-derived key), kebab-case §6.3/6.3b vocabulary (`move`, `resize`,
`text-edited`, `object-added/removed`, `page-*`, `intent-changed`,
`brief-changed`), actor `human`/`agent`/`daemon`, no wall clock, size-capped
seq-preserving compaction. Human gestures append from `/board/edit`; agent
`arrange` appends as `agent`; `board journal --since N` is the read-back.

## Not built (deliberately)

Slice 6's native `c:chart` and OMML equations (opportunistic, gated on the
fidelity matrix); cosmic-text-wasm in-place editing (the `/board/edit` text
op covers plain-text edits); hayro PDF-panel import (feature-flagged
optional); the daemon-injected `shown` journal event (the ShownCard detects
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
