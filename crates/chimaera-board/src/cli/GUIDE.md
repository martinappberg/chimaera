# chimaera board — the complete manual

Boards are plain `*.board.json` files; `chimaera board` renders them to pixels.
This page is self-contained: run the examples as-is, and never explore
`--help`, the source, or the repo to learn the tool. One law up front:
**Board computes scales and layout, never statistics** — no binning, no
aggregation, no regression; compute quartiles/CIs yourself and pass them.

## Show a result inline (the main move)

Pipe a spec to `board show`; the printed `shown … → path` line mounts the card
inline in chat. The spec is `title`, `note`, and exactly ONE of `chart`,
`table`, `text`, or `mermaid`:

    echo '{"title":"Failures by file","chart":{"x":"file","y":"n",
      "values":[{"file":"parser.rs","n":12},{"file":"lexer.rs","n":3}]}}' \
      | chimaera board show --id failures

- Channels may be bare strings (`"x":"file"`); types are inferred from inline
  JSON; the mark is inferred (nominal×quantitative→bar, ordered→line,
  point otherwise). Nominal axes sort descending; >7 categories or long
  labels flip to horizontal bars — this sugar applies ONLY when you state no
  `marks`.
- `--id SLUG` is the update handle: re-running with the same id updates the
  same card in place (sweeps, progress). `--preset default|wide|square|tall`
  or `--size WxH`; `--theme auto|talk-dark|talk-light|figure-light` (`auto`,
  the default, follows the viewer's light/dark mode).

**The full chart vocabulary passes through** — state `marks` (or `mark`) and
you own the chart: marks `bar line point area rect tick rule errorbar text
box`, per-mark `fields` overrides (`x2`/`y2` intervals, `lo`/`hi` asymmetric
errorbars, `y2` area ribbons), a `color` channel for series, `axes`, `sort`,
and per-channel `type`/`scale` (`linear|log|ordinal|temporal`). A boxplot
takes ONE call — pass a precomputed five-number summary per row
(`lo q1 med q3 hi`, or map names via `fields`):

    echo '{"title":"Latency by day","chart":{"x":"day","y":"med","mark":"box",
      "trace":"five-number summary via numpy.percentile over latency_ms",
      "inputs":["results/latency.csv"],
      "values":[{"day":"Mon","lo":11,"q1":14,"med":16,"q3":19,"hi":24},
                {"day":"Tue","lo":10,"q1":13,"med":15,"q3":17,"hi":22}]}}' \
      | chimaera board show --id latency

Tables and text: `"table":{"columns":["file","n"],"rows":[{"file":"a","n":1}]}`
· `"text":["line one","line two"]`. Diagrams: pipe mermaid flowchart source to
`chimaera board show --mermaid` (converted to a native diagram object; the
card auto-sizes to the flowchart unless `--size`/`--preset` says otherwise).

## Data provenance — favor real files, leave a trace

`data.origin` is required (`show` defaults it to `command`): `file` ·
`command` · `stated-by-user` · `derived-by-agent` — say the true one; it is
drawn on the card. Rules:

- **Favor binding real project files over inlining**: `"data":{"origin":
  "file","source":"bench.csv","sha256":"…"}` (workspace-relative CSV/TSV; a
  changed file goes loudly stale instead of silently wrong).
- **When you computed the values** (quartiles, means, aggregations), record
  `data.trace` — method, command, seed — and `data.inputs`, the files read
  (in `show`'s chart sugar, top-level `trace`/`inputs` as above). A later
  session must be able to answer "how was this calculated" from the file
  alone; `board describe` prints it.
- **Invented demo data must say so in the trace.**

## Card an existing board

`chimaera board show --file path/to/deck.board.json` — no spec, no copy:
validates the file, renders its first page to a PNG beside it, and prints the
shown line for THAT path so the chat card mounts. Use it after hand-writing
or editing a board; plain `render` does not surface a card.

## Hand-written boards (decks, figures)

`chimaera board new boards/name.board.json --title "…"`, then edit the JSON:
`pages[].objects[]`, each with a unique slug `id` (the Edit/diff/journal
anchor — never rename casually). Positions in points (16:9 = 960×540,
origin top-left, 8 pt grid snap on save). Object types: `text` `shape`
`connector` `image` `group` `table` `chart` `diagram` `equation` `icon` (+
annotations `panelLabel` `scalebar` `sigBracket` `legend` `colorbar`
`callout` `inset`). Text carries roles, not font sizes (`"role":"title" |
heading | subtitle | body | caption | label | code`); colors are theme
`@tokens` (`@fg @body @muted @accent1 @surface @edge @bg @cat1..7`) or
`#hex`. Prefer `page.intent.kind` + slots over coordinates — the layout
engine places slotted objects. Themes: `auto` (default — resolves to the
viewer's light/dark mode at render time), `talk-dark`, `talk-light`,
`figure-light`; pin one for a fixed ground. `canvas.background` (an `@token`
or `#hex`) repaints the ground under every page; a page's own
`background.fill` wins over it. After writing: `lint` then `render` (or
`show --file`), and LOOK at the PNG.

## Icons & rich composition

Board is a composition tool, not just a diagram builder: bundled **icons**,
imported **SVG/PNG** figures, and native shapes combine into real artwork, all
editable after a PPTX export. Find an icon name in one call —
`chimaera board icons flask`, `chimaera board icons arrow` (fuzzy over names +
synonyms; `--list` prints the total) — then place it:

    {"type":"icon","name":"flask","at":[80,80],"size":[48,48],"color":"@accent1"}

`color` is any `@token` or `#hex` (default `@fg`), so a recolor is a token
swap; `strokeWidth` (default 2, Tabler's scale) thickens or thins the line; a
resize is free. A **diagram node** takes a leading icon the same way —
`{"id":"train","label":"Train","icon":"flask"}` — sized to the node and laid
out beside the label; that alone lifts a plain flow out of "too boring". An
unknown name renders a visible placeholder and lints, never a silent blank.
Compose icons with `import`ed `.svg`/`.png` and shapes for figures you keep
editing, then `export --format pptx` to hand off for polishing in PowerPoint.

## Designed figures — architecture & flow diagrams

A **designed figure** is native objects you place by hand — background shapes,
icon-in-box nodes, connectors — to compose an architecture or pipeline diagram
that looks deliberate and stays editable (every object survives a PPTX export
for polishing in PowerPoint). It is a different move from `board show`'s
auto-charts and from `--mermaid`'s auto-layout: here YOU own the positions.
Which tool for the picture in front of you:

- **A quick result** (numbers, a comparison) → `board show` chart/table/text
  sugar — auto-mark, auto-layout, one pipe.
- **A quick flowchart** you don't need to hand-place → `--mermaid` (auto-laid).
- **A designed figure** (architecture, model, pipeline) → native shapes +
  icons + connectors, composed by hand. This is the editable, PPTX-editable
  path — the "drop it into PowerPoint and polish" hand-off.
- **Finished external art** → `import` an `.svg`/`.png`. It rides along as a
  picture, NOT as editable objects. So do **not** hand-author an SVG and
  `import` it when the user may want to edit the figure — compose native
  objects instead; every one stays movable and re-colorable.

Complete example — a two-lane architecture figure. Copy it, rename the ids,
change the text/positions; `chimaera board show --file
boards/chrombpnet.board.json` cards it (write it under `boards/` only for a
deliverable the user asked to keep — otherwise any path works):

```json
{
  "format": "chimaera.board",
  "formatVersion": 1,
  "title": "ChromBPNet architecture",
  "canvas": { "size": [960, 540] },
  "pages": [
    {
      "id": "page-1",
      "objects": [
        { "id": "title", "type": "text", "role": "title", "at": [56, 40], "size": [848, 48],
          "text": "ChromBPNet separates bias from signal" },
        { "id": "subtitle", "type": "text", "role": "subtitle", "at": [56, 92], "size": [848, 34],
          "text": "Two branches predict profile shape and total accessibility, then recombine." },

        { "id": "lane-bias", "type": "shape", "geo": "roundRect", "at": [212, 140], "size": [560, 120],
          "radius": 14, "fill": "@surface" },
        { "id": "lane-bias-accent", "type": "shape", "geo": "rect", "at": [212, 140], "size": [5, 120],
          "fill": "@cat2" },
        { "id": "lane-bias-label", "type": "text", "role": "label", "at": [230, 150], "size": [520, 16],
          "align": "left", "text": "ASSAY BIAS · FROZEN" },

        { "id": "lane-signal", "type": "shape", "geo": "roundRect", "at": [212, 290], "size": [560, 168],
          "radius": 14, "fill": "@surface" },
        { "id": "lane-signal-accent", "type": "shape", "geo": "rect", "at": [212, 290], "size": [5, 168],
          "fill": "@cat1" },
        { "id": "lane-signal-label", "type": "text", "role": "label", "at": [230, 300], "size": [520, 16],
          "align": "left", "text": "REGULATORY SIGNAL · TRAINABLE" },

        { "id": "dna", "type": "shape", "geo": "roundRect", "at": [56, 258], "size": [140, 44],
          "radius": 8, "fill": "@edge", "stroke": { "color": "@axis", "width": 1 } },
        { "id": "dna-icon", "type": "icon", "name": "dna", "at": [66, 267], "size": [24, 24], "color": "@fg" },
        { "id": "dna-label", "type": "text", "role": "label", "at": [98, 258], "size": [92, 44],
          "valign": "middle", "align": "left", "text": "DNA · 2,114 bp" },

        { "id": "frozen", "type": "shape", "geo": "roundRect", "at": [238, 186], "size": [150, 44],
          "radius": 8, "fill": "@edge", "stroke": { "color": "@axis", "width": 1 } },
        { "id": "frozen-icon", "type": "icon", "name": "snowflake", "at": [248, 195], "size": [24, 24], "color": "@fg" },
        { "id": "frozen-label", "type": "text", "role": "label", "at": [280, 186], "size": [100, 44],
          "valign": "middle", "align": "left", "text": "Frozen bias" },

        { "id": "conv", "type": "shape", "geo": "roundRect", "at": [238, 330], "size": [150, 44],
          "radius": 8, "fill": "@edge", "stroke": { "color": "@axis", "width": 1 } },
        { "id": "conv-icon", "type": "icon", "name": "filter", "at": [248, 339], "size": [24, 24], "color": "@fg" },
        { "id": "conv-label", "type": "text", "role": "label", "at": [280, 330], "size": [100, 44],
          "valign": "middle", "align": "left", "text": "Conv1D · k=21" },

        { "id": "dilated", "type": "shape", "geo": "roundRect", "at": [238, 392], "size": [150, 44],
          "radius": 8, "fill": "@edge", "stroke": { "color": "@axis", "width": 1 } },
        { "id": "dilated-icon", "type": "icon", "name": "stack-2", "at": [248, 401], "size": [24, 24], "color": "@fg" },
        { "id": "dilated-label", "type": "text", "role": "label", "at": [280, 392], "size": [100, 44],
          "valign": "middle", "align": "left", "text": "Dilated stack" },

        { "id": "fuse", "type": "shape", "geo": "ellipse", "at": [582, 254], "size": [172, 64],
          "fill": "@edge", "stroke": { "color": "@axis", "width": 1 } },
        { "id": "fuse-icon", "type": "icon", "name": "math-function", "at": [600, 270], "size": [28, 28], "color": "@fg" },
        { "id": "fuse-label", "type": "text", "role": "label", "at": [632, 254], "size": [112, 64],
          "valign": "middle", "align": "left", "text": "Merge heads" },

        { "id": "c-dna-frozen", "type": "connector", "geo": "bent",
          "from": { "object": "dna", "side": "right" }, "to": { "object": "frozen", "side": "left" },
          "stroke": { "color": "@axis", "width": 1.5 }, "tailEnd": "arrow" },
        { "id": "c-dna-conv", "type": "connector", "geo": "bent",
          "from": { "object": "dna", "side": "right" }, "to": { "object": "conv", "side": "left" },
          "stroke": { "color": "@axis", "width": 1.5 }, "tailEnd": "arrow" },
        { "id": "c-frozen-fuse", "type": "connector", "geo": "bent",
          "from": { "object": "frozen", "side": "right" }, "to": { "object": "fuse", "side": "left" },
          "stroke": { "color": "@axis", "width": 1.5 }, "tailEnd": "arrow" },
        { "id": "c-conv-dilated", "type": "connector", "geo": "bent",
          "from": { "object": "conv", "side": "bottom" }, "to": { "object": "dilated", "side": "top" },
          "stroke": { "color": "@axis", "width": 1.5 }, "tailEnd": "arrow" },
        { "id": "c-dilated-fuse", "type": "connector", "geo": "bent",
          "from": { "object": "dilated", "side": "right" }, "to": { "object": "fuse", "side": "left" },
          "stroke": { "color": "@axis", "width": 1.5 }, "tailEnd": "arrow" },

        { "id": "caption", "type": "text", "role": "caption", "at": [56, 500], "size": [848, 24],
          "text": "Profile merge: add logits · Count merge: LogSumExp · Output: central 1,000 bp" }
      ]
    }
  ]
}
```

The controls that example uses — the ones worth knowing before you compose:

- **Lanes** (the swimlane pattern): a rounded-rect `shape` (`geo:"roundRect"`,
  `fill:"@surface"`) as the background, a thin full-height `rect` in an accent
  token (`@cat1`/`@cat2`) laid over its left edge, and a `label`-role `text` at
  the top-left. Members are ordinary nodes positioned inside; the lane is just
  drawn behind them (objects earlier in the list paint first). Reads cleaner
  than a mermaid swimlane, and every piece stays movable.
- **Icon-in-box nodes**: three objects sharing a spot — a `shape` box, an
  `icon` (`{"type":"icon","name":"snowflake"}`; find a name with `chimaera
  board icons <query>`), and a `text` with `valign:"middle"`. `text` accepts a
  **bare string** (`"text":"Conv1D"`), an **array** of lines
  (`"text":["a","b"]`), OR **rich runs** (`"text":{"runs":[{"t":"x","b":true}]}`)
  — all three are valid anywhere text appears (labels, shape text, connector
  labels).
- **Connectors** bind endpoints by box edge — `"from":{"object":"conv",
  "side":"bottom"}`, `"to":{"object":"dilated","side":"top"}` — so the line
  re-routes when a node moves. Routing lives in `geo`: `"bent"` is a rounded
  orthogonal route (the architecture look), `"straight"` a direct line. Omit
  `geo` for the **smart default** — two object-anchored ends auto-route `bent`
  while a free `at` endpoint stays `straight`. `side` is
  `top|right|bottom|left|center`
  and chooses which edge the line leaves and enters. When the auto-route isn't
  the path you want, list explicit `waypoints` (`[[x,y],…]` in page points) it
  threads. Arrowheads: `"tailEnd":"arrow"` marks the `to` end (the usual
  direction), `"headEnd":"arrow"` the `from` end. An edge label is bound `text`
  on the connector, positioned by `labelAt` (0..1 along the path, default 0.5).
- **Explicit `size`**: `shape`, `text`, `icon`, and a diagram `node` all take
  `"size":[w,h]` in points with `"at":[x,y]` as the top-left — pin it to make
  uniform boxes and reserve exact space instead of letting content measure the
  box. After writing: `lint`, then `show --file`, and LOOK at the PNG.

## The other verbs, one line each

- `describe FILE` — read back every object, position, and chart provenance;
  run it before editing a board the human may have moved things on.
- `lint FILE [--target talk-16x9|pub-nature-single|…] [--style] [--fix]` —
  legality + measured near-miss findings; errors name object, field, numbers.
- `render FILE [--page N] [-o OUT]` — PNGs (no chat card).
- `export FILE --format pptx|pdf|svg|svg-outlined` — PPTX keeps text
  editable.
- `adopt SHOWN_ID [--to board]` — promote a shown card into the workspace.
- `import fig.svg|.png|.mmd --to FILE` — figures/mermaid into a board.
- `icons [QUERY] [--list]` — find bundled icons by name/synonym in one call.
- `journal FILE [--since N]` — what the human changed on the surface.
- `arrange FILE --op align-left|distribute-h|grid --ids a,b,c` — tidy by id.
- `theme-export ID --format mplstyle|json` — theme numbers for matplotlib.

## Where boards live

Everything `show` writes lands under `.chimaera/board/shown/`
(self-gitignored) — exploratory results never dirty the repo, and re-shows
overwrite by id. Create a persistent board ONLY when the user explicitly asks
for a deliverable (a deck, a figure to keep): put it at
`./boards/<name>.board.json` (create the directory; it is an ordinary tracked
file) and keep editing that file in place.
