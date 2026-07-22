# chimaera-board — map

The board engine: the `.board.json` format and everything that turns one into
pixels. Pure library — no daemon, no async, no network. The CLI
(`crates/chimaera/src/board.rs`) and the daemon routes
(`crates/chimaera-server/src/board.rs`) are both thin wrappers over these
functions; that single-engine property is what keeps the pane, the CLI and
(later) the exporter agreeing pixel-for-pixel. Design source of truth:
[docs/board-plan.md](../../docs/board-plan.md); current-state feature page:
[docs/features/board.md](../../docs/features/board.md).

| File | What it is |
|---|---|
| `src/lib.rs` | parse/save, `is_board_path`, the workspace surround (`.chimaera/board/`, the self-ignoring `shown/`) |
| `src/schema.rs` | the format: 5 primitives + `table` + `chart` + `diagram` + `equation` + 7 annotation composites, lenient `Object` deserialize (unknown/malformed → preserved `Unknown`); table cells reuse the `Paragraph` text model; `equation` requires `alt` (the C6 carve-out) so an alt-less one parse-fails into `Unknown` |
| `src/pretty.rs` | the canonical byte-stable JSON layout — the exact bytes are part of the format |
| `src/normalize.rs` | sugar expansion + the constraints that make ugly unrepresentable; pure and idempotent |
| `src/merge.rs` | the per-object three-way merge on slug ids (git-merge-driver semantics: ours-wins on true conflicts + a `Conflict` report; page membership follows the mover; canonical byte-stable output; never journals) |
| `src/theme.rs` | `@token` palettes, role type scale with per-role `minPt`, bundled `themes/*.theme.json` |
| `src/chart.rs` | marks over a plot-ready table → flat draw items; scales, d3 nice ticks, measured gutters |
| `src/diagram.rs` | the `diagram` composite: deterministic layered layout (Sugiyama-lite, in-crate — no maintained dagre exists) expanding to primitives at render; the mermaid flowchart import |
| `src/equation.rs` | the `equation` picture arm: LaTeX → glyph-outline SVG via mathtex (pure-Rust xetex core, formatless ini-xetex + curated prelude + bundled STIX Two Math under `fonts/`), behind the default-on `math` feature; ceilings and the missing-feature refusal compile in every build; the OMML arm is deliberately not built |
| `src/composites.rs` | the annotation layer: `panelLabel`, `scalebar`, `sigBracket`, `legend`, `colorbar`, `callout`, `inset` — each expands to primitives at render exactly like `diagram`, children id'd `<composite-id>/<part>` |
| `src/layout.rs` | text measurement/wrapping over usvg's own `fontdb` + rustybuzz |
| `src/render.rs` | scene graph → SVG (self-emitted, always escaped) → PNG/JPEG via resvg |
| `src/show.rs` | the one-shot `board show` spec → one-page board (never a second schema) |
| `src/describe.rs` | the agent-facing read-back (+ the one-line journal summary) |
| `src/journal.rs` | the semantic edit journal: seq-first append-only JSONL per board under `.chimaera/board/journal/`, no wall clock, size-capped with seq-preserving compaction (unresolved `comment` pins survive the cap; `comment`/`comment-resolved` are the §6.4 pin vocabulary — journal-only, never the board file) |
| `src/lint.rs` | the legality, target and style profiles + `lint_fix`; findings always name object, field, and the numbers |
| `src/arrange.rs` | align/distribute/grid over named objects, one pure function; refuses slot-placed targets (their geometry is derived) |
| `src/cvd.rs` | the CVD preflight: Machado 2009 simulation, CIE76 ΔE, all-pairs palette check, the computed safe series cap, `validate_theme` |
| `src/pdfimport.rs` | one PDF page → PNG via hayro, behind the non-default `pdf-import` feature (binary weight); sniff, ceilings and the missing-feature refusal compile in every build |

## Invariants that bite

- **Byte stability is a format property.** A semantically identical save is
  byte-identical; `normalize()` is a fixed point. Tests pin both — do not
  "improve" the formatting without accepting that every board rewrites.
- **One text stack.** Measurement uses the same `fontdb`/rustybuzz that usvg
  renders with. A second measurement path (DOM, another shaper) will drift
  from the renderer and break layout invisibly.
- **Board draws numbers you state; it never derives numbers.** No binning, no
  quartiles, no aggregation, no downsampling. Refusals are loud and name the
  fix.
- **Ids are sacred.** They are simultaneously the diff anchor, the agent's
  Edit anchor, the journal subject, and the merge key. Duplicate ids are an
  error, never an auto-rename.
- **Renders are pure.** PNG = f(board bytes, theme, params); the
  content-addressed cache is correct by construction and never needs
  invalidation. Keep `Date`-like nondeterminism out of the render path.
- **The 12 Mpx raster ceiling refuses rather than allocating** — the daemon
  runs on shared login nodes.
