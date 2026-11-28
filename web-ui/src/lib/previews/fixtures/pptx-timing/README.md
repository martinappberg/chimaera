# PowerPoint timing corpus

Decks whose animations and transitions were written by PowerPoint itself, and the timeline each one must parse to. Two parsers read them and must agree: the viewer's (`../../pptxTiming.ts`, TypeScript) and the PowerPoint plugin's (`chimaera-plugin-pptx`, Rust, which vendors this folder at a pinned commit). A change to a golden file is a contract change: review it as one, and change both parsers with it.

## What is here

- `decks/*.pptx`: the corpus. Small, synthetic, no real content.
- `goldens/<deck>.json`: what each deck parses to (format below). Written by `npm --prefix web-ui run timing:golden` from the TypeScript parser, then checked by a person against PowerPoint's own slide show, once. After that it changes only through a reviewed diff.
- `make/make_corpus.py`: rebuilds the decks. python-pptx lays out named shapes; PowerPoint for Mac, driven in the background through its scripting model, adds the effects and transitions and saves. Needs a Mac with PowerPoint. Rerunning it changes ids inside the files, so regenerate the goldens with it.

| Deck | Covers |
|---|---|
| `entrances` | appear, fade, zoom, float, dissolve; fly and wipe from each side; split, wheel, circle, box, blinds; one effect outside the vocabulary (boomerang) |
| `exits-emphasis` | exits; grow, spin, transparency, teeter, fill colour, darken; one shape that enters, is emphasised and leaves |
| `paragraphs` | a text box built by paragraph, with child levels riding their parent's click, by second level, and as a whole |
| `triggers` | on click, with previous, after previous; a slide that starts by itself; durations; auto-advance |
| `paths` | motion paths (lines, arc, circle); paths that add up on one shape |
| `objects` | picture, table, chart, group, connector, rotated shape |
| `interactive` | (PowerPoint's scripting cannot set a trigger shape; the on-shape-click case is a hand-written slide in `cases/`) |
| `transitions` | none, fade, fade through black, push, wipe, cut, one outside the vocabulary (checker), auto-advance |
| `morph` | Morph by object (moved, resized, rotated, `!!` names across shape types, arriving and leaving shapes), by word, by character |
| `agent-written` | no animation; `-raw` is the file as python-pptx wrote it with a duplicated shape id, the other is that file after PowerPoint saved it |

## What PowerPoint does (observed, PowerPoint for Mac 16.115)

- Slide ids (`p:sldId@id`) survive a save. Relationship ids (`rId…`) are renumbered: never keep one.
- A duplicated shape id is repaired on save: the later shape gets a new id. Shape ids are otherwise kept.
- Shapes made in PowerPoint carry `a16:creationId`; shapes a script made do not get one when PowerPoint saves the file.
- The main sequence is `p:timing/p:tnLst/p:par/p:cTn[nodeType=tmRoot]/p:childTnLst/p:seq/p:cTn[nodeType=mainSeq]`. Each child `p:par` whose start condition is `delay="indefinite"` is one click. Inside it, one `p:par` per group that starts together, then one `p:par` per effect, whose `p:cTn` carries `presetClass`, `presetID`, `presetSubtype` and `nodeType` (`clickEffect`, `withEffect`, `afterEffect`).
- A slide that starts by itself has a first click group like any other (`delay="indefinite"`) whose first effect is `withEffect` or `afterEffect`, not `clickEffect`. That group runs on entry.
- Every entrance begins with `p:set style.visibility = visible`, every exit ends with `hidden`. That, not the preset, says what is on screen.
- A text box built by paragraph has one effect for its background (`p:spTgt/p:bg`) only when the box has a fill, then one per paragraph (`p:txEl/p:pRg st end`). Child paragraphs are `withEffect` in their parent's click. `p:bldLst/p:bldP build="p"` records the choice; a chart gets `p:bldGraphic`.
- A transition is wrapped: `mc:AlternateContent` with `mc:Choice Requires="p14"` (duration in `p14:dur`, ms) or `Requires="p159"` for Morph (`p159:morph option="byObject|byWord|byChar"`), and an `mc:Fallback` (for Morph, a plain fade). `advTm` (ms) is on `p:transition`.
- Presets seen (class, id, subtype): appear `entr 1`; fly `2` with subtype 1 top, 2 right, 4 bottom, 8 left; blinds `3`; box `4`; circle `6`; dissolve `9`; fade `10`; split `16`; wheel `21`; wipe `22` with the same side subtypes; zoom `23` subtype 16; float `42`. Exits use the same ids with class `exit`. Emphasis: fill colour `1`, grow/shrink `6` (`p:animScale by 150000`), spin `8` (`p:animRot by 21600000`), transparency `9`, darken `24`, teeter `32`. Paths: class `path`, `p:animMotion path="M 0 0 L 0.25 0 E"` in fractions of the slide.

## The effect vocabulary

One name per effect, shared by both parsers, the plugin's timeline text and its writer. Class is part of the name where a pair exists.

| Name | From |
|---|---|
| `appear` / `disappear` | `entr`/`exit` 1 |
| `fade-in` / `fade-out` | 10 |
| `fly-in` / `fly-out` (+ `from`/`to` `top` `right` `bottom` `left`) | 2 |
| `wipe-in` / `wipe-out` (+ side) | 22 |
| `zoom-in` / `zoom-out` | 23 |
| `float-in` / `float-out` | 42 |
| `split-in` / `split-out`, `wheel-in`, `circle-in`, `box-in`, `blinds-in`, `dissolve-in` (and `-out`) | 16, 21, 6, 4, 3, 9 |
| `grow-shrink`, `spin`, `transparency`, `teeter`, `fill-color`, `darken` | `emph` 6, 8, 9, 32, 1, 24 |
| `path` | class `path` (any id), with its path |
| `other` | anything else: kept and shown by PowerPoint's preset numbers, never rewritten |

Transitions: `none`, `fade`, `fade-black`, `push`, `wipe`, `cut`, `morph`, `other`; `dir` is `l` `r` `u` `d` as PowerPoint writes it (absent means its default).

## Golden format

```json
{
  "deck": "triggers.pptx",
  "slides": [
    {
      "part": "ppt/slides/slide1.xml",
      "id": 256,
      "transition": { "kind": "fade", "dir": null, "dur": 2000, "advanceAfter": 3000, "advanceOnClick": true, "morph": null },
      "steps": [
        [],
        [
          { "trigger": "click", "start": 0, "dur": 500, "class": "entr", "preset": 10, "subtype": 0,
            "effect": "fade-in", "dir": null, "target": "2", "sets": "visible" },
          { "trigger": "with", "start": 0, "dur": 500, "class": "entr", "preset": 10, "subtype": 0,
            "effect": "fade-in", "dir": null, "target": "3", "sets": "visible" },
          { "trigger": "after", "start": 500, "dur": 500, "class": "entr", "preset": 10, "subtype": 0,
            "effect": "fade-in", "dir": null, "target": "4", "sets": "visible" }
        ]
      ],
      "states": [
        { "hidden": ["2", "3", "4", "5"], "moved": {} },
        { "hidden": ["5"], "moved": {} }
      ],
      "interactive": 0
    }
  ]
}
```

- `transition` is `null` when the slide has none. `dur` and `advanceAfter` are milliseconds; `advanceAfter` is `null` without `advTm`.
- `steps[0]` is what runs when the slide appears (empty for most slides); `steps[k]` is click `k`. `start` is milliseconds from the start of that step; an `after` effect starts when everything before it in the step has ended.
- `target` is the shape id as a string: `"4"` for the shape, `"4:bg"` for a text box's background, `"4:p1-1"` for paragraphs 1 to 1 (as PowerPoint numbers them, from 0).
- `sets` is `"visible"`, `"hidden"` or `null` (emphasis and paths change nothing about visibility).
- `path` (only on `path` effects) is PowerPoint's path string.
- `states[k]` is the slide after step `k`: `hidden` lists the targets not on screen, sorted as strings; `moved` maps a target to its summed path offset `[dx, dy]` in slide fractions, rounded to 4 places. `states.length == steps.length`.
- `interactive` counts on-shape-click sequences, which are not in `steps`; their entrance targets count as hidden in every state.
