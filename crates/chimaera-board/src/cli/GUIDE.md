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
  or `--size WxH`; `--theme talk-dark|talk-light|figure-light`.

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
`connector` `image` `group` `table` `chart` `diagram` `equation` (+
annotations `panelLabel` `scalebar` `sigBracket` `legend` `colorbar`
`callout` `inset`). Text carries roles, not font sizes (`"role":"title" |
heading | subtitle | body | caption | label | code`); colors are theme
`@tokens` (`@fg @body @muted @accent1 @surface @edge @bg @cat1..7`) or
`#hex`. Prefer `page.intent.kind` + slots over coordinates — the layout
engine places slotted objects. Themes: `talk-dark` (default), `talk-light`,
`figure-light`. After writing: `lint` then `render` (or `show --file`), and
LOOK at the PNG.

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
