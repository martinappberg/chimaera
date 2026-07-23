---
name: board
description: Compose and edit .board.json visual surfaces (decks, cards, quick result charts) with the chimaera board CLI — show a result mid-work, author a deck, read back positions after the human moves things, and keep the loop honest. Use when the user asks for a slide, a figure, a chart of results, or when a picture beats a paragraph.
---

# Board — compose, render, read back

A board is an ordinary `*.board.json` file anywhere in the workspace. You write
it, `chimaera board render` draws it, the human nudges objects in the BoardView
pane, and `chimaera board describe` tells you what they did. The file is the
single source of truth — there is no hidden state.

## Show a result mid-work (the most common move)

When a picture beats a paragraph — test failures, a benchmark, a before/after —
pipe a spec to `show`. One tool call, no file to name:

```sh
chimaera board show <<'JSON'
{ "title": "Test failures by file", "note": "after the parser rewrite; 3 runs",
  "chart": { "x": "file", "y": "failures",
             "values": [ {"file": "parser.rs", "failures": 12},
                         {"file": "lexer.rs", "failures": 3} ] } }
JSON
```

It prints `shown chart · N rows · theme · WxH → path`; tell the user the path
(the PNG sits beside it). Facts that matter:

- The spec takes exactly one of `chart`, `table` (`{columns, rows}`), or
  `text` (a list of lines). `title` and `note` are optional.
- Channels may be bare strings (`"x": "file"`); types are inferred from the
  inline JSON. Nominal axes sort descending by value; >7 categories or a
  label over 12 characters flips to horizontal bars. All of this is
  `show`-only sugar — real boards state everything.
- **`data.origin` defaults to `command`.** If the numbers did NOT come from
  something you ran, say so: `"data": {"origin": "stated-by-user", ...}` or
  `"derived-by-agent"`. The chip is drawn on the card; a confident chart of
  inferred numbers is the one way this feature does harm.
- Everything lands under `.chimaera/board/shown/`, which ignores itself —
  a throwaway never dirties `git status`.
- `--id SLUG` is the update handle: re-invoking with the same id overwrites
  the same card instead of minting forty across a sweep.

## Author a board

`chimaera board new talks/lab-meeting.board.json --title "..."` then edit the
JSON. The format:

- **Points only.** A 16:9 slide is 960×540 pt, origin top-left. Positions and
  sizes snap to an 8 pt grid on save.
- **Five primitives** — `text`, `shape`, `connector`, `image`, `group` — plus
  `chart`. Ids are short slugs, unique per board; they are your Edit anchor
  and the merge key, so never rename one casually.
- **Roles, not font sizes.** `"role": "title" | "heading" | "subtitle" |
  "body" | "caption" | "label" | "code"` — the theme resolves family, size,
  weight, color. There is no fontSize field. Colors are `"@tokens"`
  (`@fg @body @muted @accent1 @surface @edge @bg @cat1..7`) or `#hex`
  literals; unknown tokens are lint errors that list the palette.
- **Text** is a list of paragraphs: bare strings, or
  `{"runs": [{"t": "...", "b": true, "color": "@accent1"}]}` for styling.
- **Connectors bind by object edge**: `"from": {"object": "callout",
  "side": "left"}` — the label and the routing survive when things move.
  `"tailEnd": "arrow"` puts the head at the `to` end.
- **Chart data is stated, never derived.** Inline `values` (≤500 rows,
  ≤32 KiB), declared `origin`, marks from `bar line point rule errorbar
  text`. No histogram (binning is analysis — do it upstream and state the
  bins), no pie, no second y axis.

After writing: `chimaera board lint <path>` (errors block; every finding names
object, field, and the numbers) and `chimaera board render <path>` (PNGs land
content-addressed under `.chimaera/board/renders/`; read the printed path to
look at your own work — you cannot judge a layout you have not seen).

## Read back what the human did

```sh
chimaera board describe talks/lab-meeting.board.json
```

prints every object with its position in the same points you write:
`callout shape/roundRect at [520, 360] size [272, 96]: 3.3× median`. Run it
**before** editing a board the human may have touched — their drags landed in
the file, and clobbering a moved object with your remembered coordinates
undoes their work. The file is canonical byte-stable JSON, so `git diff` shows
gestures as clean per-line changes.

## The full verb set

Beyond show/new/render/describe/lint: `journal [--since N]` (read the human's
gestures back — ALWAYS check before editing a board a human may have
touched); `lint --target <preset>` (floors + tier census; presets:
talk-16x9, design-review, exec-update, teaching, readme-image, poster-a0,
pub-nature-single, pub-cell, pub-plos), `--style` (near-miss alignment etc.),
`--fix` (mechanical repairs); `arrange --op align-left|distribute-h|grid
--ids a,b,c`; `import <file.mmd|.svg|.png> --to board` (mermaid → native
diagram; figures → assets/ with provenance); `adopt <shown-id> [--to board]`
(promote a shown card); `export --format pptx|pdf|svg|svg-outlined`
(PPTX is natively editable — text stays text); `theme-export --format
mplstyle` (hand your matplotlib script the deck's exact style);
`rescheme <svg> --theme id` (recolor an existing figure); `validate-theme`
(CVD-safe series cap).

Prefer **slots over coordinates**: give a page `intent.kind` (cover ·
section · claim-evidence · comparison · data · quote · summary · backup ·
agenda · demo · roadmap · metrics · architecture · acknowledgements) and put
objects in slots (`title`, `body-left`, …) — the layout engine places them,
and lint stops warning about free geometry. Charts can bind a CSV directly
(`data: {origin: "file", source: "bench.csv", sha256: "..."}`) — a changed
file goes loudly stale rather than silently wrong. Timelines/Gantt: temporal
x + a bar mark with `fields: {"x2": "end"}`. Diagrams: write mermaid and
import it.

## Footguns observed live

- A board path must end in `.board.json` — `board.json` alone is refused.
- The renderer refuses sub-floor text (per-role `minPt`) — it clamps up and
  reports; fix the role, don't fight the floor.
- Unknown `geo` names draw a dashed placeholder box and say so. v0 geometries:
  `rect roundRect ellipse line triangle diamond path` (+`d` for path).
- An unknown object `type` is preserved but not drawn — older builds opening
  newer boards lose nothing.
- Don't hand-format the JSON prettily; any save rewrites it into canonical
  form anyway. Semantically identical saves are byte-identical.
