# chimaera board — the complete manual

Boards are plain `*.board` files (JSON content under a branded extension; the
legacy `*.board.json` still opens); `chimaera board` renders them to pixels.
This page is self-contained: run the examples as-is, and never explore
`--help`, the source, or the repo to learn the tool. Two laws up front:
**Board computes scales and layout, never statistics** — no binning, no
aggregation, no regression; compute quartiles/CIs yourself and pass them. And
**the board file is the deliverable**: it leaves here through `chimaera board
export` and nothing else — never re-render a board through another
presentation tool, another skill, or an HTML→pptx converter (see "Hand it
off").

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
  or `--size WxH`; `--theme auto|talk|figure|talk-dark|talk-light|figure-light|figure-dark`
  (`auto`, the default, matches the app's light/dark automatically; override
  with a scheme `talk`/`figure` — still mode-following — or a concrete variant
  like `talk-dark` to pin the ground).

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

## Choosing the inline format

Don't default to prose or one fixed shape. `chimaera board show --as
auto|chart|table|figure|slide|diagram` picks the card SHAPE; the body
(`chart`/`table`/`text`/`mermaid`) is what you already pass:

- **A quick number comparison** → `--as chart` (the compact 720×450 card; also
  the default for a chart body).
- **Tabular results** (a matrix, a config diff, test rows) → `--as table` — a
  card sized to its rows.
- **A process, architecture, or flow** → `--as diagram` (pipe `--mermaid`
  source for auto-layout, or hand-compose a designed figure — see below).
- **A titled, composed explanation the user might present** → `--as slide` — a
  16:9 (960×540) card; give it a `title` so it reads like a presentation slide.
- **A tall or multi-panel scientific figure** → `--as figure` — a portrait
  560×720 canvas.
- **Not sure / a one-off** → `--as auto` (the default): today's inference — a
  mermaid card fits its flowchart, everything else gets the compact card.

Size for the inline chat column (~600–700 px wide) so it reads cleanly.
Precedence: an explicit `--size WxH` wins over everything, then a named
`--preset` (`wide`/`square`/`tall`), then `--as`.

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

`chimaera board show --file path/to/deck.board` — no spec, no copy:
validates the file, renders its first page to a PNG beside it, and prints the
shown line for THAT path so the chat card mounts. Use it after hand-writing
or editing a board; plain `render` does not surface a card.

## Present evidence, not a pitch

A board reports what you found. Unless the user explicitly asked for a
persuasive or sales deck, check every page against these before you show it:

- **Title the finding, not the claim** — "Median latency fell 42 ms after
  caching", not "Caching is a game changer". A title that could survive the
  numbers changing is a slogan; rewrite it.
- **Say what each number IS**: measured · modelled · simulated · planned. A
  page that mixes them labels which is which (a per-item label, or the
  caption), because a reader cannot tell by looking.
- **The number and its unit beat the adjective** — "3.3× (n = 240)", not
  "dramatically faster". Cut superlatives and momentum words.
- **Name the source** on any data page — `data.origin`/`data.source`, or a
  caption naming the file, cohort, or run — so the reader can check you.

## Hand-written boards (decks, figures)

`chimaera board new boards/name.board --title "…"`, then edit the JSON:
`pages[].objects[]`, each with a unique slug `id` (the Edit/diff/journal
anchor — never rename casually). Positions in points (16:9 = 960×540,
origin top-left, 8 pt grid snap on save). Object types: `text` `shape`
`connector` `image` `group` `table` `chart` `diagram` `equation` `icon` (+
annotations `panelLabel` `scalebar` `sigBracket` `legend` `colorbar`
`callout` `inset`). Text carries roles, not font sizes (`"role":"title" |
heading | subtitle | body | caption | label | code`); colors are theme
`@tokens` (`@fg @body @muted @accent1 @surface @edge @bg @cat1..7`) or
`#hex`. Prefer `page.intent.kind` + slots over coordinates — the layout
engine places slotted objects. Themes: **by default a board matches the
Chimaera app you're viewing it in — light or dark** (`auto`, what `new`/`show`
write; an absent `theme` means the same). Override only deliberately: a
**scheme** (`talk`, `figure`) still follows the app's mode, a concrete variant
(`talk-dark`, `talk-light`, `figure-light`, `figure-dark`) pins the ground, and
`canvas.background` repaints the ground under every page with any `@token` or
`#hex` (a page's own `background.fill` wins over it; a literal ground also
decides the board's appearance, so a `#000000` canvas renders dark-mode ink
even in a light app).

A **custom** theme is a `<id>.theme.json` under `.chimaera/board/themes/`,
named by file stem, and resolution is **workspace-root-relative** — the root
is the board's nearest `.git` ancestor, else the board's own directory. So the
theme file must stay WITH the board, and two moves silently break the render:
carrying the board out from under the workspace that holds its theme, and
putting a `.git` anywhere between the board and that theme (the root moves,
the theme is now outside it). Either way the board refuses with `unknown theme
"<id>"; bundled variants are talk-dark, talk-light, figure-light,
figure-dark`. Hand over the board and its
`.chimaera/board/themes/<id>.theme.json` together, or use a bundled theme.

**Paint with `@tokens`, never `#hex` literals.** A literal color is written
through verbatim, which opts that object out of the theme entirely: it will
not follow light/dark, it survives every restyle, and re-theming means finding
each one by hand. One non-neutral literal accent per page is the budget
`lint --style` enforces (`page carries N non-neutral literal accents …; one
accent is the budget — route the rest through @tokens`). Want a specific
palette? Put it in a theme's `@cat1..7` and reference the tokens — that is one
line to restyle and it tracks the mode. After writing, close the loop below
("Alignment, margins, and the closing lint").

**Fonts** are bundled into the binary (no install, deterministic on a fontless
compute node): themes lead with **Arimo**, metric-compatible with Arial (the
PLOS/Cell figure standard), so a figure is submission-safe on any host;
**Geist** (a brand/slides sans), **IBM Plex Sans** and **JetBrains Mono** (the
`code` role) are baked in as alternates. Font lives on the *theme*, never on an
object: `theme-export <id> --format json >
.chimaera/board/themes/mine.theme.json`, put your face first in each role's
`family` array (the first name that resolves wins; keep the rest as
fallbacks), and set the board's `theme` to `mine`. Any other face works: drop
it in `.chimaera/board/fonts/` (vendored wins over bundled) and name it first.

## Icons & rich composition

Bundled **icons** and native shapes combine into real artwork, not just boxes
and arrows. Find an icon name in one call — `chimaera board icons flask`,
`chimaera board icons arrow` (fuzzy over names + synonyms; `--list` prints the
total) — then place it:

    {"type":"icon","name":"flask","at":[80,80],"size":[48,48],"color":"@accent1"}

`color` is any `@token` or `#hex` (default `@fg`), so a recolor is a token
swap; `strokeWidth` (default 2, Tabler's scale) thickens or thins the line; a
resize is free. A **diagram node** takes a leading icon the same way —
`{"id":"train","label":"Train","icon":"flask"}` — sized to the node and laid
out beside the label; that alone lifts a plain flow out of "too boring". An
unknown name renders a visible placeholder and lints, never a silent blank.

**Structure is always native objects; a raster is decoration.** Anything the
user may want to move, restyle or edit — nodes, lanes, labels, arrows — must be
`shape`/`connector`/`icon`/`text`: only those stay editable in the pane and
export as editable PowerPoint shapes. Search `chimaera board icons <query>`
before reaching for anything generated. A picture from elsewhere (a generated
illustration, a photo, a paper figure) rides along flat — `chimaera board
import art.png --to FILE` copies it into `.chimaera/board/assets/` and places
it — so use one for texture or illustration, never for the diagram itself.

## Designed figures — architecture & flow diagrams

A **designed figure** is native objects you place by hand — background shapes,
icon-in-box nodes, connectors — to compose an architecture or pipeline diagram
that looks deliberate and stays editable. Here YOU own the positions, which is
a different move from `board show`'s auto-charts and `--mermaid`'s auto-layout:

- **A quick result** (numbers, a comparison) → `board show` chart/table/text
  sugar — auto-mark, auto-layout, one pipe.
- **A quick flowchart** you don't need to hand-place → `--mermaid` (auto-laid).
- **A designed figure** (architecture, model, pipeline) → native shapes +
  icons + connectors, composed by hand. Never hand-author an SVG and `import`
  it instead: that hands the user a flat picture of a figure.

### Structure with layers (groups)

Compose a designed figure the way a designer layers it: **wrap each logical
region in a `group`** — a swimlane and the boxes on it, a panel and its parts,
a node's own box + icon + label, a legend cluster — so the region is ONE
navigable, movable layer, not a scatter of loose objects.

    {"id":"bias-lane","type":"group","objects":[ …the lane background shape,
      its accent bar, its title, its node boxes + icons… ]}

A group is a **z-order and selection envelope, not a coordinate system**: its
children keep their page-absolute `at`/`size` (ids, `describe`, journal moves,
off-canvas lint and per-object merge stay identical whether or not an object is
grouped), and the group's own box is just the union of what it holds — so
`{type:"group", id:…, objects:[…]}` is all a group needs; no `at`/`size` of its
own. What you gain: the human selects, moves, hides or restyles the whole
region at once; the pane's outline rail shows the group as a collapsible layer
with its children nested under it; and dragging the region moves everything
together — instead of pulling a lane background out from under the boxes
sitting on it. Connectors and shared input/output nodes that span two regions
stay at the top level and bind to child ids by name — a `connector` reaches a
node inside a group fine, because ids are global.

**Prefer a few well-named groups over a flat list of many objects.** One long
pile of shapes, text and icons is the tell that a figure was built object by
object rather than region by region; group it.

Complete example — a two-lane architecture figure, built **in layers**: two
lane groups (each holding its background, accent, title and node boxes/icons),
the shared input and merge nodes as their own small groups, and the connectors
between node ids at the top level. Every coordinate below is a multiple of 8
and sits inside the talk margin box (x 72…888, y 64…476), so it passes
`lint --style` clean as printed — copy it, rename the ids, change the
text/positions, and keep it linting clean;
`chimaera board show --file boards/chrombpnet.board` cards it (write it under
`boards/` only for a deliverable the user asked to keep — otherwise any path
works):

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
        { "id": "title", "type": "text", "role": "title", "at": [72, 64], "size": [816, 48],
          "text": "ChromBPNet separates bias from signal" },
        { "id": "subtitle", "type": "text", "role": "subtitle", "at": [72, 120], "size": [816, 40],
          "text": "Two branches predict profile shape and read counts, then recombine." },

        { "id": "input", "type": "group", "objects": [
          { "id": "dna", "type": "shape", "geo": "roundRect", "at": [72, 280], "size": [160, 48],
            "radius": 8, "fill": "@edge", "stroke": { "color": "@axis", "width": 1 } },
          { "id": "dna-icon", "type": "icon", "name": "dna", "at": [80, 288], "size": [32, 32], "color": "@fg" },
          { "id": "dna-label", "type": "text", "role": "label", "at": [120, 288], "size": [104, 32],
            "valign": "middle", "align": "left", "text": "DNA · 2,114 bp" }
        ] },

        { "id": "bias-lane", "type": "group", "objects": [
          { "id": "lane-bias", "type": "shape", "geo": "roundRect", "at": [248, 184], "size": [456, 112],
            "radius": 16, "fill": "@surface" },
          { "id": "lane-bias-accent", "type": "shape", "geo": "rect", "at": [248, 184], "size": [8, 112],
            "fill": "@cat2" },
          { "id": "lane-bias-label", "type": "text", "role": "label", "at": [272, 200], "size": [408, 16],
            "align": "left", "text": "ASSAY BIAS · FROZEN" },
          { "id": "frozen", "type": "shape", "geo": "roundRect", "at": [272, 224], "size": [160, 48],
            "radius": 8, "fill": "@edge", "stroke": { "color": "@axis", "width": 1 } },
          { "id": "frozen-icon", "type": "icon", "name": "snowflake", "at": [280, 232], "size": [32, 32], "color": "@fg" },
          { "id": "frozen-label", "type": "text", "role": "label", "at": [320, 232], "size": [104, 32],
            "valign": "middle", "align": "left", "text": "Frozen bias" }
        ] },

        { "id": "signal-lane", "type": "group", "objects": [
          { "id": "lane-signal", "type": "shape", "geo": "roundRect", "at": [248, 312], "size": [456, 112],
            "radius": 16, "fill": "@surface" },
          { "id": "lane-signal-accent", "type": "shape", "geo": "rect", "at": [248, 312], "size": [8, 112],
            "fill": "@cat1" },
          { "id": "lane-signal-label", "type": "text", "role": "label", "at": [272, 328], "size": [408, 16],
            "align": "left", "text": "REGULATORY SIGNAL · TRAINABLE" },
          { "id": "conv", "type": "shape", "geo": "roundRect", "at": [272, 352], "size": [160, 48],
            "radius": 8, "fill": "@edge", "stroke": { "color": "@axis", "width": 1 } },
          { "id": "conv-icon", "type": "icon", "name": "filter", "at": [280, 360], "size": [32, 32], "color": "@fg" },
          { "id": "conv-label", "type": "text", "role": "label", "at": [320, 360], "size": [104, 32],
            "valign": "middle", "align": "left", "text": "Conv1D · k=21" },
          { "id": "dilated", "type": "shape", "geo": "roundRect", "at": [456, 352], "size": [160, 48],
            "radius": 8, "fill": "@edge", "stroke": { "color": "@axis", "width": 1 } },
          { "id": "dilated-icon", "type": "icon", "name": "stack-2", "at": [464, 360], "size": [32, 32], "color": "@fg" },
          { "id": "dilated-label", "type": "text", "role": "label", "at": [504, 360], "size": [104, 32],
            "valign": "middle", "align": "left", "text": "Dilated stack" }
        ] },

        { "id": "merge", "type": "group", "objects": [
          { "id": "fuse", "type": "shape", "geo": "ellipse", "at": [728, 272], "size": [152, 64],
            "fill": "@edge", "stroke": { "color": "@axis", "width": 1 } },
          { "id": "fuse-icon", "type": "icon", "name": "math-function", "at": [744, 288], "size": [32, 32], "color": "@fg" },
          { "id": "fuse-label", "type": "text", "role": "label", "at": [784, 288], "size": [88, 32],
            "valign": "middle", "align": "left", "text": "Merge heads" }
        ] },

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
          "from": { "object": "conv", "side": "right" }, "to": { "object": "dilated", "side": "left" },
          "stroke": { "color": "@axis", "width": 1.5 }, "tailEnd": "arrow" },
        { "id": "c-dilated-fuse", "type": "connector", "geo": "bent",
          "from": { "object": "dilated", "side": "right" }, "to": { "object": "fuse", "side": "left" },
          "stroke": { "color": "@axis", "width": 1.5 }, "tailEnd": "arrow" },

        { "id": "caption", "type": "text", "role": "caption", "at": [72, 448], "size": [816, 24],
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
  drawn behind them (objects earlier in the list paint first). **Wrap the
  background, accent, title and member nodes in one `group`** (as the example
  does) so the whole lane is a single layer that moves and restyles as a unit —
  see "Structure with layers (groups)" above. Reads cleaner than a mermaid
  swimlane, and every piece stays movable.
- **Icon-in-box nodes**: three objects sharing a spot — a `shape` box, an
  `icon` (`{"type":"icon","name":"snowflake"}`; find a name with `chimaera
  board icons <query>`), and a `text` with `valign:"middle"`. Inset the icon
  and the label inside the box (8 pt in the example) rather than giving them
  the box's own frame: a text frame more than 2.5× its own ink height lints
  **underfull**, so a 16 pt label in a 48 pt box is a finding while the same
  label in a 32 pt frame, centred, is not. `text` accepts a **bare string**
  (`"text":"Conv1D"`), an **array** of lines (`"text":["a","b"]`), OR **rich
  runs** (`"text":{"runs":[{"t":"x","b":true}]}`) — all three are valid
  anywhere text appears (labels, shape text, connector labels).
- **Connectors** bind endpoints by box edge — `"from":{"object":"conv",
  "side":"right"}`, `"to":{"object":"dilated","side":"left"}` — so the line
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
  box.

## Alignment, margins, and the closing lint

Place on numbers you computed, never by eye. Four scales govern a page, and
`lint --style` measures every one of them:

- **Margins come from the theme, widened by your own grid.** `talk` reserves
  72 pt left and right, 64 pt top and bottom; `figure` reserves 8 pt all round.
  Lint enforces the **wider** of the theme's margin and `canvas.grid.margin` on
  every edge — declaring a grid margin is you stating where content starts, so
  it binds, and a slack grid can never relax the theme's. On a 960×540 talk
  slide with no grid that box is **x 72…888, y 64…476**; add
  `"grid":{"margin":80}` and it tightens to **x 80…880, y 80…460**. Every
  object — footers, page numbers, legend dots — sits inside it or it is a
  margin violation. Read your grid's margin before you place anything: this is
  the single most common way a page that "looks fine" lints dirty.
- **Every `at` and `size` snaps to 8 pt on save**, so author on multiples of 8
  or your placement shifts under you the first time the file is written. A
  bottom edge is therefore always a multiple of 8 too: under a grid margin of
  80 the last legal band ends at y 456, not 460.
- **Columns come from `canvas.grid`** — `{cols, rows?, margin, gutter}`,
  advisory geometry (objects keep their own `at`/`size`) that the pane draws
  and snaps to and you compute against. The cell rect is deterministic math:
  for a canvas `W×H`, `cols` columns, margin `m`, gutter `g` — column width
  `cw = (W − 2m − g·(cols−1)) / cols`, cell `(col,row)` top-left
  `x = m + col·(cw + g)`, a `colSpan` of `s` is `w = s·cw + (s−1)·g`; row
  height `rh = (H − 2m − g·(rows−1)) / rows` when `rows` is set, else `cw` (a
  square module) with `y = m + row·(rh + g)`. Choose `cols`/`margin`/`gutter`
  so `cw` AND the pitch `cw + g` are multiples of 8 — otherwise the save-snap
  knocks objects off your own cells. Two that work on a 960×540 talk slide,
  both making the enforced box **x 80…880, y 80…460**:
  `{"cols":10,"margin":80,"gutter":0}` (80 pt columns at x 80, 160, … 800, the
  last ending at 880 — the default for a deck) and
  `{"cols":3,"margin":80,"gutter":40}` (three 240 pt panels at x 80, 360, 640).
  A `"margin":0` grid is the trap the other way: its outer columns start at
  x 0 and end at x 960, both outside the talk margins, so placing on them lints
  dirty on every page.
- **Vertical rhythm is a scale, not a grid** — with `rows` unset the grid
  constrains x alone, so y is yours: bands on multiples of 8, ~24 pt between
  related blocks and 40+ between sections. A talk page that works under either
  grid above: title at y 80, body band from y 192, caption/footer ending by
  y 456.

Declaring a grid and then placing at eyeballed coordinates is worse than
declaring none — lint reports every object that sits on no cell. Same row →
same `y`, same column → same `x`, and reuse one `size` for peer boxes. After
placing, tidy a selection by id with **`arrange`** (`align-left`,
`align-right`, `align-top`, `align-bottom`, `align-center-h`,
`align-center-v`, `distribute-h`, `distribute-v`, `grid`) — the first id is the
anchor everything else snaps to. In the pane a selection can also **snap to the
grid** and a group **moves as one unit**.

### Close the loop before you show the user

Plain `lint FILE` proves only that the board is legal: it prints `clean` on a
deck whose text overflows its boxes and whose footers hang past the margin.
The check that matters, on every board you hand over:

    chimaera board lint FILE --style        # margins, off-grid, overfull/underfull, budgets
    chimaera board lint FILE --style --fix  # snap the mechanically unambiguous ones
    chimaera board render FILE              # then LOOK at the PNG

Every finding names the object, the field and the numbers, so fix them
directly and re-run until `--style` prints `clean` — a `warning` you leave
behind is a defect you shipped; an `info` (off-grid drift) is a nudge. Only
then tell the user the board is ready.

## The other verbs, one line each

- `describe FILE` — read back every object, position, and chart provenance;
  run it before editing a board the human may have moved things on.
- `lint FILE [--target talk-16x9|pub-nature-single|…] [--style] [--strict]
  [--fix]` — legality + the measured layout findings; always run `--style`.
- `render FILE [--page N] [-o OUT]` — PNGs (no chat card).
- `export FILE --format pptx|pdf|svg|svg-outlined` — the hand-off; see
  "Hand it off" below.
- `adopt SHOWN_ID [--to board]` — promote a shown card into the workspace.
- `import fig.svg|.png|.mmd --to FILE` — figures/mermaid into a board.
- `icons [QUERY] [--list]` — find bundled icons by name/synonym in one call.
- `journal FILE [--since N]` — what the human changed on the surface.
- `arrange FILE --op align-left|distribute-h|grid --ids a,b,c` — tidy by id.
- `theme-export ID --format mplstyle|json` — theme numbers for matplotlib.

## Hand it off — export is ours

A board IS the deliverable, and `chimaera board export` is the only way it
leaves here:

    chimaera board export boards/talk.board --format pptx   # an editable deck
    chimaera board export boards/talk.board --format pdf    # the whole deck
    chimaera board export boards/talk.board --format svg    # one file per page

`--format pptx|pdf|svg|svg-outlined` (`svg-outlined` flattens glyphs to paths
for a host without the fonts). Output lands in `.chimaera/board/exports/`
unless `-o` says otherwise — one file for pptx/pdf, one per page for SVG — and
pptx prints a fate line per object. PPTX is native, not a picture of a slide:
text stays text, a `group` becomes a real PowerPoint group, a table a real
table, a bent connector a `custGeom` path, an icon a group of editable vector
shapes — the user opens it and moves anything.

**Never re-convert a board through another presentation tool, another skill,
or an HTML→pptx converter.** Those drop pages, flatten every object into
unusable XML, and destroy the editability that is the whole point — and their
font substitutions are not ours to debug. If a handed-over deck looks wrong,
first check who wrote it: `docProps/app.xml` in our export says
`Application: chimaera board`.

## Where boards live

Everything `show` writes lands under `.chimaera/board/shown/`
(self-gitignored) — exploratory results never dirty the repo, and re-shows
overwrite by id. Create a persistent board ONLY when the user explicitly asks
for a deliverable (a deck, a figure to keep): put it at
`./boards/<name>.board` (create the directory; it is an ordinary tracked
file) and keep editing that file in place.
