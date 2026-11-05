# PowerPoint: a plugin that helps agents make good decks

> **Dated design record (2026-11-04), pre-implementation proposal.** Awaiting the
> maintainer's go-ahead. Nothing here has shipped. Viewing decks and referencing
> slides stay core ([document workbench](document-workbench-plan.md), issue
> martinappberg/chimaera#345); this plan is the opt-in authoring half.

Agents write `.pptx` files blind. They place boxes on blank slides, overflow text,
pick colours that fail contrast and never see the result. The **PowerPoint** plugin
(`id = "pptx"`, its own repository `martinappberg/chimaera-plugin-pptx`) gives every
agent in a workspace where it is on:

1. **Eyes**: `render_slides` turns slides into pictures the agent sees in the same
   tool answer.
2. **Taste**: four curated themes (masters, layouts, placeholders) and a guide that
   teaches building from a theme's layouts instead of hand-placed boxes.
3. **A checker**: `check_deck` finds overflowing text, shapes off the slide, fonts a
   viewer may not have, low contrast and inconsistent type, as a problems list in the
   workbench and as a tool answer.

Why a plugin and not core (the maintainer's split): the picture engine is LibreOffice,
which not everyone wants, and the plugin contract keeps format tooling out of the host.
Why not Claude Code's own pptx skill: this works for every agent Chimaera runs, gives
agents a picture of their slides, and ships themes.

## What the user sees

- **On the deck's file bar** (beside the core viewer, which stays as it is): one file
  action, **Check**. It opens the plugin's **Slides** tab for that deck.
- **The Slides tab**: the deck as a grid of rendered slides (LibreOffice's rendering,
  so what a colleague without the fonts will see too), the problems list beside it
  (counts in its head, each row names its slide; **Ask agent** on every row), a
  `status` pill (`12 slides · 3 problems · rendered 4 s ago`), **Render again**, and
  **Save PDF beside deck** (the existing `save-to-workspace` action). While the tab is
  open, an agent's write to the deck re-renders it (the Typst pattern: `watch` + a
  `file-changed` event, debounced).
- **A Themes tab** (the card's **Open**): each theme as a contact sheet of its sample
  slides, **Save to workspace** (copies the template into the project) and **Ask
  agent** ("Build a deck on the Folio theme about …").
- **On the card**: one tool, LibreOffice, and two settings (below). No other chrome.

Nothing here edits the core viewer (`PptxView.svelte`). Two optional generic follow-ups
would bring problems *onto* the deck view; both are the viewer owner's call
([follow-ups](#follow-ups-for-core-not-in-this-plan)).

## What agents get

Instruction paragraph (always present where on, ≤ 8 KiB, about 600 bytes): build decks
from a theme with python-pptx, call `render_slides` after every change and look at the
pictures, fix what `check_deck` reports, call `deck_guide` before the first deck.

| Tool | Arguments | Answer |
|---|---|---|
| `render_slides` | `path`, `slides?` (`"3"`, `"2-5,9"`), `full?` (bool) | Pictures in the answer (below), the problems found, the PNG paths. Waits for LibreOffice (≤ 45 s, the host's hold); a first run creates its profile, so says how to call back if it outlasts the hold. |
| `check_deck` | `path` | Every problem as `slide 3 · "Title 1": …`, errors first. Static checks need nothing installed; measured overflow comes from the newest render of this exact file (it says "render to measure overflow" when there is none). Also publishes `diagnostics/1`. |
| `deck_themes` | `name?` | No name: the four themes, one line each, and one contact-sheet picture of all four. A name: its layouts with each placeholder's `idx`, type and box, its colours and fonts, the template's absolute path (written to the output folder on demand), and that theme's contact sheet. |
| `deck_guide` | — | The design checklist and the python-pptx recipe ([below](#the-guide-and-the-agent-side-skill)). |

**The picture budget.** A whole deck at full size costs ~1,200 tokens a slide. So:
with no `slides`, the answer is one **contact sheet** per 12 slides (a 4 × 3 grid,
slide numbers stamped in a corner; enough to judge layout, balance and overflow) plus
the problems list; with `slides` (≤ 6) or `full`, each slide at 1600 px wide. Every
slide's PNG is always written, and its absolute path listed, so an agent whose client
drops images can open it with its own file tool.

## The render engine

**LibreOffice converts the deck to PDF; the plugin rasterizes the PDF itself.**

- `soffice --headless --norestore -env:UserInstallation=file://<output>/lo-profile
  --convert-to 'pdf:impress_pdf_Export:{…ReduceImageResolution, MaxImageResolution 150…}'
  --outdir <output>/<key> <deck>` as one plugin job (wall 120 s, priority by caller).
  Its own profile matters: without it, a headless run hands the file to a LibreOffice
  the user has open and returns at once with nothing converted. Image reduction keeps
  the PDF small enough to rasterize inside the plugin's 64 MiB. LibreOffice older than
  7.4 has no JSON filter options; the plugin falls back to plain `pdf` there.
- **Rasterizing in the component** with [hayro](https://github.com/LaurenzV/hayro)
  (pure Rust, Apache-2.0/MIT, the PDF renderer Typst uses for PDF images). A spike on
  this branch's toolchain: it builds for `wasm32-wasip2` (5.7 MB component before
  size tuning) and renders 12 slides at 1600 px and encodes them as PNG in 211 ms
  natively; the component should do it in about a second. So LibreOffice is the
  **only** program the plugin needs: no poppler, Ghostscript or Python on the host.
- Why not LibreOffice to PNG directly: its graphic export writes only the first
  slide, and its page-range option is ignored for PNG (LibreOffice bug 157425).
- Why not the core viewer's renderer (`@aiden0z/pptx-renderer`): it runs in the
  browser, and the agent may be on a host with no window open. LibreOffice also
  substitutes missing fonts the way a colleague's machine would, which is the point.
- **Timing.** The job runs; `job-finished` (30 s budget) rasterizes every slide into
  `output:<key>/slide-NN.png` and the contact sheets. The waiting tool's
  `tool_resume` (5 s budget) may run before that event: it then rasterizes only what
  its answer shows (one sheet, or ≤ 6 slides), and the event skips what exists.
  Renders are keyed by the deck's path, size and mtime, so a repeated call on an
  unchanged deck answers from the PNGs without starting LibreOffice.

### Installing LibreOffice

The plugin never installs anything on its own. In order:

1. **The host's own LibreOffice**: `soffice` on the PATH the user's terminals get,
   Environment settings included (a `module load libreoffice` on a cluster counts).
   Most Linux servers and many clusters have one.
2. **macOS's standard install** (`/Applications/LibreOffice.app`, which puts nothing
   on the PATH). Needs host change **B** below; without it the plugin's empty state
   says to add `/Applications/LibreOffice.app/Contents/MacOS` in Environment settings.
3. **One click**: a `[[tools]]` entry, pinned by URL and sha256 like TinyTeX. This is
   the part with a real choice, because LibreOffice ships as a `.dmg` on macOS and an
   AppImage on Linux, and the host unpacks neither today:

| Option | macOS | Linux | Cost |
|---|---|---|---|
| **C1. Host unpackers** `unpack = "dmg"` and `"appimage"` | `hdiutil attach -nobrowse -readonly`, copy the `.app`, detach | run the file once with `--appimage-extract` in the staging folder, inside the existing unpack confinement | Two generic unpackers in `toolchain.rs` with tests; artifacts come straight from The Document Foundation / LibreItalia mirrors (versioned URLs) |
| **C2. Re-host** a tarball per platform in the plugin's own releases | CI unpacks TDF's signed `.dmg` into a `.tar.xz` (the signature survives) | CI extracts the AppImage into a `.tar.xz` | No host change; we redistribute ~300 MB per platform (MPL-2.0: notice + source link) and re-cut on each LibreOffice update |
| **C3. No one-click in v1** | Empty state links to libreoffice.org and Environment settings | Same, or `module load` | Nothing to build; the weakest first run |

**Recommendation: C3 for the first build, C1 next.** C1 keeps the bytes coming from
the publisher and helps any later plugin that needs a macOS app or an AppImage. The
first build and its live proof do not depend on it.

## The checks

Each finding is `{slide, shape?, severity, message, fix}`; published as `diagnostics/1`
items with `file` = the deck, `line` = the slide number (the deck viewer's
`#slide=N` convention), `source = "pptx"`.

| Check | How | Severity |
|---|---|---|
| **Text overflows its box** | *Measured*: the render's glyph boxes (hayro's interpreter reports them) against each text shape's box resolved through slide → layout → master; text that runs past a box's bottom or the slide's edge, and is not inside another shape's box | error |
| **Shape off the slide** | Static: an `xfrm` outside `sldSz` (by more than 1% of the slide) | error (entirely off), warning (partly) |
| **Fonts the deck doesn't embed** | Static: every typeface used (runs, theme `+mj`/`+mn`) minus `embeddedFontLst` minus the fonts every PowerPoint has (Arial, Calibri, Cambria, Georgia, Aptos, …). Measured too: a font the render substituted means *these pictures* don't show the real font, and the answer says so | warning |
| **Low contrast** | Static: run colour (direct, or inherited from the master's text styles) against the resolved solid background (shape fill, else slide, layout, master); WCAG ratio < 4.5 (< 3 at ≥ 24 pt). Gradients and pictures: sampled from the render around the glyph boxes | warning |
| **Inconsistent type** | Static: more than two families in the deck (monospace exempt); a layout's title at different sizes across slides; body text under 14 pt; direct size overrides fighting the theme | warning, info |
| **One idea per slide** | Static: > 60 words of body text or > 6 bullets | info |
| **Empty placeholder, missing title** | Static: a placeholder left empty (it shows "Click to add" in PowerPoint), a content slide with no title | warning |

Reading the deck: `host::read` stops at 8 MiB, and a deck with photographs is often
larger. Under 8 MiB the component reads it directly; above, the render job's output
gives no XML, so the plugin declares `cp` and copies the deck into its output folder,
where `output_read` reads ranges (the zip's central directory, then only the XML
parts). Media are never inflated.

## Themes

Four themes, 16:9, designed here. The only outside bytes are python-pptx's default
template (MIT) they start from, so the themes ship under the plugin's own licence.
Each is a `.pptx` with one master, eleven named layouts (Title,
Section, Title and Content, Two Content, Comparison, Title Only, Statement, Big
Number, Figure, Figure and Notes, Closing), a colour scheme with six accents tested
for contrast on both backgrounds, and font schemes built only from fonts every
PowerPoint has, each with a metric-compatible open twin so LibreOffice's render is
faithful (Calibri → Carlito, Cambria → Caladea, Arial → Liberation Sans, Georgia →
Gelasio).

| Theme | Look | Fonts (head / body) | For |
|---|---|---|---|
| **Clarity** | White, one cobalt accent, generous margins | Arial / Arial | Updates, proposals |
| **Ink** | Near-black, warm white text, one amber accent | Arial / Arial | Talks, keynotes |
| **Folio** | Warm paper, serif headlines, thin rules | Georgia / Arial | Narrative, reviews |
| **Lab** | Light grey, figure-first layouts, numbered captions | Calibri / Calibri | Research talks, lab meetings |

They are generated by a script in the plugin's repository from one spec per theme
(colours, fonts, layout boxes), starting from python-pptx's default template (MIT; known
to open cleanly in PowerPoint), so a fix is a spec edit. The same spec is also
written as `<theme>.json` for pptxgenjs's `defineSlideMaster`, for agents that work in
Node. The component embeds the `.pptx` files and their preview sheets (≈ 300 KB).

## The guide and the agent-side skill

`deck_guide` (and the skill) carry the checklist and the recipe:

- **Start from a theme**: `Presentation(template)`, delete its sample slides, add each
  slide from a named layout, fill placeholders by `idx`. Never a blank slide with
  free text boxes; never set fonts or colours on runs (the theme owns them).
- **Hierarchy**: one headline that states the point ("Revenue doubled in Q3", not
  "Q3 revenue"), at most one level of bullets, the takeaway first.
- **Font pairing and size**: the theme's two fonts only; nothing under 14 pt.
- **Contrast**: theme colours on theme backgrounds; never text over a busy photo
  without a scrim.
- **One idea per slide**: split rather than shrink; speaker notes for detail.
- **Charts are charts**: native charts (`add_chart`) from the data, in the theme's
  accents, so they stay editable; a picture only for a figure that already exists.
- **Look**: `render_slides` after each change; read every problem `check_deck` reports
  and fix the cause (shorter text, another layout), never by shrinking type.

The **agent-side plugin** (`recommends.agent_plugins` for claude and codex, the same
repository as a marketplace, Mycelium's shape) is a skill, `decks`, with the same text,
the theme files and a 150-line `deck.py` helper (open a theme, list layouts, add a
slide by layout name, fill placeholders, add a native chart). It is useful without
Chimaera too; inside Chimaera it tells the agent to prefer the plugin's tools.
Recommended, never required: the workbench plugin's instruction paragraph and
`deck_guide` carry what an agent needs.

## Host changes (generic, each with tests and docs)

**A. A plugin tool's answer can show the agent pictures.** Today `call_tool` answers
one text block (`runtime.rs`); the WIT `tool-result` is a typed record, so a new field
means a new world. Proposed without a WIT change: a line of the answer that is exactly
`![alt](output:<path>)`, naming a PNG, JPEG, WebP or GIF in *that plugin's* output
folder for *that workspace*, becomes an MCP `image` content block after the text; the
line itself is rewritten to the file's absolute path. At most 8 pictures, 2 MiB each,
8 MiB in all, read off the reactor through the output folder's no-link reads; one
over a cap or missing stays a line of text with the reason. Any plugin can use it (a
plot from a data plugin, a rendered figure); no plugin id or format is named. Tests in
`tests/plugin_platform.rs` through the platform fixture; the guide, the server map and
the plugins feature page updated. (The alternative is `images: list<string>` on
`tool-result` in a new WIT world 0.3; heavier, and nothing else needs 0.3 yet.)

**B. (Optional) A declared program may name where it is usually installed.**
`[[programs]] also = ["/Applications/LibreOffice.app/Contents/MacOS"]`: absolute
folders (or `~/…`) looked in after the PATH and before the plugin's own tools. Listed on
the card's Can row and in the capability digest. Small change in `jobs.rs::resolve`
and `platform.rs`.

**C1** (later, if chosen): the two unpackers above.

## Manifest sketch

```toml
id = "pptx"
name = "PowerPoint"
version = "0.1.0"
summary = "Agents see their slides, build on curated themes, and fix what the deck checker finds."
api = "0.2"

[access]
files = "read"

[provides]
mcp_tools = ["render_slides", "check_deck", "deck_themes", "deck_guide"]
events = ["file-changed", "job-finished", "settings-changed", "switched-off"]

[adds]
ui = ["Check on every .pptx: rendered slides and their problems · a Themes gallery · LibreOffice detected or installed"]
agents = ["render_slides shows an agent its slides · check_deck lists overflow, off-slide shapes, fonts, contrast · deck_themes · deck_guide"]

[[views]]
id = "slides"
title = "Slides"
slot = "tab"

[[views]]
id = "themes"
title = "Themes"
slot = "tab"

[[actions]]
match = ["*.pptx"]
label = "Check"
action = "check"

[[programs]]
name = "soffice"
version = ["--version"]

[[programs]]
name = "cp"

[[settings]]
key = "render_on_change"     # re-render an open deck when it changes
type = "bool"
default = true

[[settings]]
key = "timeout_s"
type = "number"
default = 120
scope = "host"

[recommends.agent_plugins.claude]
id = "decks@chimaera-plugin-pptx"
marketplace = "martinappberg/chimaera-plugin-pptx"

[recommends.agent_plugins.codex]
id = "decks@chimaera-plugin-pptx"
marketplace = "martinappberg/chimaera-plugin-pptx"

[release]
github = "martinappberg/chimaera-plugin-pptx"
```

No `[[files]]` claim on `*.pptx`: claiming it would replace the core viewer. Resource
use: the component does nothing until a deck is checked or a tool is called; jobs run
under the host's queue and limits; rendered PNGs live in the output folder under its
1 GiB quota, a render kept per deck (the newest 12 decks).

## Plan of work

1. Host change A (+ B if approved) on this branch, with tests, docs, `just check`.
2. The plugin, locally in `~/dev/chimaera-plugin-pptx` (no GitHub repository until the
   maintainer says so): zip/XML reading and the checks as pure, natively tested
   functions; hayro rasterizing and the contact sheet; the jobs, views and tools.
3. The themes and their generator; previews rendered through the plugin itself.
4. The agent-side skill package in the same repository.
5. Live proof: install the build into the isolated daemon (`chimaera plugin add
   --path`), a real Claude chat (and Codex, billed, if approved) builds a themed deck,
   renders it, sees the pictures, fixes what `check_deck` flags; screenshots of the
   Slides and Themes tabs in light and dark.

Not done without the maintainer: creating the repository, publishing a release, adding
it to `plugins/plugins.lock`.

## Questions for the maintainer

1. **Go-ahead** on the shape above.
2. **Host change A** as a markdown-image line (recommended) or a WIT 0.3 field?
3. **Host change B** (macOS's LibreOffice found without PATH setup): yes or no?
4. **One-click LibreOffice**: C3 now and C1 next (recommended), C1 now, or C2?
5. **Name**: "PowerPoint" (Microsoft's trademark, used to say what files it works with)
   or a neutral "Slides"? The id stays `pptx`.

## Follow-ups for core (not in this plan)

- **Problems on the deck view.** A generic "N problems" chip in the file bar
  (`PluginFileGate`) for any file that has `diagnostics/1` items and no editor marks,
  opening the problems list. Benefits any plugin that checks a binary format.
- **`line` as slide.** The deck viewer could treat an `open-file` line as a slide (as
  `#slide=N` already is), so a problem's **Go to** lands on its slide. The viewer
  session's call.
