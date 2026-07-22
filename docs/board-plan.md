# Boards — the visual composition surface: design & plan

Status: **design draft for discussion** (2026-07-21). Nothing here is built.
This document synthesizes eight grounding passes (four over this codebase, four
over verified July 2026 research — OOXML/PPTX internals, scene-graph format
prior art, the pure-Rust render stack, and journal-figure/presentation ground
truth) plus a three-lens design panel (format-first / interaction-first /
workflow-first) with a judge that scored and merged them. It then survived an
adversarial pass from five hostile lenses (daemon constraints, interaction,
export fidelity, agent behavior, product scope) in which each finding had to
survive an independent attempt to refute it: 31 claims were raised, 23 were
refuted, and the 8 that held are folded in below — the output ceiling in §7, the
git-epoch storm in §7, quick-open indexing in §4, overlap-aware undo in §6.7,
PPTX placeholders in §11, group and connector coordinate spaces in §3.6,
lint-through-panels in §3.5, and slice 1's real scope in §14.

A second panel then designed the **element vocabulary** (§3.6–§3.8) — what Board
draws natively from data versus only imports as a picture — over six further
research passes, three lenses (minimalist / coverage / export-first), and a judge.
One of those passes generated its evidence by building real `.pptx` files locally
and hand-writing a minimal embedded workbook, rather than trusting recall.

Decisions marked **[decide]** are the maintainer's call. Everything else is
decided — where a design was contested, the resolution and its one-line
rationale are stated rather than left as options.

Companion plan: `docs/skills-manager-plan.md` (the "Loadout" tab), currently on
branch `claude/chimaera-skills-manager-4c4110` — not linked here because it is
not on this branch yet. The board skill is the natural first-party pack that
dogfoods Loadout's install path, and both features share the same posture: files
are the database, the daemon only scans and serves.

## 0. The one-paragraph version

A **board** is an ordinary git-tracked `.board.json` file — a small, strict
scene graph of pages and named objects (text with real runs, shapes, images,
plot panels, arrows, groups) in **points**, whose every construct is a clean
constructs project onto PPTX at a **declared fidelity tier per object** — most
land as natively editable PowerPoint objects by mechanical projection, and the
rest degrade to a stated tier the preflight tells you *before* you export, never
after. A **theme-token layer**
(`@accent1`, `role: "title"`) sits between the board and its literal styling, so
"strict system by default, fully customizable" is literal: agents author against
a constrained schema, humans restyle by swapping tokens. A pure-Rust engine in
the daemon binary rasterizes a page — or one region, or one object — to
PNG/JPEG in tens of milliseconds, which is how **the agent sees the board**; a
prose `describe` dump is how it **reads positions**; and an append-only
**semantic journal** turns every human gesture (moved `panel-a`, selected
`callout`, pinned "make this stand out") into compact structured events the
agent reads back — which is how it **knows what you meant**. The editor pane
displays the engine's own pixels with a vector overlay for handles, so what you
see *is* what exports, by construction. It is deliberately not a drawing tool:
no pen, no bezier, no blend modes — text, shapes, images, arrows, groups. The
defensible core is **the bidirectional loop over a plain file on whatever host
owns the work** — Claude artifacts and claude-canvas can generate a picture;
nothing lets you drag a panel on a login node and have `codex` read what you did
and why.

## 1. The name — decided: **Board**

The design panel called this "composer" throughout, and that name is taken: the
chat message input is the composer (`web-ui/src/lib/chat/Composer.svelte`,
`composerBus.ts`, `attachImageToComposer`). A second "composer" would make every
future grep ambiguous in the one area — chat — where the two features meet.

**Board** it is (maintainer's call, 2026-07-21). Files are `.board.json`, the
pane is `BoardView`, the crate is `chimaera-board`, the CLI is
`chimaera board render …`, the feature is "boards" in prose. Self-describing,
collision-free, and it survives the feature growing from decks into figures and
posters.

## 2. What already exists to build on

The feature needs almost no new *idioms* — every mechanism it depends on is
shipped and load-bearing somewhere else. That is the argument for building it
natively rather than as a plugin.

| Primitive | Where | Reused for |
|---|---|---|
| Extension-routed file views (`viewKindFor` → `FileView` branch, lazy subview) | `web-ui/src/lib/previews/files.ts:560-575` | `board` view kind — inherits tab identity `f:<path>`, dedupe, preview/pin, rename-follow, delete-prune, quick-open, git decoration, keep-alive, and unknown-kind text fallback **for free** |
| Ticketed binary serving (`POST /fs/ticket` → `GET /raw/{ticket}`, 10-min, range-aware) | `fs.rs:1738-1975` | serving rendered rasters to `<img>` tags that can't send a bearer token |
| Atomic writes with mtime conflict (`PUT /fs/file?expect_mtime=`, 409, `X-Mtime` chaining) | `fs.rs:610-752` | board saves; the 409 path is the conflict-merge trigger |
| Per-workspace epoch invalidation on `/ws/events` (git epoch, `mark_path_dirty`) | `git/service.rs:445-482` | board-changed invalidation — pull-based, never a firehose |
| Registration-scoped fs polling (≤64 mounted files, 2s stat) | `fs_watch.rs` | agent-edit → UI live update for the open board |
| Seq-first append-only JSONL journal (bounded mpsc writer, cap → compaction, torn-tail truncation) | `chimaera-agent/src/journal.rs` | the semantic edit journal, verbatim discipline |
| Nested CLI subcommand enum (the `Compute { cmd }` shape) | `crates/chimaera/src/main.rs:83-90` | `chimaera board <cmd>` — delegation-only entrypoint |
| Leaf engine crate depended on by both binary and server (`chimaera-pty`, `chimaera-agent`) | `Cargo.toml` workspace members | `chimaera-board` — one engine, CLI and daemon, laptop and login node |
| Inline chat artifacts (`turn_end` collection → `ArtifactGallery`/`InlinePreview`, ticket-backed, capped at 8) | `chat/store.svelte.ts:1300-1319` | board artifact cards in chat |
| `composerBus` text + image attachment into the chat input | `chat/composerBus.ts:75` | selection-as-deixis and region snapshots into a message |
| Curated theme system with a 31-token contract and WCAG ≥4.5:1 gate | `settings/themes.ts` | precedent (and quality bar) for board document themes |
| Rebindable action registry + one capture-phase dispatcher | `shared/keys.ts`, `App.svelte` | the board keymap, including extending the `sizable` predicate for ⌘±/⌘0 |
| Boards in product code | — | **zero** |

## 3. The board format

The schema *is* the product. Four constraints drive every decision: it must (a)
project onto PPTX at a fidelity that is **computable before export**, not
discovered after, (b) read like prose under `git diff`, (c) be
authorable by an agent from a ~300-line spec excerpt, and (d) be constrained
enough that a naive agent cannot produce something ugly.

### 3.1 Units: points, only points

**pt (72/inch), never px, never EMU, never affine matrices.** A 16:9 slide *is*
960 × 540 pt. PPTX export is exact integer arithmetic (1 pt = 12,700 EMU).
Physical figure sizes convert with one multiply (Nature single column 89 mm =
252.28 pt) — and the **mm intent lives in the preset**, not the file, so the
board never carries a DPI assumption. Origin top-left, y down. Each object
carries `at: [x, y]` (top-left of the unrotated box), `size: [w, h]`, optional
`rotation` (degrees clockwise about center, matching PPTX `ST_Angle/60000`) and
`flipH`/`flipV`. This is a 1:1 shadow of `spPr/xfrm` decoded into human numbers,
always in page space — including grouped children (§3.6).

One unit everywhere also kills a whole class of agent arithmetic errors, which
is worth more here than in a human-only tool.

### 3.2 Text: explicit sparse runs, not markdown

Runs, not a markdown blob. The fidelity target is `a:p → a:r → a:rPr`, where a
run independently carries family, size, color, and weight — exactly what a slide
callout or a figure caption needs and what markdown emphasis cannot express.
Markdown is offered as **skill-side authoring sugar** that normalizes to runs on
write, never as a stored representation (two stored styling representations
create normalization ambiguity and diff churn).

Verbosity is bounded by sugar: a paragraph that is one unstyled run may be
written as a bare string, and `normalize()` expands it. Runs carry **only
overrides**; everything unset inherits from the object's `role` → the theme.
Sizes are plain pt in the file (export multiplies by 100). Paragraph spacing is
exact pt (`spcPts`, never `spcPct`) for cross-app determinism.

### 3.3 Theme tokens: the `@` sigil

Every color is either a token reference `"@accent1"` or a literal `"#1a1a1a"`.
The sigil makes indirection obvious to both reader and agent, and maps directly
onto the PPTX color model: **`@`-refs export as `<a:schemeClr>`** and literals as
**`<a:srgbClr>`**, so a slide pasted into a themed deck re-themes natively — fonts
included, since a run whose family is its theme role unmodified exports as
`+mj-lt`/`+mn-lt` rather than a literal `a:latin` (which is emitted only for
explicit instance overrides). Baking resolved families into every run would
silently defeat half of "re-themes natively".
Fonts and sizes are referenced *through roles* — `role: "heading"` resolves
family/size/weight/color from the theme's type scale — with sparse instance
overrides winning. **Resolved styles are never baked into the file**, and the
theme is referenced by path, never snapshot-inlined (the theme file is
git-tracked in the same repo, so determinism is already guaranteed; inlining
would churn every board diff).

### 3.4 Ids, order, versioning, lenient parsing

Ids are short human slugs (`panel-a`, `deck-title`), unique per board — they are
simultaneously the diff anchor, the agent's `Edit` anchor, the journal's
subject, and the merge key. Z-order is array order within a page.
`formatVersion` is an integer with explicit migrations. **No churn fields ever**
— no nonces, no `updated` timestamps, no selection or zoom state (Excalidraw's
dirty-on-open is the anti-pattern that makes a format unmergeable).

Serialization is **byte-stable**: fixed key order, 2-space indent, one property
per line, trailing newline — a semantically identical save is byte-identical, so
`git status` stays honest.

Parsing is **lenient and never bricks**: unknown fields are preserved verbatim
(forward compatibility across daemon versions, the settings-store discipline),
an unknown object `type` is preserved-but-skipped-in-render (never dropped from
the array), and a genuinely malformed board falls back to the existing text/Code
view with a repair banner — the file is human-readable JSON, so that fallback is
actually useful over ssh.

### 3.5 Anti-ugly is a format property, not a guideline

The format *prevents* bad output rather than merely permitting good output:

- **Per-role `minPt` lint errors** — Nature 5 pt, PLOS 8 pt, slides 18 pt. Below
  the target's minimum is an error, not a warning.
- **Autofit is unrepresentable.** There is no autofit field. Text is always
  explicit-size and the exporter measures it (cosmic-text) and sizes boxes with
  ~10% headroom, always emitting `noAutofit`. This closes PPTX's `normAutofit`
  round-tripping trap at the schema level.
- **Colors default to `@`-tokens** drawn from validated palettes.
- **Off-canvas objects** and **unresolved fonts** lint as warnings/errors.
- `chimaera board lint --target nature-single` **gates export**, and it lints
  *through* panels rather than merely around them. This matters because in a real
  journal figure almost all the text — axis labels, ticks, legends — lives inside
  the imported panel, and the board's own placement is what creates the violation:
  an 8 pt tick label authored at 6 in wide and then placed at 420 pt is scaled to
  ~4.6 pt. So for an imported panel (an `image` with provenance) whose SVG carries real `<text>`, lint reads
  `font-size` and `stroke-width`, multiplies by the panel's placed/natural scale,
  and checks against the target's `minPt` and `minLineWidthPt`; for a raster panel
  it checks effective DPI at placed size. **Where the panel is outlined glyphs it
  says so rather than passing.** A clean lint means "nothing detectable is wrong",
  never "the portal will accept this". (Scale derives from the SVG root `width` ÷
  `viewBox` width carried through nested transforms — matplotlib writes
  `font-size: 8px` in user units on `width="432pt" viewBox="0 0 432 288"`, so
  assuming CSS 96 dpi would under-report by 25%.)

### 3.6 A concrete board

```json
{
  "format": "chimaera.board",
  "formatVersion": 1,
  "title": "Kinase screen — lab meeting",
  "theme": ".chimaera/board/themes/talk-dark.theme.json",
  "canvas": { "preset": "talk-16x9", "size": [960, 540] },
  "pages": [
    {
      "id": "cover",
      "background": { "fill": "@bg" },
      "objects": [
        { "id": "deck-title", "type": "text", "role": "title",
          "at": [80, 210], "size": [800, 90],
          "text": ["Kinase inhibitor screen"] },
        { "id": "subtitle", "type": "text", "role": "subtitle",
          "at": [80, 310], "size": [800, 50],
          "text": [{ "runs": [
            { "t": "Hits from a " },
            { "t": "1,280-compound", "b": true, "color": "@accent1" },
            { "t": " library" }
          ] }] }
      ]
    },
    {
      "id": "results",
      "objects": [
        { "id": "heading", "type": "text", "role": "heading",
          "at": [80, 48], "size": [800, 56], "text": ["Dose–response"] },
        { "id": "panel-a", "type": "image",
          "at": [80, 130], "size": [420, 320],
          "src": ".chimaera/board/assets/dose_response.svg",
          "provenance": {
            "tool": "matplotlib",
            "script": "scripts/fig2.py",
            "regen": "python scripts/fig2.py",
            "themeExport": "talk-dark.mplstyle",
            "generated": "2026-07-20T09:14:00Z"
          } },
        { "id": "callout", "type": "shape", "geo": "roundRect",
          "at": [520, 150], "size": [360, 120],
          "fill": "@surface", "stroke": { "color": "@accent1", "width": 1.5 },
          "text": [{ "runs": [{ "t": "IC50 = 42 nM", "b": true }] }] },
        { "id": "arrow-1", "type": "connector", "geo": "straight",
          "from": { "object": "callout", "side": "left" },
          "to": { "object": "panel-a", "side": "right" },
          "stroke": { "color": "@fg", "width": 1.5 }, "tailEnd": "arrow" }
      ],
      "notes": "Emphasize the sub-100 nM potency."
    }
  ]
}
```

Object taxonomy — **exactly five primitive types**, and everything else in the
vocabulary is a composite that expands into them (§3.8):

| Type | Purpose | PPTX |
|---|---|---|
| `text` | A box of paragraphs of explicit runs. The only thing that owns glyph layout. | `p:sp` + `a:txBody`; `role: title/heading` → real `p:ph` |
| `shape` | A geometry — named preset **or** arbitrary path — with fill/stroke and optional bound child text. Absorbs `line`. | `p:sp` + `a:prstGeom`, or `a:custGeom` where `geo` is a path |
| `connector` | A stroked two-endpoint geometry binding to other objects by box edge. | `p:cxnSp` with its own resolved `a:xfrm` |
| `image` | Placed pixels or SVG, with optional `srcRect`, `provenance`, `pixelSize`, `frame`, `tint`. Absorbs `plot`. | `p:pic` (+ `svgBlip`) |
| `group` | A z-order and selection envelope over page-absolute children. | `p:grpSp`, identity child space |

Two deletions from the earlier draft, both because *a type whose whole definition
is another type plus a field is not a type*. **`plot` is gone** — it was "an image
carrying provenance", so `provenance`, `pixelSize`, and `frame` become fields on
`image`, collapsing the stale badge, the Regenerate button, the panel lint, the
anchor machinery, and the `p:pic` writer to one code path (and letting a pasted
screenshot carry provenance too). `describe` still *prints* the word `plot` for an
image that has provenance — the human vocabulary survives, the schema branch does
not. **`line` is gone** — a connector's irreducible property is *binding*, not
thinness, so an unbound straight line is `shape` with `geo: "line"`. That removes
the which-type-do-I-emit ambiguity agents get wrong in every tool carrying both.

Groups prefer one nesting level (deeper degrades in Keynote and Google Slides).
Never: freehand, bezier, boolean path ops, blend modes, animation.

**`geo` is a named preset *or* a path.** Named geometries stay the authored
default — they read beautifully under `git diff`, they carry a real `a:avLst`
adjust value so PowerPoint's yellow handle still works, and they are what the
skill points agents at. `{"geo": "path", "d": "M …"}` is the fallback for
geometry that has no name: composite outlines, `sigBracket` and `callout` tails,
`scalebar` caps, violin and KM shapes, and anything `detach-on-drag`
materializes. **Board never infers a preset from a path** — inference produces
near-misses that ship as visibly wrong corner radii.

A path exports as `a:custGeom`, which is a **first-class editable PowerPoint
shape**: the same `spPr` fill and stroke, the same theme colors, the same bound
`a:txBody` with real editable text, and Edit Points works. The relation actually
runs the other way — when a user hits Edit Points on a *preset* shape, PowerPoint
converts it *to* custGeom, so custGeom is the native representation of "a shape a
human has edited." Two writer rules are non-negotiable, both verified to fail
otherwise against a shipping OOXML consumer:

- **Never emit `a:arcTo`.** Its parameterization is unlike SVG's and consumers
  disagree; three separate `arcTo` test shapes drew *literally nothing*, and
  LibreOffice has a long-standing custGeom `arcTo` bug. Flatten every arc to
  cubics (≤4 segments, ~1e-4 relative error). Costs nothing.
- **Always emit `a:path` coordinates in shape-local EMU with `w`/`h` equal to the
  shape's `ext`.** A normalized path space rendered at ~4% of intended size in a
  real consumer — a one-line convention that would otherwise ship microscopic
  shapes in some apps and pass every unit test.

This also makes `detach-on-drag` (§3.8) **lossless**: a detached composite becomes
literal editable shapes with no approximation, which it could not be while `geo`
was a fixed enum.

**Four fields every object carries, none of which is a type:** `anchor` (§3.7),
`alt` (exports as `p:cNvPr/@descr` — ~5 lines of writer, and it completes the
accessibility story), `link` (run- or shape-level `a:hlinkClick`, so a DOI
survives export instead of dying inside a flattened picture), and image-only
`tint` (recolors a monochrome SVG to a theme token).

**Tier 1 for the pptx target** — the vocabulary that lands as a first-class
editable object in PowerPoint 2016+, Keynote, LibreOffice 7.x+, and Google Slides
import. Note the reframing: this is a *property of an export target*, not an
admission test for the schema (§3.7).

*Geometry — wide.* Any path, as `a:custGeom`; the named presets
rect/roundRect/ellipse/triangle/diamond/hexagon/chevron/block-arrows/star5,
`flowChartDocument`/`flowChartDecision`/`flowChartMagneticDisk`/
`flowChartPreparation`/`parallelogram`/`stadium`, and `leftBrace`/`rightBrace`/
`bracketPair` (constant in figures — "these three lanes are the treatment
group"); straight and bent connectors with arrowheads.

*Paint — narrow, and deliberately narrower than DrawingML permits.* Solid and
2–3-stop **linear** fills; **fill alpha** (`<a:alpha>` — there is no Venn overlap,
highlight band, or legend swatch without it); **stroke alpha**
(`<a:ln><a:solidFill><a:alpha>`, valid and honored — its omission earlier was an
oversight); solid strokes with dash/cap/join; at most one soft outer shadow.

*The named silent-failure list, which is why paint stays narrow.* **Radial
gradients, `a:pattFill`, `a:softEdge`, and `a:path/@fill="none"` all failed
silently in a shipping OOXML consumer** — the pattern fill rendered as *nothing at
all*, and `a:outerShdw` did not render either. A figure that arrives with its
hatched overlap as white space is worse than one that refused to export. These are
`vector` tier at best and must be named in the degradation table, never quietly
emitted. **Holes are the least portable construct in the vocabulary**: DrawingML
has no `fill-rule` switch, a hole exists only by subpath-winding convention, and
three donut strategies all rendered as a solid disc. A path with a hole is a
degradation row, not a safe primitive.

*Also tier 1:* explicit runs; `buChar`/`buAutoNum` bullets with explicit
`buFont`; PNG/JPEG at ~2× placed size with `srcRect` crop; `svgBlip`-over-PNG
(**a picture-quality feature, never an editability feature — §11**); a generated
`clrScheme`/`fontScheme` plus **title/body placeholders on a generated master and
a minimal layout set** (title, title+body, blank, picture); plain-text notes.

Two coordinate spaces the schema must pin down explicitly, because leaving them
implicit yields a board that renders correctly in the pane and wrong in
PowerPoint:

- **Grouped children carry page-absolute `at`/`size`, exactly like ungrouped
  objects.** A group is a selection and z-order envelope, not a coordinate
  system. That keeps ids, `describe`, journal move events, off-canvas lint, and
  per-object merge uniform whether or not an object is grouped, and lets the
  exporter emit an identity child space (`a:chOff` = `a:off`, `a:chExt` =
  `a:ext`). Group rotation is therefore never composed with child rotation:
  rotating a group rewrites each child's `at` and `rotation` in page space at
  commit, so the file always reads as what you see.
- **A connector's `side` names an edge of the target's bounding box** —
  top/right/bottom/left — **not an OOXML `a:cxnLst` index**, whose numbering is
  geometry-specific (rect has four connection sites, hexagon six, star5 more, and
  "left" has no stable index across them). The exporter always resolves both
  endpoints to page-space points and writes the `p:cxnSp`'s own `a:xfrm`
  off/ext/flipH/flipV from them — never omitted, never zero-extent — attaching
  `a:stCxn`/`a:endCxn` only as an optional reroute-on-edit enhancement where the
  target preset has a site for that edge. PowerPoint reroutes on edit but renders
  the *stored* geometry on open, and Keynote and Google Slides never reroute.

## 3.7 The inclusion principle, and anchors

"Handle practically everything" has two failure modes: a wish list, and a wall
every real figure hits. What prevents both is a test with an operational
procedure. A candidate is a **native element** if and only if it passes all four
clauses; one failure and it is an imported panel.

> **C1 — Deterministic.** Same file + same theme + same vendored fonts ⇒ the same
> output, byte for byte. No seed, no iteration, no convergence criterion, no
> layout engine running in the consumer application, no command executed at
> render time.
>
> **C2 — Arithmetic-only.** Geometry is computable from numbers already stated in
> the file, the theme, or a plot-ready table it references — using scales,
> layout, and typography, never an estimator, fit, smoother, binner, clusterer,
> projection, or solver.
>
> **C3 — Bounded.** It expands to fewer than 2,000 **drawn** primitives for the
> render path, and its **emitted export-object count** is computable before
> expansion and stays under the target's ceiling. These are two different numbers,
> and an earlier draft conflated them: §3.8's data cap already permits ≤5,000
> drawn marks. The export bound is what actually bites — file size is a non-issue
> in every direction (5,000 custGeom circles zip to 138 KB; a 40,000-segment path
> to 362 KB), but ~200–500 emitted shapes is a comfortable slide, ~1,000 is the
> "are you sure" line, and ~5,000 stops being a document.
>
> **C4 — Closed-form at the destination.** Board resolves all geometry to explicit
> coordinates before writing, and nothing in the output may depend on the
> consumer's text metrics, layout engine, or version. If the destination
> application has to compute where things go, Board does not know what it exported.
>
> **C5 — Declared fate.** Every primitive it emits has a **computable export tier
> at every target**, derived per instance *before* export and surfaced in the
> preflight and in `describe`. An element may land below native at a target; it may
> never land there **silently**.
>
> **C6 — Text integrity.** Every glyph Board draws originates from a `text` run in
> the file, and every such run reaches an editability-claiming target as **one real
> text run** — `a:r` inside a single `a:txBody` on PPTX, a tagged run in PDF,
> `<text>`/`<tspan>` in the SVG real-text variant. Run and paragraph boundaries in
> the file are exactly the boundaries in the output. Text is **never outlined,
> never rasterized, never split per word or per glyph to make it fit.** No element
> that owns text may fall below `grouped` tier.

The one-line form, which goes verbatim into the skill: **Board computes scales,
layout, and typography. Board never computes statistics.** Colloquially: *you
could place it correctly with a ruler and the caption, without opening the
dataset.*

**C4 is stronger than the clause it replaces, not weaker.** It still decides
SmartArt, autofit, live layout containers, and Microsoft's `cx:` chartex in one
stroke — and it now independently forbids the tempting "just embed an SVG and let
PowerPoint convert it to shapes" trick, because that conversion demonstrably
re-lays-out text differently per build and per installed font. What C4 no longer
does is decide *what the schema may express*, which was never its job: **PowerPoint's
feature set is not a dependency of a scientific figure's file format.** The old
clause was in fact already false — `equation` and `colorbar` are native composites
that ship as pictures, so it was documenting a rule shipped elements broke.

**C6 is the load-bearing addition, and it is the maintainer's own caveat —
*"as long as the general text isn't like one text box for every word"* — promoted
from a preference to an invariant.** It is what makes widening the geometry
vocabulary safe. Its live threat comes from upstream rather than from PowerPoint:
**matplotlib mathtext emits one `<tspan>` per glyph with absolute x positions**, so
`-\log_{10}(p)` arrives as 16 separately-positioned single-character runs. Plain
axis and tick labels are one run each and are fine; math labels are not, and lint
must say so rather than let a panel claim verified text.

**The corollary that makes this generous rather than restrictive**, and the single
most useful finding of the whole design pass: *most of the bioinformatics long
tail passes C2 once the statistics have already happened upstream.* A violin
outline is a polygon. A Kaplan–Meier curve is a step line. A box is a five-number
summary. A clustered heatmap is a grid of rects in a supplied leaf order. Board
needs no statistics engine to draw any of them — it needs the script to emit its
reduced numbers. **The seam is a two-way street:** a script that gains
`--emit board-data` upgrades its own picture into a native, lintable,
re-themeable chart without moving one line of statistics out of your Python.

### Anchors — the enabling change

Every object gets an `anchor`, not just `at`. Without this, annotation is a lie:
an arrowhead on a cell, a bracket over two bars, a scale bar on a micrograph all
silently **detach** the moment the panel is moved, resized, cropped, or
regenerated — which is exactly the revision agony the feature exists to kill.

```json
"anchor": { "at": [520, 150] }                                           // page pt — the default
"anchor": { "object": "panel-a", "rel": [0.5, 0.0], "offset": [0, -8] }  // survives move + resize
"anchor": { "object": "micro-1", "px": [512, 300] }                      // survives crop too
"anchor": { "object": "panel-a", "data": [12.5, 0.83] }                  // panel data space
```

Anchors **resolve to page points at `normalize()`**, so `describe`, off-canvas
lint, journal move events, per-object merge (§6.6), per-field undo (§6.7), and the
PPTX writer all keep seeing plain integers and are entirely unchanged. `describe`
prints both the anchor and the resolved point. A `data` anchor requires the target
image to carry a **`frame`** (plot-area rect in pixels + axis limits + scale
type), which native charts have by construction and imported panels earn only via
the regenerate path — a deliberate incentive toward the lintable option.
Regenerating an image marks its anchored dependents **needs-review**.

Anchors must land in **slice 1's schema** even if only `at` and `rel` resolve at
first; retrofitting them re-keys every derived element and every journal event.

## 3.8 The composite vocabulary

Everything beyond the five primitives is a **composite**: it stores intent, and
its geometry is computed at render. Composites are distinct `type` values in the
file — legible under `git diff`, the natural key for the inspector, and already
covered by §3.4's lenient parser — but they are implemented behind exactly one
internal trait:

```rust
trait Composite {
    fn bound(&self, ctx: &Ctx) -> usize;           // C3, computed before expansion
    fn expand(&self, ctx: &Ctx) -> Vec<Primitive>; // pure, total, deterministic
}
```

That single trait is what keeps eleven names from costing eleven times: the
renderer, exporter, `describe`, and `lint` all run on the *expanded* tree and
never learn a composite's name. Three semantics are load-bearing:

1. **The expansion is never stored.** The file holds ~30 lines of intent and git
   diffs the intent. Storing it would be a second stored representation — the same
   normalization ambiguity and diff churn that rules out stored markdown (a theme
   tweak would churn a hundred diff lines per gesture). Spec-only is also what
   makes retheme and retarget free.
2. **Derived children get stable derived ids** — `fig2a/mark.point[3]`,
   `fig2a/tick.x[0]` — addressable in `describe --expand` and in journal events,
   never written to the file.
3. **Detach-on-drag.** Dragging a generated child materializes the expansion as
   literal primitives in a `group`, drops the spec, and emits `object.detached`.
   You gain shapes you can edit; you lose live re-layout — **and the UI says so
   before it happens.**

| Type | Purpose |
|---|---|
| `chart` | Scales, axes, ticks and ≤8 marks over a plot-ready table (below) |
| `table` | Grid of cells with explicit widths, borders, fills → native `a:tbl` |
| `diagram` | Nodes + edges + containers under a deterministic hierarchical/grid/radial layout |
| `panelLabel` | Derived `a`/`b`/`c` from an ordered panel list — never stores the letter |
| `scalebar` | Physical-length bar computed from the target image's `pixelSize` |
| `sigBracket` | Significance bracket with auto-tier stacking. Draws; never tests |
| `inset` | Source rect + magnified crop + leader lines, from one `srcRect` |
| `legend` | Swatch-and-label list bound to the panels it describes |
| `colorbar` | Continuous ramp with ticks; ramp rasterizes on PPTX export |
| `callout` | Shape + leader connector — the most common figure annotation, blessed |
| `equation` | LaTeX, rendered to a cached asset |

### `chart` — eight marks, nine channels, zero transforms

Native charts earn their place because they are the one element where every
clause pays at once: re-themeable with the deck, diffable as numbers, **lintable
through** (§3.5's "cannot verify this panel" structurally cannot apply — the
exporter owns the tick font size), and re-flowable on retarget (a picture's 8 pt
tick becomes 4.6 pt at Cell's column width; a native chart re-lays-out its ticks).

```json
{ "id": "fig2a", "type": "chart",
  "at": [80, 130], "size": [420, 320],
  "alt": "Viability by dose for two cell lines",
  "data": { "source": "figures/fig2a_source.csv", "sha256": "9c1a…", "rows": 24,
            "fields": { "x": "dose_nM", "y": "viability", "err": "sem", "series": "cell_line" } },
  "x": { "scale": "log", "title": "Dose (nM)", "ticks": [0.1, 1, 10], "format": { "sig": 2 } },
  "y": { "scale": "linear", "title": "% viability", "domain": [0, 100], "nice": true },
  "color": { "field": "cell_line", "palette": "@categorical" },
  "marks": [
    { "mark": "errorbar", "capPt": 3 },
    { "mark": "line", "width": 1.2 },
    { "mark": "point", "size": 4 },
    { "mark": "rule", "y": 50, "stroke": "@muted", "dash": [3, 3] }
  ],
  "axes": { "spines": ["left", "bottom"], "grid": "none" },
  "provenance": { "script": "scripts/fig2.py", "regen": "python scripts/fig2.py --emit board-data" } }
```

The eight marks: `bar` (`stack: none|stack|group`) · `line` (`step: none|post` —
*this is Kaplan–Meier*) · `point` · `area` (explicit `y`/`y2` — a CI ribbon *and* a
violin half) · `rect` (heatmap cell over a matrix + named colormap) · `errorbar`
(*this is the forest plot*) · `rule` · `text`. That covers, natively and lintably:
bar, grouped and stacked bar, line, multi-series line, step/KM, scatter, volcano,
forest, dot plot, box (given the five-number summary), violin (given the density
polygon), heatmap (given the matrix and leaf order), CI ribbons, and any of those
with reference lines and direct labels.

**Rejecting Vega-Lite's `transform` block is not scope triage — it is C2 expressed
as a schema.** Nineteen transform types is precisely where "we are writing a
plotting library" begins. Faceting is likewise absent: small multiples are N
`chart` objects placed by `board arrange --op grid`, which is only possible
because Board *is* the layout engine.

**Where the data lives:**

| Form | Cap | Why |
|---|---|---|
| `values` inline | ≤500 plotted points **and** ≤32 KiB serialized | the write cap is 1 MiB; an inline 50k series is an unwritable file, and it poisons the id-anchored sparse-`Edit` contract |
| `source` file + column map + `sha256` + `rows` | ≤20,000 rows, ≤5,000 drawn marks | the default — and it *is* the journal's Source Data deliverable |
| `dataset` | a board-level named table shared across panels | cross-panel consistency for free |

CSV/TSV(.gz) and xlsx only; **parquet is what the regen script reads, never what
Board binds to**. Above the caps there is no native chart — it is an imported
panel, refused with a named error pointing at the path. **Board never silently
downsamples:** choosing how to reduce a dense scatter changes apparent density and
hides outliers, which is a scientific misstatement, and *how* to reduce is an
analysis decision belonging in reviewable code. A `sha256` mismatch is **stale** —
loud in `describe`, `lint`, the pane, and the export result, blocking a
`--target`-gated export, and **never auto-refreshed**, because silently mutating a
figure under review is worse than a badge.

Axis tick formatting is specified, not left to `format!` — significant-figure
default, optional SI prefixes, explicit per-axis `format: { sig, prefix, sep }`,
log minor ticks off by default, and a fixed rounding rule so C1 holds across
platforms. Unspecified, this ships `0.30000000000000004` on an axis in week one.
Continuous colormaps (`viridis`, `magma`, `cividis`, `RdBu`) are 256-entry tables
**bundled in the crate and referenced by name**, with the theme naming its
default; a colormap is never expressible as an `a:gradFill`, and lint refuses any
attempt to approximate one.

### The annotation layer — the actual wedge

Each of these sits *above* an already-rendered panel, which is exactly what no
plotting library can do for itself:

- **`panelLabel`** stores `{ for, corner }` and **never the literal letter**.
  Letters derive from the page's ordered panel list, so reordering panels
  relabels the figure, and a Nature→Cell retarget flips `a b c` → `A B C` with the
  type role swapped, atomically. Labels snap to a shared left guide rather than
  per-panel bboxes, because tick-label widths differ and per-panel alignment reads
  ragged. Lint gains missing / duplicate / gap-in-sequence checks.
- **`scalebar`** `{ for, length: 50, unit: "um", corner: "br" }`, requiring
  `image.pixelSize` read on import from OME-XML, ImageJ/baseline TIFF tags, or PNG
  `pHYs`. Drawn length is computed, so it stays **correct through resize and
  crop** — the classic silent lie in hand-assembled figures. A missing `pixelSize`
  is a lint **error, never a guess**.
- **`sigBracket`** `{ from, to, p: 0.013, tier: 0 }`. Board auto-stacks tiers to
  avoid overlap and maps `p` → `*`/`**`/`n.s.` through a theme-configurable rule,
  so "this journal wants exact p-values" is a preset switch. **Board draws; Board
  never tests.** Tier stacking is a collision problem in *page* space — precisely
  what an upstream static panel cannot solve for itself.
- **`inset`**, **`legend`** (carrying `describes: [...]` so lint can flag drift),
  **`colorbar`**, **`callout`**.

Board draws an `errorbar` from a supplied column and never computes SEM. In
exchange it ships the integrity check nobody else offers: provenance records
`errorBar` / `n` / `test`, and lint asserts the caption states them.

### Imported panels are Tier 2, and Tier 2 is not a lesser tier

An `image` with `provenance` is the right representation for everything that fails
the principle: distributions, violins-from-raw, clustermaps with dendrograms,
survival fits, UMAPs, hexbins, contours, faceted grids, flow-cytometry gates,
genome tracks, sequence logos, chemical structures, phylogenies, microscopy
composites. **This is where the statistics are *correct*** — computed in
reviewable code, in git, in the Methods section — and it is what LLMs author best.
§4 already says an imported matplotlib SVG *is* source; the vocabulary must not
quietly demote it.

Ten mechanisms keep it first-class: the provenance card with Regenerate and
Re-export-theme; `pixelSize` → scale bars; `srcRect` → insets; `frame` → data
anchors; `theme-export` with `svg.fonttype:'none'` so lint reads real `<text>`;
the honest *"⚠ cannot verify text size in this panel"* where glyphs are outlined;
anchors that survive regeneration as needs-review; the stale badge; `alt` and
`link`; and the `--emit board-data` upgrade path. And a vocabulary rule enforces
it: **every composite must work identically over an imported panel and over a
native chart.** A `sigBracket` over a seaborn PNG and over a native `chart` are
the same object.

Flow-cytometry gates get a named rule: Board may reposition an illegible gate
*label*; it may never draw the gate, because the gate polygon *produces* the
percentages.

### Page level

`page.layout` + **named slots** is the default authoring path and ships in slice 1
— §11 already generates a master and layout set with `p:ph` placeholders, so it
costs nothing at export and is expensive to retrofit; it resolves to absolute pt
at `normalize()` and is never live. **`page.caption`** is structured prose that is
**not drawn on the page by default** — Nature and Cell want the caption in the
manuscript, not baked into the artwork — exported alongside the figure and
rendered in only when the target preset says to. It also gives the
error-bar/`n`/test integrity lint something concrete to read, and it removes the
most common reason a figure blows its `minPt` budget.

## 4. Where things live

Boards are **ordinary files anywhere in the workspace** — `figures/fig2.board.json`
next to the manuscript, `talks/lab-meeting.board.json`. This is a deliberate
refinement of the original "everything in a dotdir" instinct: files-as-truth
means the figure belongs next to the paper it illustrates, git-diffability is a
hard requirement, and the `FileTab` pipeline routes any path for free. What
belongs in a managed home is everything *around* the board:

```
.chimaera/board/
  themes/      tracked   *.theme.json      — curated + user themes
  fonts/       tracked   Inter/, …         — vendored fonts (determinism on HPC)
  assets/      tracked   dose_response.svg — imported figure panels
  .gitignore   tracked   — written on first use; ignores the three below
  renders/     IGNORED   <hash>.png        — content-addressed raster cache
  exports/     IGNORED   deck.pptx, fig2.pdf
  journal/     IGNORED   <board-id>.jsonl  — the semantic edit stream
```

The split is exactly your "renders don't clutter but stay accessible": **tracked**
= anything that is truth and must travel with the repo (boards, themes, vendored
fonts, imported assets — an imported matplotlib SVG *is* source; losing it loses
the figure). **Gitignored + reconstructible** = renders (a pure function of
board + theme + fonts), exports (regenerable final artifacts), and the journal
(hot state; git holds the durable audit). All three stay on disk, in the file
tree, one click away — they simply never appear in `git status` or a PR.

**Gitignored is not un-indexed**, and that bites here. No daemon-side walker reads
`.gitignore`: `quickopen::walk` filters by a fixed directory-*name* list
(`.git`, `node_modules`, `target`, …). With render-every-turn (§6.2) a bare ⌘P
would fill with `<hash>.png` noise, and on a Lustre workspace the extra entries
eat into the walker's file cap and time cap — meaning real source files start
dropping out of quick-open as a side effect of using boards. The three generated
directories must be added to the walker's ignore set, not merely gitignored.

**Naming note [decide]:** `~/.chimaera` is the *daemon's* home. A workspace-level
`.chimaera/` is a different thing in a different place, and establishes a
namespace future workspace-scoped features (Loadout's ack snapshot, for one) can
share — but the echo is a real readability hazard. The alternative is a distinct
name like `.board/` or `.chimaera-board/`. Recommendation: `.chimaera/board/`,
documented once, because one workspace namespace beats N sibling dotdirs.

## 5. The pane

A `board` `FileViewKind`: register the extension in `viewKindFor`, add a
`BoardView` branch in `FileView.svelte`. Everything in §2's first row comes free,
including rollback safety — an older daemon renders a v2 board as text rather
than dropping the tab.

**Regions.** A **theme bar** across the top (theme picker, canvas preset, target
preset: Talk-16:9 / Nature-single / Nature-double / Cell / PLOS / Poster-A0). A
left **outline rail** (collapsible): pages as a thumbnail navigator, and under
the active page an object outline — indented, z-order top-to-bottom, click to
select, drag to restack, eye/lock toggles. The center is the **stage** (the page
on a neutral pasteboard, page bounds drawn, optional rulers). The right is the
**inspector**. The bottom is a **status strip**: zoom %, cursor position in
physical units, a live **font-availability indicator** (green all-present, amber
with the missing family *named*), and a "rendered N ms ago" chip.

**Selection, move, resize, snap.** Click selects, shift-click extends, marquee
multi-selects, click-empty deselects. Eight handles plus a rotation handle.
Corner resize is proportional with shift, edge resize is one axis. Snapping to
page center/edges/margins, an 8 pt grid, and sibling edges/centers with live
alignment guides and equal-spacing hints; `alt` suppresses. Arrows nudge 1 pt,
shift-arrow 10 pt.

**Text resize changes the box, never the font size.** This is the single most
important interaction decision in the editor: it is what keeps a figure inside
its journal's font bounds through every resize, and it is exactly what
Illustrator gets wrong. Font size changes only via the role dropdown or an
explicit numeric override in the inspector.

**Alignment tools — layout is a verb, not an object.** A floating toolbar at ≥2
selected: align left/center/right/top/middle/bottom, distribute
horizontal/vertical, match size, tidy into grid, align plot areas. The agent gets
the identical verbs (`board arrange --op align-left|distribute-h|grid|stack-v`),
and both write the *same journal events* — which is what makes the two actors'
histories comparable. Crucially these are **deterministic pure functions that
write absolute `at`/`size` and then disappear**: there are no stored layout
containers, because a container owning its siblings' positions would break
§6.6's per-object merge and §6.7's per-field undo (both key on the object's own
`at`), and PPTX has no container, so a live one would reintroduce exactly the
consumer-side layout drift C4 forbids.

**The fidelity preflight.** Every object has an **export tier** at the active
target, computed by a pure function over the same normalized, expanded tree the
renderer, lint, and the writer already run on:

| tier | meaning | pptx | pdf | svg |
|---|---|---|---|---|
| `native` | first-class editable object of the right kind | `p:sp`/`p:cxnSp`/`a:tbl`/`p:pic` | tagged text + vector | `<text>` + primitives |
| `grouped` | group of native primitives; visual identity exact, composite identity lost | `p:grpSp` | vector | group |
| `vector` | one embedded vector picture; recoverable only by a manual, build-dependent, recipient-side conversion | `svgBlip`-over-PNG | vector XObject | `<image>` |
| `raster` | PNG at declared DPI; terminal | `p:pic` | image | `<image>` |

Three properties are invariants, not niceties. **Demotion is per-element, never
per-page** — one rich object must never drag its page to a picture, which is the
failure mode of every existing export-to-pptx tool and the single thing that
would make this feature untrustworthy. **The exporter may not make a tier
decision the preflight did not already compute**, with a debug assertion on
violation; otherwise you get the classic "preflight said clean, output disagrees"
bug, invisible until a collaborator complains. And **every demotion carries a
reason string naming the field** — `"radial gradient (5 stops) → pptx supports
≤3-stop linear: vector picture"`. The reason string is the entire UX: it is the
difference between a rule and a lecture, and it lets a human decide "fine" or
"I'll use a flat fill" in two seconds.

**The preset carries an `exportFloor`, and that is what keeps the model quiet.**
Talk presets default to `grouped` — you are shipping a deck, so a picture-tier
chart is a real problem and lints as an error. Journal presets default to
`raster`, i.e. don't care, because the output is a PDF and pptx editability is
irrelevant. **Nobody assembling a Nature figure is ever nagged about PowerPoint.**
A "strict editable deck" profile is just `exportFloor: "native"` — a lint profile,
not a schema variant.

Surfaced as: a tier glyph per object in the outline rail **shown only below the
board's floor** (a badge on everything is noise within a day), a running "N export
editable, M as pictures" in the status strip, `lint --target` printing a census
(`38 native · 4 grouped · 1 vector (fig2a: radial gradient) · 0 raster`), and
`⌘K → Export` opening a sheet grouped by tier — each row naming object, tier, and
reason, with reveal-in-stage — alongside unverified-panel warnings, stale digests,
missing `pixelSize`, and every blocking lint. **`describe` prints the tier and the
reason too, and that is a requirement rather than polish:** the preflight is UI,
and the primary author is an agent that never sees it.

Because charts are now native, the export preflight also carries a **CVD and
grayscale check** — deuteranope/protanope/tritanope matrices plus a luminance
transform over the rendered pixmap, shown as four panels side by side, with a lint
warning when two series in one chart fall within a ΔE threshold under simulation.
That is ~30 lines over tiny-skia and a scientific-integrity check no presentation
tool offers.

**Inspector**, contextual: numeric transform (authoritative — you can type
`x = 80`, because the file is numeric and the UI never hides the coordinate
system), fill/stroke with theme swatches first and custom hex behind a
disclosure, text (role dropdown, size shown as *resolved-from-theme* with an
override affordance that displays "= theme (overridden)"), and for imported panels
a **provenance card** with Regenerate and Re-export-theme buttons.

**Present mode** (decks; `P`): full-screen from the pane — page-at-a-time,
arrow/space advance, `Esc` exits, a presenter view on a second window showing
speaker notes, next-slide thumbnail, and elapsed time. It renders the same
engine rasters at display resolution, pre-warming the next page. This is the one
region all three designs under-specified despite "presentations first."

**Empty states.** A new board opens a centered card — Slide deck / Figure panel /
Poster — with a theme picker, plus **"Ask an agent to draft it"** which opens a
chat session with a staged prompt naming the board path. An empty figure page
shows ghost drop-zones sized to the active journal preset. Target: first board
on screen in under 60 seconds, from the file tree or from chat.

**Keymap** (pane-local, through the existing rebindable registry): `V` select,
`T` text, `R`/`O`/`L` rect/ellipse/line, `⌘G`/`⌘⇧G` group/ungroup, `⌘D`
duplicate, `⌘]`/`⌘[` raise/lower, `⌫` delete, `[`/`]` prev/next page, `⌘K`
command palette (align, distribute, retarget, export), `P` present, `⌘±`/`⌘0`
zoom (extending the `sizable` pane predicate), space-drag or trackpad to pan.

**Deliberately absent, and this is a feature:** no pen or freehand, no bezier
node editing, no boolean path ops, no gradient mesh, no blend modes beyond one
soft shadow, no animation timeline, no infinite whiteboard. It beats PowerPoint
and desktop artifacts *for figures and decks* by refusing to become Figma.

## 6. The bidirectional loop

This is the requirement that justifies the feature. Four channels, all over
plain files, all agent-agnostic — a `codex` TUI on a login node uses exactly the
same interface as a structured chat session.

### 6.1 The agent reads positions — `describe`

```
$ chimaera board describe figures/fig2.board.json --page results
page results (slide, 960×540 pt, theme talk-dark)
  heading   text  "Dose–response"          at  80,48   size 800×56   role heading (28pt/@fg)
  panel-a   plot  assets/dose_response.svg at  80,130  size 420×320  from scripts/fig2.py
  callout   shape roundRect                at 520,150  size 360×120  fill @surface stroke @accent1
  arrow-1   connector  callout.left → panel-a.right
  ⚠ caption-1 text 4.5pt — below nature-single minimum (5pt)
```

Prose, named objects, integer pt, resolved styles, lint inline. The agent never
parses raw JSON to *understand* a board — it reads this. It edits the JSON
directly, anchored on ids.

### 6.2 The agent sees the board — raster

```
chimaera board render fig2.board.json --page results --object callout --scale 2 -o /tmp/x.png
chimaera board render fig2.board.json --region 500,140,400,140 --format jpeg -o /tmp/x.jpg
```

Whole page, an arbitrary region, or a single object — your "select which part
should be rendered for the agent to see." Warm renders are tens of milliseconds
(§7), which is what makes "render and look" affordable *every turn* rather than
a special occasion. In chat, the same render arrives as an image attachment; in
a TUI session, the agent runs the CLI itself.

### 6.3 The human's gestures become structured data — the journal

One append-only JSONL per board under `.chimaera/board/journal/`, written with
`journal.rs` discipline (seq-first and gap-free, dedicated writer over a bounded
mpsc, cap → compaction at a `board.opened` boundary, oversize entry replaced
with an error record, crash-torn tail truncated on open). Gestures are
**coalesced**: a 900 ms drag is one `object.moved` with from/to, not 60 frames.

```jsonl
{"seq":41,"ts":"2026-07-21T10:00:01Z","actor":"human","op":"select","page":"results","objects":["panel-a"]}
{"seq":42,"ts":"2026-07-21T10:00:09Z","actor":"human","op":"move","page":"results","object":"panel-a","from":[80,130],"to":[96,130]}
{"seq":43,"ts":"2026-07-21T10:00:14Z","actor":"human","op":"resize","page":"results","object":"panel-a","from":[420,320],"to":[460,350]}
{"seq":44,"ts":"2026-07-21T10:00:21Z","actor":"human","op":"comment","page":"results","object":"callout","pin":"c1","text":"make this the same blue as panel A's points"}
{"seq":45,"ts":"2026-07-21T10:00:40Z","actor":"agent:claude-7f3","op":"restyle","page":"results","object":"callout","changed":{"stroke.color":"@accent2"},"note":"matched panel-a series color"}
{"seq":46,"ts":"2026-07-21T10:00:40Z","actor":"agent:claude-7f3","op":"render","page":"results","hash":"9c1a…"}
```

`chimaera board journal <board> --since 40` is the agent's cheap read of *what
the human just did* — no full-file diff, no guessing. Because every mutation
from either actor writes both the file and an event, the journal is a complete
semantic causal trace across both, in human-readable named-object terms
(requirement 3a satisfied twice over: `describe` for state, journal for change).

**Three details that are easy to get wrong and must be in slice 1:**
1. **Multi-writer.** The daemon UI and a separately-invoked `chimaera board`
   CLI process can both want to append. The CLI **routes its append through the
   running daemon** when one owns the workspace (a local route call), and only
   falls back to a direct advisory-locked append when no daemon does. One writer
   per file, always.
2. **Rename-follow.** Journals are keyed by board path; a rename must re-key the
   journal on the same event that already rewrites tab paths, or a renamed board
   silently orphans its history.
3. **Compaction must preserve** unresolved `comment` pins and the latest
   `select` entry — otherwise deixis and pins vanish at a cap boundary, which
   would be a spooky, hard-to-reproduce bug.

### 6.4 Selection is pointing

When objects are selected in the pane and you type "make **this** bigger" into a
chat session, the board resolves `this` → object ids and injects, via the
existing `composerBus`, a compact context line (`[board: figures/fig2.board.json
› results › callout, arrow-1]`) **plus an object-scoped region snapshot as an
image attachment**. The agent knows which objects and sees them, without you
describing either. In a TUI session — where Chimaera never types into a PTY —
the same action instead offers "copy snapshot path", dropping the render into
the session upload dir and giving you an `@`-path to send yourself.

Comment **pins** are the lower-bandwidth version of the same thing: drop a
numbered dot on the canvas, optionally bound to an object, with text. Pins live
in the journal only — never in the board file, whose diffability must not be
polluted by conversation. Resolving appends `comment.resolved`.

### 6.5 The agent's edits land legibly

The agent edits the board with its ordinary `Edit`/`Write` tools (id-anchored,
sparse). The write bumps the workspace epoch; the board is a registered mounted
path so `fs_watch` picks it up; `fileStore` revalidates **in place** (never
null-then-refetch). `BoardView` then diffs old vs new **by id** and animates the
delta: moved and resized objects tween ~180 ms to their new box, created objects
fade and scale in, deleted objects fade out, restyled objects flash a 1.2 s
**attribution glow in the acting agent's hue**, and a transient chip narrates it
("claude moved panel-a, restyled callout") with click-to-focus.

This is the anti-poltergeist mechanism and it is not decoration: an agent that
silently rearranges your figure is unnerving and untrustworthy, and the fix is
that every remote edit is *narrated, attributed, and animated* so you can see
what changed and undo precisely.

### 6.6 Conflict: you are dragging while the agent writes

Because the file is structured JSON with stable ids and zero churn fields,
conflict resolution is a **per-object three-way merge**, not textual:

- On drag start the UI snapshots the on-disk board + `X-Mtime` as base, holds an
  optimistic local overlay, and does not write.
- If the agent writes mid-drag, revalidation applies the agent's changes to
  **every object except the one under your pointer**, tweening them in with
  attribution, and silently advances the base mtime. Your drag is never yanked.
- A gesture's end appends to the journal immediately, but the **file write is
  coalesced on a ~500 ms idle boundary** (forced flush on pane blur/close, on
  export, and before any `describe`/`render` reads the file). The optimistic
  overlay already covers the gap, so a burst of drags — and key-repeat arrow
  nudges, which would otherwise write ~30×/s — becomes one write carrying the
  accumulated multi-object delta. The write still carries `expect_mtime`; a 409
  re-reads, re-applies your deltas by id, and rewrites.
- **Same-object collision:** your in-flight delta re-applies on top of the
  agent's write for that object — you win where you are actively touching, the
  agent wins everywhere else — with an attributed toast and one-click undo. No
  modal conflict bar; the human is present and undo is cheap, so a combative
  dialog is the wrong instinct.

The common case (agent restyles `callout` while you move `panel-a`) merges
cleanly with zero loss, which is only true because ids are the merge key.

### 6.7 Undo across two actors

Undo is journal-driven and **actor-aware**, but "skip the other actor's entries"
is only sound when the entries commute — and same-object edits do not. The naive
version actively causes the harm it was meant to prevent: you nudge `panel-a` to
`[96,130]` (seq 42), the agent later re-lays-out the page and moves it to
`[300,200]` (seq 47), you press `⌘Z` once meaning to undo your nudge, and
inverting seq 42 writes the *absolute* `[80,130]` — silently destroying the
agent's move, with no attribution, because undo believed it only touched your own
event. §6.6's merge policy makes interleaved same-object entries the common case,
not the corner case. So the contract is per-field and checked:

- **Events record per-field prior/new values** — `"changed":{"at":{"from":[80,130],"to":[96,130]}}` — so overlap is detectable at all. `move` and `resize` normalize to the shape `restyle` already has.
- **Undo is overlap-aware.** Undoing entry E scans forward for any later entry, from any actor or another window, touching the same (object id, field path). None found — invert directly and silently; this is the common case and stays a plain `⌘Z`.
- **On overlap, never clobber silently.** Prompt with attribution ("claude has since moved panel-a — undo your move anyway?" / [Undo mine] [Keep claude's]). "Undo mine" on a numeric geometry field applies the **inverse delta** (−16 pt x) to the current value rather than restoring an absolute box. For non-numeric fields (color token, text, enum) no delta exists, so the honest options are clobber-with-consent or skip-and-say-so. Either way the entry is marked resolved so a second `⌘Z` does not re-prompt.
- **The invariant:** *an event is invertible only against a journal in which no later event touched the same object and field path; otherwise undo rebases or asks. Undo never writes an absolute value over another actor's later write without attribution.*

A separate menu item explicitly *targets* an agent edit ("Undo claude's restyle
of callout?") for when that is what you actually want. Git remains the coarse
time-travel backstop.

### 6.8 Latency budget

| Segment | Budget |
|---|---|
| gesture → optimistic UI | < 16 ms (local overlay, no round trip) |
| gesture → journal append | async, backpressure-bounded, never blocks a gesture |
| save → disk (atomic PUT) | < 5 ms local, + RTT remote |
| save → git panel + tree refetch | **not per gesture** — coalesced; one `git status -uall` per settle window |
| agent edit → UI animation starts | ~0 ms same-window (client bus); ≤ 2 s cross-process (fs poll) |
| selection → snapshot attached in chat | < 100 ms (warm render + attach) |
| warm page render | 20–60 ms; promise < 100 ms, target < 50 ms |

The only segment above 100 ms is cross-process file-watch detection on the 2 s
poll — acceptable for the human-perceptible "the agent is working" beat, and
invisible for same-window edits. The `save → disk` figure is the write alone;
the epoch fan-out it would otherwise trigger is exactly why writes are coalesced
rather than issued per pointer-up.

## 7. Rendering, and why the pane shows the engine's pixels

A new **`chimaera-board`** leaf crate (depends only on `chimaera-core`; both the
binary and `chimaera-server` depend on it — the `chimaera-pty` pattern). Stack,
all pure Rust with zero C dependencies so it cross-compiles to musl exactly like
the daemon: **usvg + resvg + tiny-skia** (SVG panels, shapes, compositing),
**cosmic-text** (our text layout — shares `fontdb` with usvg, so one font world),
**fontdb** (discovery), **krilla + krilla-svg** (PDF with real selectable text
and font subsetting), **png** (image-rs, fast profile for previews) and
**jpeg-encoder**. Roughly 8–14 MB of binary.

**The RSS math, stated honestly, because it constrains the feature set.** A
framebuffer is one contiguous `w × h × 4` tiny-skia pixmap — it scales with
*output pixels*, not board complexity, and tiny-skia does not tile. A 1920×1080
screen preview is 8.3 MB; a 16:9 slide at 300 DPI is 36 MB; a Nature
double-column figure at 300 DPI is ~24 MB; **A0 at 300 DPI is 139 Mpx = 558 MB**,
which would take out a daemon whose whole budget is ~150 MB — and with it every
PTY and chat session it owns. So: a hard **output ceiling of ~12 Mpx (~48 MB)**
on every raster path, `--scale` included, and renders above ~4 Mpx take a
**separate 1-permit large-render lane** rather than sharing the 8-permit
filesystem pool (eight concurrent 48 MB renders is itself over budget). The
existing size gate covers inputs; this one covers outputs, and the plan needs
both.

**The parity decision.** The stage displays **server-rendered rasters** with a
thin client-side vector overlay for selection handles, guides, snap lines, and
drag ghosts. The page raster is the truth layer; when you grab an object, the
daemon mints a two-part render (page-without-object, object-alone) so the drag
translates a crisp sprite over a static backdrop, and a fresh render lands on
release.

This is the same instinct as *terminal state lives server-side*: it means **what
you see is what exports, visually, by construction, on day one** (a demoted object
is pixel-identical — that is the entire point of a tier; what you get
*structurally* is the per-object contract of §5, printed before you export) — no
DOM-vs-cosmic-text
metric drift, no "it looked right in the editor and wrapped differently in the
PPTX", and imported matplotlib SVGs render through the identical pipeline in
both places. The cost is that in-place *text editing* wants a local layout
engine; that arrives in a later slice by compiling cosmic-text to wasm, and
until then text editing happens in an overlaid field that commits to a re-render.
**DOM `measureText` must never become layout truth at any point** — that is the
door through which drift permanently enters.

**CLI** (a nested `Board { cmd }` enum, the `Compute` shape):

```
chimaera board new      <path> [--kind slide|figure|poster] [--preset ID] [--theme ID]
chimaera board render   <board> [--page ID] [--object ID | --region X,Y,W,H]
                                [--scale N] [--format png|jpeg] [-o FILE]
chimaera board describe <board> [--page ID]
chimaera board journal  <board> [--since SEQ] [--limit N]
chimaera board lint     <board> [--target nature-single|cell|plos|talk|poster-a0]
chimaera board export   <board> --to pptx|pdf|svg|png [--target ID] [-o FILE]
chimaera board rescheme <asset.svg> --theme ID [-o FILE]
chimaera board fonts    [--board <board>]
chimaera board theme-export <theme> --as mplstyle|ggtheme [-o FILE]
```

**Daemon routes** (bearer-authed, versioned envelopes, additive to the stable
wire): `POST /api/v1/board/render` → mints a `/raw` ticket; `GET
/api/v1/board/describe`; `GET|POST /api/v1/board/journal`; `POST
/api/v1/board/export` → a download ticket. Board mutations bump a per-workspace
**board epoch** on `/ws/events` — invalidate-and-pull, never payload on the
firehose.

Board writes go through a **dedicated `PUT /api/v1/board/file`** rather than the
generic `PUT /fs/file`, for one specific reason: the generic route calls
`mark_path_dirty` (`git/service.rs:448`), which bumps the *git* epoch, wakes
`/ws/events`, and makes every window on the workspace refetch
`git status --porcelain=v2 -uall` under a 4-permit pool — seconds on a large repo
over Lustre. Boards are tracked files, so at one save per pointer-up a normal
layout session becomes a sustained `git status` storm. The board route bumps the
board epoch immediately (that is what drives the pane) and **defers the git bump
to a ~1 s per-path settle timer**, so a layout session costs one `git status`
rather than one per gesture.

**Render cache** in `renders/`, content-addressed on
`hash(page subtree + resolved theme + font-set fingerprint + region + scale)`,
LRU-capped by directory byte budget. An agent's `render` journal entry records
the hash, so the UI reuses the exact bytes the agent saw. This is what makes
render-every-turn cheap.

## 8. Fonts

First-class, because on a login node this is where naive tools fall over. Order:
**vendored** `.chimaera/board/fonts/` (git-tracked, so a figure renders
byte-identically on your laptop and on a fontless compute node) → **bundled**
OFL defaults baked into the binary via `include_bytes!` (Inter, Source Sans, a
Noto subset) → **system** scan via `fontdb` (pure-Rust fontconfig parsing, no C
linkage, works headless).

A missing font **never fails and is never silent**: the nearest family
substitutes, and the substitution is surfaced in the status strip (naming the
missing family), in `describe`, as a `lint` error, on the render response, and
as a faint corner watermark on the raster — so a wrong-font export cannot be
mistaken for a correct one. Vendoring carries OFL attribution and redistribution
obligations; the bundled set ships its licenses and `board fonts` prints them.

## 9. Themes and the curated defaults

A `.theme.json` carries `palette` (named colors), `type` (roles → family/size/
weight/color, each with `minPt`), `fonts` (family → vendored path), `spacing`
(the 8 pt grid, margins), and `presets`. Defaults are an acceptance criterion,
not a nicety — the whole promise of "better than PowerPoint out of the box"
lives here. Ship:

- **talk-dark** and **talk-light** — the Butterick fixes: off-neutral background
  (never pure white or black), body text at ~88% gray, exactly one accent, a
  modular ~1.25 type scale on the 960×540 pt / 8 pt grid, at most two families.
- **figure-okabe-ito** and **figure-tol-bright** — categorical palettes that are
  colorblind-safe and grayscale-robust (`#E69F00 #56B4E9 #009E73 #F0E442
  #0072B2 #D55E00 #CC79A7`; Tol Bright as the alternative), **Viridis** for
  continuous. No red-green encoding, no jet, ever. Minimal chrome: top/right
  spines off, thin axes, direct labels over heavy legends.
- **journal-nature / journal-cell / plos** — correct column widths, font-size
  bounds, and panel-label style (Nature 8 pt bold lowercase `a b c`; Cell/PLOS
  capitals), with **Arial and not Helvetica for PLOS** — the specific trap that
  bounces submissions.
- **poster-a0**.

Themes validate for WCAG text contrast, reusing the app-theme legibility
contract. **Target presets are first-class**: switching Nature-single → Cell
atomically remaps canvas width, font-size bounds, panel-label style, and export
format — resubmission becomes one click instead of a manual redo.

## 10. Figures: matplotlib, ggplot, and rescheming

**Import.** Drop an `.svg`/`.pdf`/`.png` from `results/` onto the stage (or from
the file tree); it is copied into `assets/` (tracked) and becomes an `image` node
with `provenance {tool, script, regen, themeExport, generated}`. matplotlib SVGs
exported with the default `svg.fonttype:'path'` render pixel-faithfully with no
fonts required, which is the easy and common case.

**Rescheming, two paths, in this order of preference:**

1. **Regenerate on-theme (lossless, preferred).** `chimaera board theme-export`
   emits the theme as a `.mplstyle` or a ggplot `theme()` snippet — palette as
   `axes.prop_cycle`, family and per-role sizes, spines off, `svg.fonttype:'none'`
   (real text!), `pdf.fonttype:42`, transparent background, exact figsize in mm,
   and never `bbox_inches='tight'` (which silently breaks exact sizing). The
   provenance `regen` command re-runs the script; the agent or the Regenerate
   button reproduces the panel natively on-theme.
2. **In-place SVG recolor (for what you cannot regenerate).** `board rescheme`
   remaps colors **by element id/group and role — never a global hex
   find-replace**, because the same hex means a data series in one place and a
   gridline in another. Text re-fonting works only where the SVG carries real
   `<text>`, which is precisely why path 1 pushes `fonttype:'none'` upstream
   rather than trying to reverse-engineer outlines.

A panel whose script is newer than its asset shows a **stale badge** with a
one-click regenerate. PDF-panel import (via `hayro`) is feature-gated to a late
slice; v1 honestly says "export SVG or PNG from your plotting code."

## 11. Exports

- **PPTX — a pure-Rust OOXML writer** (`zip` + `quick-xml`) inside
  `chimaera-board`, emitting §3.6's tier-1 vocabulary natively and everything
  else at its declared tier. This is a deliberate deviation
  from the research's python-pptx recommendation, and the argument is a Chimaera
  invariant: the daemon is one static musl binary deployed to login nodes with no
  Python and no system dependencies, so shelling out breaks remote-transparency.
  The subset is a small fixed vocabulary — tractable to emit directly. **python-pptx
  becomes a CI-only fidelity oracle**: tests open our output and assert object
  counts, text, positions, and **a title placeholder per titled slide** round-trip.
  A PowerPoint / Keynote / LibreOffice / Google Slides fidelity matrix is a
  release gate, not a hope. Always `noAutofit`; `@`-tokens become `schemeClr` refs
  against a generated `clrScheme`.

  Text objects with `role: "title"`/`"heading"` export as **real placeholders**
  (`p:nvSpPr/p:nvPr/p:ph` with `type`/`idx` matching the generated layout), not
  free-floating text boxes. This is the one part of the package that is a design
  decision rather than boilerplate, and getting it wrong fails *silently*: without
  a `ph`, the deck opens pixel-correct while Outline view is empty, the thumbnail
  rail shows no slide titles, "Reuse Slides" and "Reset" do nothing, and the
  Accessibility Checker flags every slide as missing a title — none of which an
  object-count/text/position oracle would catch. Placeholder shapes still carry
  their own explicit `xfrm` (board positions are truth; nothing inherits geometry
  from the layout, so Reset cannot rewrite a figure), and the generated master's
  `txStyles` are inert so placeholder inheritance cannot reintroduce the
  `normAutofit` trap §3.5 closes at the schema level.
- **PDF** via krilla — real selectable text, subsetted fonts.
- **SVG** in two variants: text-as-paths by default (renders identically
  anywhere) and a real-`<text>` + `@font-face` toggle (editable in Illustrator
  and Inkscape). This is matplotlib's own tradeoff, surfaced as a choice.
- **PNG/JPEG** at export DPI (≥300 for journals, 600 for line art).

- **Tables** export as native `a:tbl`. Cells are `a:txBody`, so this reuses the
  `a:p`/`a:r`/`a:rPr` writer verbatim; `tableStyles.xml` is a single empty element
  and Board styles every cell explicitly. ~150 lines, universally safe, and the
  cheapest win in the document.
- **`alt`** → `p:cNvPr/@descr` and **`link`** → `a:hlinkClick`, so accessibility
  and DOIs survive rather than dying inside a flattened picture. Note that array
  order is both z-order *and* screen-reader reading order — a free accessibility
  win and a constraint on any future auto-restack.

**Charts export as grouped vector shapes, not native `c:chart` — in v1.** Worth
recording *why*, because the research verified the alternative is feasible rather
than merely imagined: a minimal `c:chart` part is ~1.9 KB, and a hand-written
5-part 1.6 KB embedded workbook was built and read back successfully using `zip` +
`quick-xml` alone. But it is a second XML writer plus a `numCache`/workbook
duplication hazard; **Google Slides flattens `c:chart` to a non-editable object
anyway**; Keynote's behavior is *unverified*; and grouped shapes are editable in
every target, including the two that flatten. Because the expansion is
deterministic, `c:chart` remains available later as a pure exporter optimization,
gated on a hand-verified "double-click → Edit Data opens" pass.

**The degradation contract — stated before you export, not discovered after:**

| | PowerPoint | Keynote | LibreOffice | Google Slides | PDF | SVG |
|---|---|---|---|---|---|---|
| `text` `shape` `connector` `group` | editable | editable | editable | editable | real text | text/paths |
| `table` → `a:tbl` | editable | editable | editable | editable | real text | text/paths |
| `chart` + all composites | editable shapes; **composite identity lost** | editable shapes | editable shapes | editable shapes | vector | vector |
| `equation` v1 | picture | picture | picture | picture | vector | vector |
| imported panel | picture | picture | picture | picture | embedded | embedded |
| `alt`, `link` | preserved | preserved | preserved | preserved | tagged | `aria`/`<a>` |

Composite identity is lost at the destination and that is *correct* — the identity
lives in the `.board.json`, which stays yours. Keynote's handling of grouped
composites goes on the fidelity matrix as **unverified and hand-checked**. This
table is *generated* from the same `tier()` the preflight calls, so it cannot
drift from the exporter.

Four corrections that fall out of the tier model:

- **`svgBlip` is a picture-quality feature, and the docs must say so plainly so
  nobody re-litigates it.** Everything already landing at `raster` — `equation`,
  the `colorbar` ramp, imported panels — costs nothing to *also* carry as
  `svgBlip` beside the PNG: modern PowerPoint and LibreOffice ≥24.2 get
  resolution-independent vector, everyone else gets today's PNG, nobody gets
  worse. It is **not** an editability mechanism (§11 note below).
- **`colorbar` splits.** The ramp is a continuous field and rasterizes, but its
  **tick labels must not go down with it** — they export as separate `native` text
  objects positioned over the raster. C6 forbids taking real text into a picture.
- **`equation` is the one named C6 exception**, carved explicitly so it cannot
  generalize: an equation is *notation*, not prose. It must carry `alt` with the
  LaTeX source, and lint must not count it as verified text. The SVG
  text-as-paths variant is likewise not a C6 violation — it is a deliberately
  chosen rendering output, and the preflight names it as such.
- **The python-pptx oracle is structurally blind to `svgBlip`** — it reports such
  a picture's content type as `image/png`, seeing only the fallback. Any assertion
  about SVG content must go through raw XML. (It is also not an authoring
  reference: its `FreeformBuilder` supports only `moveTo`/`lnTo`/`close` — no
  béziers at all — and is O(n²), taking 163 s for 40,000 segments against 0.11 s
  for a direct-XML writer. The pure-Rust-writer decision is vindicated.)

**Why we do not embed SVG to get editability**, recorded once because it is the
obvious idea and it does not survive contact with evidence: PowerPoint's *Convert
to Shape* is a manual, per-graphic, recipient-side ritual; it is **Windows-M365
only and absent on PowerPoint for Mac**; Google Slides ignores the SVG entirely
and renders the PNG forever; and its text outcome is build-dependent, with three
credibly-reported results — retained, silently dropped, and exploded per letter.
Board authoring the content means Board can emit real shapes and real text
instead, and `a:custGeom` (§3.6) delivers exactly the "shapes and boxes, all
editable" outcome with zero recipient action and no risk to text.

Exports land in `.chimaera/board/exports/` and are offered as a download ticket.
`lint --target` runs first and blocks on min-font-size (board text *and* scaled
panel-internal text), sub-minimum line weight after panel scaling, effective
raster DPI below the target floor, off-canvas, and unresolved-font errors. Any
panel it could not inspect is reported as an unverified-panel warning on the
export result. Target presets therefore carry `minLineWidthPt` (Nature: 0.25) and
`minEffectiveDpi` (300 halftone / 600 line art) alongside `minPt`.

## 12. Skills and chat

Canonical `.claude/skills/board/SKILL.md` + a `.agents/skills/board/` Codex
bridge — the interface is **files + CLI, zero MCP**, which is what makes it work
for any agent, in a TUI or in chat, locally or over ssh. The skill is the
format's `llms.txt`: the coordinate system, the object taxonomy, the run and
token model, the id-anchored sparse-edit contract, the CLI, and the loop
("after editing, `render` and look; before asking, `describe`; to know what the
human did, `journal --since`"). A versioned spec is fetchable, because models
confidently emit stale formats otherwise.

The skill opens with the line that makes the whole vocabulary teachable in a
paragraph, and it is worth quoting verbatim because everything else follows from
it:

> **Board computes scales, layout, and typography. Board never computes
> statistics.**
>
> There are five object types. Everything else you have heard called an element —
> chart, table, diagram, bracket, scale bar, legend, colorbar, inset, panel label,
> callout, equation — is a composite that expands into those five,
> deterministically, from numbers you already stated.
>
> If drawing it would require reading your dataset or fitting a model, it is a
> panel your script produces and Board imports, with provenance — and that is a
> first-class answer, not a consolation prize.

**Chat.** `.board.json` joins `viewKindFor` and `INLINE_PREVIEW_KINDS`, so a
board an agent writes surfaces as an **inline artifact card** — rendered through
the render route and backed by `fileStore` (the *live* pattern, not the
write-once memo, so the card updates as the agent iterates). A per-tile "Open in
board" action threads through `ArtifactGallery` → `InlinePreview`. Outgoing
snapshots and selection deixis use `composerBus` (§6.4). And `board new` plus a
staged prompt means an agent can bootstrap a board from inside a chat turn with
no CLI round-trip — the empty-state button and the skill both go through it.

## 13. Edge cases and failure honesty

- **Missing fonts** — substitute, name it in five places, watermark the raster.
- **Huge assets** — size-gate before parse; rasters downscaled to ~2× placed
  size; an oversized SVG is refused with an explanation, never an OOM on a
  150 MB-budget daemon.
- **Huge outputs** — the ceiling is on output pixels too, not just inputs. A
  render or PNG/JPEG export whose `page size × scale` exceeds ~12 Mpx is refused
  with a named error stating the computed pixel count and pointing at the vector
  target, exactly as an oversized input is. A0 at 300 DPI is a 558 MB single
  allocation and nothing about the board's contents makes it smaller. Poster and
  ≥600 DPI targets therefore default to PDF (krilla streams — no full-page
  framebuffer) or SVG; high-DPI PNG is the explicit exception. If a genuine
  600 DPI poster raster is ever needed the escape hatch is banded rasterization
  (horizontal strips with a translated transform, streamed into the encoder, with
  overlap where filters cross strip boundaries) — deferred until something needs
  it.
- **An opaque panel.** matplotlib's default `svg.fonttype:'path'` emits outlined
  glyphs — no `<text>`, no `font-size`, nothing to measure. Such a panel lints as
  an explicit *"⚠ cannot verify text size in this panel"* in `describe` and on the
  export result, **never a silent pass**. This is the strongest argument for §10's
  regenerate-on-theme path: `svg.fonttype:'none'` is what makes a figure fully
  lintable, not merely on-palette.
- **Malformed board** — lenient parse; true corruption falls back to the text
  view with a repair banner and an "ask an agent to fix it" affordance.
- **A future formatVersion** — text fallback, never a dropped tab.
- **Remote hosts** — every step (render, export, describe, journal) runs on the
  daemon that owns the files; the same binary is already deployed there, so the
  whole loop is remote-transparent with no new transport. Only ticketed rasters
  cross the tunnel.
- **A stale `frame`.** A `data:` anchor resolves through its panel's `frame`; if
  the script is re-run with different axis limits, the anchor points at the wrong
  data coordinate **with no visible symptom**. This is the one place in the design
  where a wrong answer is invisible, so `frame` carries the asset's content hash:
  on mismatch, `data:` anchors degrade to their last resolved `rel` and lint
  raises an error.
- **`regen` never runs on the render path.** The Regenerate button executes
  `provenance.regen` **only** on an explicit human click or agent CLI invocation,
  in a normal PTY session Chimaera already owns — never on open, never during
  render, never on export, never as a lint side effect. The render path is a pure
  function of files on disk, always; otherwise C1 is a fiction and a page that
  executes a command to draw itself is a security and latency non-starter.
- **Journal loss** — hot and reconstructible; the agent loses recent context,
  nothing durable. Git holds truth.
- **Two windows on one board** — the same per-object merge as §6.6.

## 14. Phasing

1. **Slice 1 — the spine, dogfoodable in a week.** `chimaera-board` crate:
   schema, `normalize`, lenient parse, the raster path (usvg/resvg/cosmic-text/
   png), bundled fonts. CLI `new`, `render`, `describe`, `lint`, `journal`.
   `BoardView` with the stage, select/move/resize/snap, outline rail, numeric
   inspector, page navigator. One theme (`talk-dark`) + PNG export. The journal
   writer **and gesture emission**. The board skill (claude + codex bridge).
   Plus the two daemon routes this slice's own content already requires:
   `POST /api/v1/board/render` → a `/raw` ticket (§7 — the stage shows
   server-rendered rasters, so without it the pane has *no pixel source*) and the
   journal-append route (§6.3 — one writer per file means the CLI's append routes
   through the daemon). Both are thin wrappers over the same crate functions the
   CLI calls, and the ticket-serving half already ships. Live agent-edit refresh
   in slice 1 is the plain in-place `fileStore` revalidation on the existing 2 s
   poll; slice 2 adds the epoch's faster invalidation and the tween/attribution
   on top.
   **The day-one dogfood, which is the whole point of the feature:** an agent
   authors a two-slide deck from the skill, renders it, you drag a box, the agent
   reads `describe` + `journal --since` and adjusts. Rust unit tests on the
   schema, normalize, and lint (the web UI has no component tests — the isolated
   preview is its net).
   Also in slice 1, because all three are expensive to retrofit and cheap now:
   the **anchor union in the schema** (`at` + `rel` resolving), the `alt`/`link`
   fields carried through, `page.layout` + named slots, `page.caption`, and the
   `Composite` trait shipped with exactly one implementation (`table`, rendered
   but not yet exported) so the mechanism cannot be retrofitted badly. And the
   **export-tier machinery** (`tier()`, the reason strings, `exportFloor` on
   presets, the census in `lint --target` and `describe`) — **with the §3.6/§3.8
   vocabulary completely unchanged.** *Ship the door, not the rooms:* the
   machinery is cheap now and expensive to retrofit (it re-keys the preflight,
   `describe`, lint output, and the skill — the same argument that lands anchors
   in slice 1), while rich elements are the reverse: cheap to add, impossible to
   remove once boards in the wild use them. The first rich element is admitted
   when a concrete figure is demonstrably blocked, not when expressiveness is
   argued in the abstract.
2. **Slice 2 — the loop's polish.** The remaining daemon routes (`describe`,
   `export`) + board epoch; live agent-edit animation with attribution and
   narration; selection-as-deixis and region snapshots into chat; comments and
   pins; per-object conflict merge; actor-aware undo — extended to derived
   children and `object.detached`. `board arrange` verbs; `lint --fix` for the
   mechanically repairable classes (off-canvas, collision, sub-minimum font).
3. **Slice 3 — exports.** Pure-Rust PPTX + krilla PDF + SVG (both variants) +
   JPEG; composite expansion → grouped `p:sp`/`p:cxnSp`; **`table` → native
   `a:tbl`**; `alt` and `link`; notes + `notesMaster`; the **fidelity preflight
   UI**; `equation` v1 (the picture arm, nearly free once the raster path exists);
   the python-pptx CI oracle and the cross-app fidelity matrix; target presets;
   `lint --target` gating.
4. **Slice 4 — figures, the wedge.** matplotlib/ggplot import, provenance, stale
   badges, `theme-export` to `.mplstyle`/ggtheme, `rescheme`; Okabe-Ito/Tol/Viridis
   themes; journal width presets; poster preset. Then the annotation layer —
   `image.pixelSize`, `panelLabel`, `scalebar`, `inset`, `sigBracket`, `legend`,
   `colorbar`, `callout`; `anchor.px`; `theme-export` emitting a hashed `frame`
   which unlocks `anchor.data`; cross-panel consistency lints; align-plot-areas;
   the CVD/grayscale preflight. **The annotation layer comes before `chart`
   within this slice** — it is the actual wedge.
5. **Slice 4b — `chart`.** Eight marks, file-referenced data, digest staleness,
   tick formatting, bundled colormaps, the mark-count ceiling. Gated on `table`
   landing, since both take the grouped-shape export path.
6. **Slice 5 — decks, diagrams, and parity.** Present mode with presenter view;
   inline board artifacts and snapshot attachments; cosmic-text-wasm for in-place
   text editing; `diagram` with vendored `dagre-rs` + orthogonal routing;
   `board import mermaid`; icons and `tint`; caption-integrity lint; `svgBlip`
   progressive enhancement; `hayro` PDF-panel import behind a feature flag.
7. **Slice 6 — opportunistic, gated on the fidelity matrix.** Native `c:chart` as
   an exporter optimization; the OMML arm of `equation`.

Every slice is independently shippable and live-verified per **verify-app**;
anything touching chat is gated by `just chat-smoke`. A `feat:` carries its
feature-catalog page ([document-feature](../.claude/skills/document-feature/SKILL.md))
and an intent capture.

## 15. Open questions **[decide]**

- **The workspace dotdir** (§4) — `.chimaera/board/` (one namespace, echoes the
  daemon home) vs `.chimaera-board/` (unambiguous, proliferates).
- **Figures vs decks first.** Slice 1 ships a deck because 16:9 is one fixed
  canvas and the schema is simplest there, but the *wedge* — the thing nobody
  else does — is figure assembly. Flipping slices 3 and 4 would put the
  differentiated use case in your hands two slices earlier at the cost of
  shipping PPTX later.
- **How much of the plugin story to honor now.** Recommendation in the earlier
  brainstorm stands: ship this as an optional built-in surface (a setting hides
  it), publish the *format* spec in an outside repo if you want an ecosystem,
  and defer a general UI-plugin system until three things need it. Worth an
  explicit yes/no, since it was your original framing.
- **One 60-second hand-check, if you want it.** The SVG-editability question was
  settled on the Mac gap, the manual ritual, and Google Slides — none of which
  depend on the text outcome — so nothing in the plan hinges on this. But if
  you're curious: on a Windows M365 box, insert a matplotlib figure saved with
  `svg.fonttype:'none'`, right-click → Graphics Format → Convert to Shape →
  ungroup twice, and click an axis label. Editable text, per-letter shapes, or
  nothing? The negative control is the same figure at matplotlib's *default*
  `svg.fonttype:'path'`, which contains **zero `<text>` elements** (measured: 0
  `<text>`, 53 `<use>` glyph refs) and should yield ~53 letter-shaped freeforms —
  confirming that "text vanished" reports are the input SVG's fault, not
  PowerPoint's. On your Mac the button should simply be absent.
- **Mermaid, decided but worth your eye.** `board import mermaid` converts once
  into a `diagram` spec (mermaid text kept in provenance only) rather than storing
  mermaid or rendering it client-side — a second stored representation is the
  anti-pattern §3.2 already rejects, and mermaid.js in the browser breaks §7's
  parity invariant and cannot run from a login node. If you'd rather agents just
  write mermaid and see it, that's a different product and worth saying now.
- **User-scope themes** (`~/.chimaera/board/themes/`) so a lab's house style
  follows you across projects on a shared HPC home — powerful, but it weakens
  the "the repo contains everything needed to rebuild the figure" story.

## Appendix: what we deliberately do NOT build

**The escape hatch, stated once, for everything in this list:** generate it
upstream, import it as an `image` with provenance, and annotate it with
composites. That keeps the regenerate path, the anchors, the theme-export loop,
and the honest "cannot verify this panel" lint. Board never says *you can't* — it
says *not natively, and here is where it lives instead.*

- **A drawing tool.** No pen, bezier, boolean ops, gradient mesh, blend modes,
  or animation. The object set is text, shape, image, connector, group.
- **All statistics** — fits, LOESS, KDE where the density isn't supplied,
  histogram binning, clustering and linkage, survival estimation, dendrogram leaf
  order, multiple-testing correction. Their *outputs* are native; the computation
  never is. A wrong mean in a layout tool is invisible in a way a wrong font size
  is not.
- **Aggregation and downsampling of any kind.** An aggregation engine is a
  statistics engine, and an agent will reach for it.
- **Vega-Lite-style `transform` blocks and faceting.** Small multiples are N
  charts placed by `arrange`.
- **SmartArt** — named explicitly, because "why not SmartArt for the workflow
  diagram" will be asked on every deck. PowerPoint runs its layout program on
  open, so Board would be guessing at its own export, and placeholder inheritance
  silently reopens the `normAutofit` hole §3.5 closes. A grouped-shape `diagram`
  degrades strictly better and stays yours.
- **`cx:` chartex** (waterfall, treemap, boxWhisker, histogram, sunburst, funnel)
  — a Microsoft extension outside ECMA-376, an entire second writer, and only
  recently supported by LibreOffice. Box-and-whisker is exactly where an imported
  seaborn panel is better anyway. Likewise `c:stockChart` and `c:surfaceChart`,
  standardized since 2006 and implemented by essentially nobody.
- **`a:custGeom` projection of *imported* SVG** — but not for the reason first
  assumed. Path-count explosion is the wrong model: a 40,000-segment path is
  362 KB and renders fine, and what actually explodes is *object* count (a
  5,000-point scatter becomes 5,000 shapes), which C3's export bound already
  governs. The real reason is that projecting an imported panel to geometry
  **defeats the panel lint that reads the SVG's own `<text>` and `stroke-width`**
  — the mechanism that makes §3.5's lint-through-panels honest. Board's *own*
  shapes emit custGeom freely (§3.6).
- **Force-directed layout** — iterative and seeded, so it fails C1 and fights
  byte-stable serialization.
- **Axis breaks, and any redrawing or annotating over an imported panel's axes.**
  Implying a scale the data does not have is adjacent to falsification. Board may
  warn when an object overlaps a panel's plot area; it may never draw over it.
- **Redrawing flow-cytometry gates** — the gate polygon produces the percentages.
- **Domain renderers** — genome tracks, sequence logos, alignments, structures,
  phylogenies, pathway and chemical structures. Newick is the most tempting
  exception and still fails the ruler test: a tree is a chart plus a layout
  algorithm plus a long tail of support values, branch scaling, circular layouts,
  and clade collapse.
- **Networks, Sankey, alluvial, chord, treemap, sunburst, maps and projections,
  3-D surfaces, polar/radar/ternary** — coordinate systems Board doesn't own.
- **Client-side mermaid.js rendering** — breaks §7's parity invariant and remote
  transparency, and its `<foreignObject>` labels don't survive resvg on export.
- **Storing composite expansions in the file** — the same normalization ambiguity
  and diff churn that rules out stored markdown.
- **Spreadsheet cell-range binding and compute-on-open data sources** — a range is
  not a data contract, and executing a command to render a page is a security and
  latency non-starter.
- **Cross-board transclusion.** A panel appearing in both `figures/fig2.board.json`
  and `talks/lab-meeting.board.json` is the most common real workflow here, and
  the answer is that both boards reference the same tracked path under `assets/`
  — so the stale badge fires for both. No cross-board references: they would break
  the per-file merge model and the "one file is the figure" story.
- **Media and `p:oleObj`** — no figure use case, and one dropped screen recording
  blows the ~150 MB daemon budget.
- **DOM `measureText` as layout truth.** The stage shows the engine's pixels
  (§7); a browser-measured shortcut would permanently split editor and export.
- **Autofit.** Unrepresentable in the schema by design (§3.5).
- **Markdown as the stored text model.** Authoring sugar only — a second stored
  styling representation makes normalization ambiguous and diffs churn.
- **Inlined theme snapshots in boards.** The theme is a tracked file in the same
  repo; snapshotting buys nothing and churns every diff.
- **Comments and pins in the board file.** Journal-only; conversation must not
  pollute the document's diff.
- **A daemon-side board database.** Files are truth. On a shared NFS home a
  per-host daemon store would show different truth per daemon and none at all to
  a teammate's clone.
- **Shelling out to Python (python-pptx, Inkscape, LibreOffice) at runtime.**
  Breaks the static-binary and remote-transparency invariants; python-pptx is a
  CI oracle only.
- **pdfium or mutool for PDF import.** AGPL contagion and system dependencies;
  `hayro` behind a feature flag is the honest path, and v1 says "export SVG."
- **Global hex find-replace rescheming.** The same hex means different things in
  different parts of a matplotlib SVG; id/role-scoped remap or regenerate.
- **An infinite canvas.** Boards are pages with real physical sizes, because the
  output is a slide or a journal figure with a column width.
