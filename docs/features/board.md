# Board

Chimaera's visual composition surface: agents write ordinary `*.board.json`
files (decks, cards, quick result charts), the daemon renders them
server-side, the BoardView pane shows the pixels and lets the human move
things, and the agent reads the gestures back. The full design — including
everything not yet built — is [docs/board-plan.md](../board-plan.md); this page
covers what is **implemented today** (slice 0 + the slice-1 spine).

**Where it lives.** The engine is its own crate,
`crates/chimaera-board/` (schema, `normalize`, themes, chart, layout/text
measurement, SVG→PNG render, `show`, `describe`, `lint`, and the canonical
byte-stable writer in `pretty.rs`). CLI verbs: `crates/chimaera/src/board.rs`.
Daemon routes: `crates/chimaera-server/src/board.rs`. Pane:
`web-ui/src/lib/previews/BoardView.svelte`, registered as the `board`
`FileViewKind` in `previews/files.ts` (matched on the full `.board.json`
suffix, since `extension()` only sees the last dot segment). Skill:
`.claude/skills/board/SKILL.md` (+ the `.agents/skills/board/` bridge).

## The format (implemented subset)

- `formatVersion: 1`; points only (960×540 for 16:9), origin top-left; ids are
  slugs and double as the diff anchor, Edit anchor, and merge key.
- Five primitives — `text`, `shape`, `connector`, `image` (placeholder box in
  v0), `group` — plus `chart`. Unknown fields round-trip verbatim; an unknown
  or malformed *object* is preserved-but-not-drawn, so a newer board opened by
  an older daemon loses nothing.
- **Byte-stable canonical serialization**: fixed key order, scalar arrays
  inline, container-array elements one per line (z-order and data rows diff as
  line moves), small objects inline under a 100-byte budget, root always
  expanded. A semantically identical save is byte-identical.
- `normalize()` snaps geometry to the 8 pt grid, collapses bare runs, infers
  chart channel types from inline JSON only, and enforces the inline-data caps
  (≤500 rows, ≤32 KiB).
- Themes: bundled `talk-dark` / `talk-light` (`@token` palette, role-based
  type scale with per-role `minPt`, Okabe–Ito categorical ramp, WCAG-checked
  in unit tests). Workspace themes resolve from `.chimaera/board/themes/`.
- Chart v0: marks `bar` (grouped/stacked) · `line` (incl. `step: "post"`) ·
  `point` · `rule` · `errorbar` · `text`; linear/ordinal/temporal scales; d3
  nice ticks with step-derived decimal places; measured gutters; direct labels
  and a series row instead of a legend; a bar axis always includes zero; the
  required `data.origin` chip is drawn on the render. Histogram/pie/second-y
  are refused.

## CLI

`chimaera board show` (spec on stdin → one-page board + PNG under the
self-ignoring `.chimaera/board/shown/`), `new`, `render` (content-addressed
PNGs under `.chimaera/board/renders/`), `describe` (the agent read-back),
`lint`. All verbs are thin wrappers over crate functions; the daemon wraps the
same functions, so the pane and the CLI cannot disagree.

## Daemon routes

All bearer-authed: `POST /api/v1/board/render` (renders content-addressed,
answers with a `/raw` ticket — no image bytes on the JSON wire),
`POST /api/v1/board/describe`, and `POST /api/v1/board/edit` (one semantic
move/resize gesture by object id; normalizes and saves canonically, returns
the new `X-Mtime` token). Render/describe/edit run under the shared
filesystem blocking semaphore. `.chimaera/board/{renders,exports,journal,shown}`
are excluded from the quick-open walker **by parent path** so a user's real
`exports/` elsewhere stays indexed.

## The pane

Opening any `*.board.json` shows the stage (server-rendered raster — layout
truth is never re-derived in the DOM), the outline rail, a numeric inspector
(pt, 8 pt steps), and the page navigator. Click selects; drag moves; both the
inspector and drags commit through `/board/edit`. Agent edits to the file
arrive via the fileStore's 2 s disk watch and re-render **in place** with no
flash. Boards past the 256 KB first chunk view fine but lose client-side
selection.

## Not built yet (see the plan)

The journal event stream (gestures currently land only as file changes),
slots/layouts and `brief`/`intent` resolution (parsed and preserved, not
resolved), anchors resolution, image/SVG placement, exports (PPTX/PDF/SVG),
`board adopt`, the chat `ShownCard`, `lint --style`, and the figures pack.

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
