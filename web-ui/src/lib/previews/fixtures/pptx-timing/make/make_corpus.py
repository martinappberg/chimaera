#!/usr/bin/env python3
"""Build the PowerPoint timing corpus: small decks whose animations and
transitions are written by PowerPoint itself, so parsers and writers are
tested against what PowerPoint really saves, never against XML we imagined.

Each deck is made in two steps:
  1. python-pptx lays out the slides and named shapes (no animation).
  2. PowerPoint for Mac opens that file in the background, adds the effects
     and transitions through its own scripting model, and saves in place.

Run on a Mac with Microsoft PowerPoint, with a Python that has python-pptx:

    python make_corpus.py [deck ...]        # all decks, or the ones named

It writes ../decks/<name>.pptx. PowerPoint is never brought to the front and
only the files this script opened are touched. A statement PowerPoint refuses
is reported and skipped, and the deck is still saved: read the report.
"""

import os
import subprocess
import sys
import tempfile
import time

from pptx import Presentation
from pptx.chart.data import CategoryChartData
from pptx.enum.chart import XL_CHART_TYPE
from pptx.enum.shapes import MSO_CONNECTOR, MSO_SHAPE
from pptx.enum.text import MSO_AUTO_SIZE
from pptx.util import Inches, Pt

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.normpath(os.path.join(HERE, "..", "decks"))

# --- the corpus --------------------------------------------------------------
#
# A deck is a list of slides. A slide has:
#   shapes: [(name, kind, {box?, text?, rot?, fill?, line?})]   kind: rect oval text
#           table chart picture group line; fill: gradient | picture; line: width in pt
#   fx:     [(shape, effect, trigger, {dir?, exit?, dur?, level?, amount?, size?, autorev?, color?, path?})]
#           amount: a spin's degrees; size: a grow's percent; autorev: plays back
#           color: (r, g, b) a colour effect changes to; path: a motion path's string
#           (dur and color apply to every effect one add makes: by paragraph, several)
#           effect: PowerPoint's own name after "animation type"
#           trigger: click | with | after
#   transition: {effect, dur?, advance?}          effect: after "entry effect"
#   interactive: [(trigger shape, shape, effect)] an on-shape-click sequence


def grid(i, n):
    """A box for the i-th of n shapes: a row, or two rows past five."""
    cols = n if n <= 5 else (n + 1) // 2
    row, col = divmod(i, cols)
    w = 12.3 / cols
    return (0.5 + col * w, 1.2 + row * 2.9, w - 0.4, 2.2)


def row(names, kind="rect"):
    return [(n, kind, {}) for n in names]


THREE = ["First point", "Second point", "Third point"]

# A font no system ships: PowerPoint and browsers each substitute their own.
MISSING_FONT = "Chimaera Corpus Missing Sans"
LONG = [
    "A first point that runs on long enough to wrap across the width of its box",
    "A second point, with more words than the box was drawn to hold at first",
    "Third: the box grows to fit, so its drawn height is not the file's height",
    "Fourth, carrying on in the same way so the text keeps pushing downwards",
    "Fifth, still more words, measured in whatever font stands in for the missing one",
    "Sixth and last, which ends the list a good way below where the box began",
]

DECKS = {
    "entrances": [
        {
            "shapes": row(["Appear", "Fade", "Zoom", "Float", "Dissolve"]),
            "fx": [
                ("Appear", "appear", "click", {}),
                ("Fade", "fade", "click", {}),
                ("Zoom", "zoom", "click", {}),
                ("Float", "ascend", "click", {}),
                ("Dissolve", "dissolve", "click", {}),
            ],
        },
        {
            "shapes": row(["FlyBottom", "FlyLeft", "FlyRight", "FlyTop"]),
            "fx": [
                ("FlyBottom", "fly", "click", {"dir": "down"}),
                ("FlyLeft", "fly", "click", {"dir": "left"}),
                ("FlyRight", "fly", "click", {"dir": "right"}),
                ("FlyTop", "fly", "click", {"dir": "up"}),
            ],
        },
        {
            "shapes": row(["WipeBottom", "WipeLeft", "WipeRight", "WipeTop"]),
            "fx": [
                ("WipeBottom", "wipe", "click", {"dir": "down"}),
                ("WipeLeft", "wipe", "click", {"dir": "left"}),
                ("WipeRight", "wipe", "click", {"dir": "right"}),
                ("WipeTop", "wipe", "click", {"dir": "up"}),
            ],
        },
        {
            "shapes": row(["Split", "Wheel", "Circle", "Box", "Blinds", "Boomerang"]),
            "fx": [
                ("Split", "split", "click", {}),
                ("Wheel", "wheel", "click", {}),
                ("Circle", "circle", "click", {}),
                ("Box", "box", "click", {}),
                ("Blinds", "blinds", "click", {}),
                ("Boomerang", "boomerang", "click", {}),
            ],
        },
    ],
    "exits-emphasis": [
        {
            "shapes": row(["FadeOut", "FlyOut", "WipeOut", "ZoomOut", "Disappear"]),
            "fx": [
                ("FadeOut", "fade", "click", {"exit": True}),
                ("FlyOut", "fly", "click", {"exit": True, "dir": "left"}),
                ("WipeOut", "wipe", "click", {"exit": True, "dir": "up"}),
                ("ZoomOut", "zoom", "click", {"exit": True}),
                ("Disappear", "appear", "click", {"exit": True}),
            ],
        },
        {
            "shapes": row(["Grow", "Spin", "Transparency", "Teeter", "FillColor", "Darken"]),
            "fx": [
                ("Grow", "grow shrink", "click", {}),
                ("Spin", "spin", "click", {}),
                ("Transparency", "transparency", "click", {}),
                ("Teeter", "teeter", "click", {}),
                ("FillColor", "change fill color", "click", {}),
                ("Darken", "darken", "click", {}),
            ],
        },
        {
            # In, emphasised, then out: one shape through all three classes.
            "shapes": row(["Lifecycle", "Bystander"]),
            "fx": [
                ("Lifecycle", "fade", "click", {}),
                ("Lifecycle", "spin", "click", {}),
                ("Lifecycle", "fade", "click", {"exit": True}),
            ],
        },
    ],
    "paragraphs": [
        {
            "shapes": [("Title", "text", {"text": ["Builds by paragraph"], "box": (0.6, 0.4, 12, 1)}),
                       ("Body", "text", {"text": THREE, "box": (0.6, 1.8, 12, 4)})],
            "fx": [("Body", "fade", "click", {"level": "first"})],
        },
        {
            "shapes": [("Body", "text", {"text": ["Top one", (1, "Child a"), (1, "Child b"), "Top two", (1, "Child c")],
                                         "box": (0.6, 1.0, 12, 5)})],
            "fx": [("Body", "wipe", "click", {"level": "first", "dir": "left"})],
        },
        {
            "shapes": [("Body", "text", {"text": ["Top one", (1, "Child a"), (1, "Child b"), "Top two"],
                                         "box": (0.6, 1.0, 12, 5)})],
            "fx": [("Body", "appear", "click", {"level": "second"})],
        },
        {
            # The whole box at once, beside a box that is never animated.
            "shapes": [("Whole", "text", {"text": THREE, "box": (0.6, 1.0, 5.8, 5)}),
                       ("Still", "text", {"text": THREE, "box": (6.9, 1.0, 5.8, 5)})],
            "fx": [("Whole", "fade", "click", {})],
        },
    ],
    "triggers": [
        {
            "shapes": row(["A", "B", "C", "D"]),
            "fx": [
                ("A", "fade", "click", {}),
                ("B", "fade", "with", {}),
                ("C", "fade", "after", {}),
                ("D", "fly", "click", {"dur": 2.0, "dir": "left"}),
            ],
        },
        {
            # Nothing waits for a click: the slide starts by itself.
            "shapes": row(["OnLoad", "ThenThis", "ClickMe"]),
            "fx": [
                ("OnLoad", "fade", "with", {}),
                ("ThenThis", "wipe", "after", {"dir": "left"}),
                ("ClickMe", "zoom", "click", {}),
            ],
        },
        {
            "shapes": row(["Chain1", "Chain2", "Chain3", "Chain4"]),
            "fx": [
                ("Chain1", "appear", "click", {}),
                ("Chain2", "fade", "after", {"dur": 0.25}),
                ("Chain3", "fade", "after", {"dur": 1.5}),
                ("Chain4", "fade", "with", {}),
            ],
            "transition": {"effect": "fade", "advance": 3.0},
        },
    ],
    "paths": [
        {
            "shapes": row(["Right", "Down", "Diagonal", "Arc", "Circle"]),
            "fx": [
                ("Right", "right path", "click", {}),
                ("Down", "down path", "click", {}),
                ("Diagonal", "diagonal down right path", "click", {}),
                ("Arc", "arc up path", "click", {}),
                ("Circle", "circle path", "click", {}),
            ],
        },
        {
            # A path after an entrance, then away: offsets must add up.
            "shapes": row(["Mover", "Stay"]),
            "fx": [
                ("Mover", "fade", "click", {}),
                ("Mover", "right path", "click", {}),
                ("Mover", "down path", "click", {}),
            ],
        },
    ],
    "objects": [
        {
            "shapes": [("Pic", "picture", {"box": (0.6, 1.0, 3, 2.2)}),
                       ("Table", "table", {"box": (4.0, 1.0, 4, 2.2)}),
                       ("Chart", "chart", {"box": (8.4, 1.0, 4.4, 3.2)}),
                       ("Group", "group", {"box": (0.6, 4.2, 3, 2.2)}),
                       ("Line", "line", {"box": (4.0, 4.6, 4, 1.2)}),
                       ("Rotated", "rect", {"box": (8.6, 4.6, 2.4, 1.4), "rot": 30})],
            "fx": [
                ("Pic", "fade", "click", {}),
                ("Table", "wipe", "click", {"dir": "left"}),
                ("Chart", "fade", "click", {}),
                ("Group", "zoom", "click", {}),
                ("Line", "wipe", "click", {"dir": "left"}),
                ("Rotated", "fly", "click", {"dir": "down"}),
            ],
        },
    ],
    "interactive": [
        {
            "shapes": row(["Button", "Reveal", "Normal"]),
            "fx": [("Normal", "fade", "click", {})],
            "interactive": [("Button", "Reveal", "appear")],
        },
    ],
    "transitions": [
        {"shapes": row(["One"])},
        {"shapes": row(["Fade"]), "transition": {"effect": "fade"}},
        {"shapes": row(["FadeBlack"]), "transition": {"effect": "fade black", "dur": 1.5}},
        {"shapes": row(["PushLeft"]), "transition": {"effect": "push left"}},
        {"shapes": row(["PushUp"]), "transition": {"effect": "push up"}},
        {"shapes": row(["WipeRight"]), "transition": {"effect": "wipe right"}},
        {"shapes": row(["Cut"]), "transition": {"effect": "cut"}},
        {"shapes": row(["Unsupported"]), "transition": {"effect": "checkerboard across"}},
        {"shapes": row(["Auto"]), "transition": {"effect": "fade", "advance": 2.0}},
    ],
    "morph": [
        {
            "shapes": [("Box", "rect", {"box": (0.6, 0.8, 2.4, 1.4)}),
                       ("Dot", "oval", {"box": (4.0, 0.8, 1.4, 1.4)}),
                       ("Label", "text", {"text": ["Alpha"], "box": (6.4, 0.8, 4, 1)}),
                       ("!!Hero", "rect", {"box": (0.6, 4.4, 2, 2)}),
                       ("Leaves", "rect", {"box": (9.6, 4.4, 2.4, 1.4)})],
        },
        {
            "shapes": [("Box", "rect", {"box": (8.6, 4.6, 3.6, 2.1)}),
                       ("Dot", "oval", {"box": (1.0, 4.6, 2.2, 2.2)}),
                       ("Label", "text", {"text": ["Alpha beta"], "box": (0.6, 0.6, 6, 1)}),
                       ("!!Hero", "oval", {"box": (5.6, 2.6, 2.4, 2.4), "rot": 45}),
                       ("Arrives", "rect", {"box": (9.6, 0.8, 2.4, 1.4)})],
            "transition": {"effect": "morph by object", "dur": 1.5},
        },
        {
            "shapes": [("Label", "text", {"text": ["Alpha beta gamma"], "box": (3.6, 3.0, 8, 1)})],
            "transition": {"effect": "morph by word"},
        },
        {
            "shapes": [("Label", "text", {"text": ["Gamma alpha"], "box": (0.6, 5.6, 8, 1)})],
            "transition": {"effect": "morph by char"},
        },
    ],
    # Text boxes that grow to fit their text (spAutoFit), set in a font no
    # machine has, so every renderer measures them in its own fallback font
    # and draws the box at its own size, not the file's.
    "autofit": [
        {
            "shapes": [("Grows", "text", {"text": LONG, "box": (0.6, 0.8, 7.5, 1.0), "autofit": True,
                                          "font": MISSING_FONT, "size": 24})],
            "fx": [("Grows", "fade", "click", {"level": "first"})],
        },
        {
            "shapes": [("Heading", "text", {"text": ["A heading long enough to wrap onto a second line in any font"],
                                            "box": (0.6, 0.4, 6.0, 0.6), "autofit": True,
                                            "font": MISSING_FONT, "size": 40}),
                       ("Under", "rect", {"box": (0.6, 4.6, 3.0, 1.6)})],
            "fx": [("Heading", "fade", "click", {}), ("Under", "fade", "click", {})],
        },
    ],
    # Emphasis that holds after its click (README R24-R31): sizes and turns
    # that compound, opacity, colour on solid, gradient and picture fills,
    # lines and text, a group, an effect that plays back (and must not hold),
    # and held values through an exit and a re-entrance.
    "emphasis": [
        {
            "shapes": [("GrowTwice", "rect", {}), ("GrowShrink", "rect", {}), ("Spin90", "rect", {}),
                       ("Tilted", "rect", {"rot": 30}), ("Pulse", "rect", {}), ("Group", "group", {})],
            "fx": [
                ("GrowTwice", "grow shrink", "click", {}),
                ("GrowTwice", "grow shrink", "click", {}),
                ("GrowShrink", "grow shrink", "click", {}),
                ("GrowShrink", "grow shrink", "click", {"size": 50}),
                ("Spin90", "spin", "click", {"amount": 90}),
                ("Tilted", "spin", "click", {"amount": 90}),
                ("Pulse", "grow shrink", "click", {"autorev": True}),
                ("Group", "grow shrink", "click", {}),
            ],
        },
        {
            "shapes": [("FillSolid", "rect", {}), ("FillGradient", "rect", {"fill": "gradient"}),
                       ("FillPicture", "rect", {"fill": "picture"}), ("LineColor", "rect", {"line": 6}),
                       ("FontBox", "text", {"text": THREE}), ("FontParagraphs", "text", {"text": THREE})],
            "fx": [
                ("FillSolid", "change fill color", "click", {}),
                ("FillGradient", "change fill color", "click", {}),
                ("FillPicture", "change fill color", "click", {}),
                ("LineColor", "change line color", "click", {}),
                ("FontBox", "change font color", "click", {}),
                ("FontParagraphs", "change font color", "click", {"level": "first"}),
            ],
        },
        {
            "shapes": [("Darken", "rect", {}), ("Lighten", "rect", {}), ("Desaturate", "rect", {}),
                       ("Complementary", "rect", {}), ("DarkenGradient", "rect", {"fill": "gradient"}),
                       ("Transparency", "rect", {})],
            "fx": [
                ("Darken", "darken", "click", {}),
                ("Lighten", "lighten", "click", {}),
                ("Desaturate", "desaturate", "click", {}),
                ("Complementary", "complementary color", "click", {}),
                ("DarkenGradient", "darken", "click", {}),
                ("Transparency", "transparency", "click", {}),
            ],
        },
        {
            "shapes": [("Cycle", "rect", {}), ("Boomerang", "rect", {"rot": 30})],
            "fx": [
                ("Cycle", "grow shrink", "click", {}),
                ("Cycle", "spin", "with", {"amount": 90}),
                ("Cycle", "fade", "click", {"exit": True}),
                ("Cycle", "fade", "click", {}),
                ("Boomerang", "boomerang", "click", {}),
            ],
        },
    ],
    # Which change wins when two land on one text box, a paragraph and the
    # whole box (font colour, transparency, size), in both orders; and a
    # second path written to start where the first ended, as PowerPoint's
    # own UI chains them (README R18, R30).
    "order": [
        {
            "shapes": [("ParaThenBox", "text", {"text": ["First", "Second"]}),
                       ("BoxThenPara", "text", {"text": ["First", "Second"]})],
            "fx": [
                ("ParaThenBox", "change font color", "click", {"level": "first", "color": (255, 0, 0), "dur": 0.5}),
                ("ParaThenBox", "change font color", "click", {"color": (0, 0, 255), "dur": 0.5}),
                ("BoxThenPara", "change font color", "click", {"color": (0, 0, 255), "dur": 0.5}),
                ("BoxThenPara", "change font color", "click", {"level": "first", "color": (255, 0, 0), "dur": 0.5}),
            ],
        },
        {
            "shapes": [("FadeParaThenBox", "rect", {"text": ["First", "Second"]}),
                       ("FadeBoxThenPara", "rect", {"text": ["First", "Second"]}),
                       ("GrowParaThenBox", "rect", {"text": ["First", "Second"]})],
            "fx": [
                ("FadeParaThenBox", "transparency", "click", {"level": "first"}),
                ("FadeParaThenBox", "transparency", "click", {}),
                ("FadeBoxThenPara", "transparency", "click", {}),
                ("FadeBoxThenPara", "transparency", "click", {"level": "first"}),
                ("GrowParaThenBox", "grow shrink", "click", {"level": "first", "dur": 0.5}),
                ("GrowParaThenBox", "grow shrink", "click", {"dur": 0.5}),
            ],
        },
        {
            "shapes": row(["Chained", "Restart"]),
            "fx": [
                ("Chained", "right path", "click", {"path": "M 0 0 L 0.25 0 E"}),
                ("Chained", "down path", "click", {"path": "M 0.25 0 L 0.25 0.25 E"}),
                ("Restart", "right path", "click", {"path": "M 0 0 L 0.25 0 E"}),
                ("Restart", "down path", "click", {"path": "M 0 0 L 0 0.25 E"}),
            ],
        },
    ],
    # Every pattern filter PowerPoint's scripting offers, in and out, two
    # seconds each so its film shows the pattern growing; and spins on
    # shapes with and without shadow and outline, and on a picture, to find
    # what PowerPoint turns them about.
    "filters": [
        {
            "shapes": row(["Dissolve", "Checkerboard", "RandomBars", "Strips", "Wedge"]),
            "fx": [
                ("Dissolve", "dissolve", "click", {"dur": 2.0}),
                ("Checkerboard", "checkerboard", "click", {"dur": 2.0}),
                ("RandomBars", "random bars", "click", {"dur": 2.0}),
                ("Strips", "strips", "click", {"dur": 2.0}),
                ("Wedge", "wedge", "click", {"dur": 2.0}),
            ],
        },
        {
            "shapes": row(["CircleIn", "CircleOut", "DiamondIn", "DiamondOut", "PlusIn", "PlusOut"]),
            "fx": [
                ("CircleIn", "circle", "click", {"dur": 2.0, "dir": "inward"}),
                ("CircleOut", "circle", "click", {"dur": 2.0, "dir": "out"}),
                ("DiamondIn", "diamond", "click", {"dur": 2.0, "dir": "inward"}),
                ("DiamondOut", "diamond", "click", {"dur": 2.0, "dir": "out"}),
                ("PlusIn", "plus", "click", {"dur": 2.0, "dir": "inward"}),
                ("PlusOut", "plus", "click", {"dur": 2.0, "dir": "out"}),
            ],
        },
        {
            "shapes": row(["DissolveOut", "CheckerboardOut", "RandomBarsOut", "StripsOut", "WedgeOut"]),
            "fx": [
                ("DissolveOut", "dissolve", "click", {"dur": 2.0, "exit": True}),
                ("CheckerboardOut", "checkerboard", "click", {"dur": 2.0, "exit": True}),
                ("RandomBarsOut", "random bars", "click", {"dur": 2.0, "exit": True}),
                ("StripsOut", "strips", "click", {"dur": 2.0, "exit": True}),
                ("WedgeOut", "wedge", "click", {"dur": 2.0, "exit": True}),
            ],
        },
        {
            "shapes": [("Plain", "rect", {"shadow": False, "noline": True}), ("Shadow", "rect", {"noline": True}),
                       ("Outline", "rect", {"shadow": False, "line": 12}), ("Both", "rect", {"line": 12}),
                       ("Picture", "picture", {})],
            "fx": [
                ("Plain", "spin", "click", {"amount": 90}),
                ("Shadow", "spin", "click", {"amount": 90}),
                ("Outline", "spin", "click", {"amount": 90}),
                ("Both", "spin", "click", {"amount": 90}),
                ("Picture", "spin", "click", {"amount": 90}),
            ],
        },
    ],
    # No animation, and saved by PowerPoint: what happens to the ids python-pptx
    # wrote (see agent-written-raw.pptx, the same deck before PowerPoint).
    "agent-written": [
        {"shapes": [("Title 1", "text", {"text": ["Written by a script"], "box": (0.6, 0.4, 12, 1)}),
                    ("Body", "text", {"text": THREE, "box": (0.6, 1.8, 12, 4)})]},
        {"shapes": row(["Same", "Same", "Other"])},
    ],
}

# --- step 1: the base deck ---------------------------------------------------


def picture_file():
    from PIL import Image, ImageDraw

    path = os.path.join(tempfile.gettempdir(), "pptx-corpus-picture.png")
    img = Image.new("RGB", (300, 220), (214, 228, 240))
    d = ImageDraw.Draw(img)
    d.ellipse((90, 50, 210, 170), fill=(156, 74, 48))
    img.save(path)
    return path


def picture_fill(slide, sh):
    """Fill an autoshape with a picture (python-pptx has no API for it)."""
    from pptx.oxml.ns import qn
    from lxml import etree

    _, rid = slide.part.get_or_add_image_part(picture_file())
    sp_pr = sh._element.spPr
    for tag in ("a:solidFill", "a:gradFill", "a:noFill", "a:blipFill", "a:pattFill"):
        for el in sp_pr.findall(qn(tag)):
            sp_pr.remove(el)
    blip = etree.SubElement(sp_pr, qn("a:blipFill"))
    etree.SubElement(blip, qn("a:blip")).set(qn("r:embed"), rid)
    etree.SubElement(etree.SubElement(blip, qn("a:stretch")), qn("a:fillRect"))
    # A fill goes right after the geometry, before any line.
    geom = sp_pr.find(qn("a:prstGeom"))
    if geom is not None:
        geom.addnext(blip)


def add_shape(slide, name, kind, opts, box):
    x, y, w, h = (Inches(v) for v in opts.get("box", box))
    shapes = slide.shapes
    if kind in ("rect", "oval"):
        sh = shapes.add_shape(MSO_SHAPE.RECTANGLE if kind == "rect" else MSO_SHAPE.OVAL, x, y, w, h)
        text = opts.get("text", [name])
        sh.text_frame.text = text[0]
        for t in text[1:]:
            sh.text_frame.add_paragraph().text = t
    elif kind == "text":
        sh = shapes.add_textbox(x, y, w, h)
        tf = sh.text_frame
        tf.word_wrap = True
        if opts.get("autofit"):
            tf.auto_size = MSO_AUTO_SIZE.SHAPE_TO_FIT_TEXT
        for i, item in enumerate(opts.get("text", [name])):
            level, text = item if isinstance(item, tuple) else (0, item)
            p = tf.paragraphs[0] if i == 0 else tf.add_paragraph()
            p.text = text
            p.level = level
            p.runs[0].font.size = Pt(opts.get("size", 28) - 4 * level)
            if "font" in opts:
                p.runs[0].font.name = opts["font"]
    elif kind == "table":
        sh = shapes.add_table(2, 3, x, y, w, h)
        for r in range(2):
            for c in range(3):
                sh.table.cell(r, c).text = f"r{r + 1}c{c + 1}"
    elif kind == "chart":
        data = CategoryChartData()
        data.categories = ["A", "B", "C"]
        data.add_series("First", (3, 5, 2))
        data.add_series("Second", (4, 1, 6))
        sh = shapes.add_chart(XL_CHART_TYPE.COLUMN_CLUSTERED, x, y, w, h, data)
    elif kind == "picture":
        sh = shapes.add_picture(picture_file(), x, y, w, h)
    elif kind == "group":
        sh = shapes.add_group_shape()
        a = sh.shapes.add_shape(MSO_SHAPE.RECTANGLE, x, y, w // 2, h)
        b = sh.shapes.add_shape(MSO_SHAPE.OVAL, x + w // 2, y, w // 2, h)
        a.name, b.name = f"{name} left", f"{name} right"
    elif kind == "line":
        sh = shapes.add_connector(MSO_CONNECTOR.STRAIGHT, x, y, x + w, y + h)
    else:
        raise ValueError(kind)
    sh.name = name
    if opts.get("fill") == "gradient":
        sh.fill.gradient()
        sh.fill.gradient_angle = 90
    elif opts.get("fill") == "picture":
        picture_fill(slide, sh)
    if "line" in opts:
        sh.line.width = Pt(opts["line"])
    if opts.get("noline"):
        sh.line.fill.background()
    if opts.get("shadow") is False:
        sh.shadow.inherit = False
    if "rot" in opts:
        sh.rotation = opts["rot"]
    return sh


def base_deck(slides, path):
    prs = Presentation()
    prs.slide_width, prs.slide_height = Inches(13.333), Inches(7.5)
    blank = prs.slide_layouts[6]
    for spec in slides:
        slide = prs.slides.add_slide(blank)
        shapes = spec.get("shapes", [])
        for i, (name, kind, opts) in enumerate(shapes):
            add_shape(slide, name, kind, opts, grid(i, len(shapes)))
    prs.save(path)


# --- step 2: PowerPoint adds the motion ---------------------------------------

TRIGGER = {"click": "on page click", "with": "with previous", "after": "after previous"}
LEVEL = {"first": "text by first level", "second": "text by second level", "all": "text by all levels"}


def q(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def attempt(lines, what, body):
    """One statement PowerPoint may refuse: noted, not fatal."""
    lines.append("\t\ttry")
    lines.extend("\t\t\t" + b for b in body)
    lines.append("\t\ton error msg")
    lines.append(f"\t\t\tset end of problems to {q(what)} & \": \" & msg")
    lines.append("\t\tend try")


def script(name, slides):
    L = [
        "set problems to {}",
        'tell application "Microsoft PowerPoint"',
        f"\tset pres to presentation {q(name + '.pptx')}",
    ]
    for i, spec in enumerate(slides, 1):
        L.append(f"\tset s to slide {i} of pres")
        # By position: PowerPoint's "add effect" refuses a shape found by a
        # name filter, and shapes are numbered in the order they were added.
        order = [n for n, _, _ in spec.get("shapes", [])]

        def sh(n):
            return f"shape {order.index(n) + 1} of s"

        if spec.get("fx"):
            L.append("\tset seq to main sequence of timeline of s")
        for j, (shape, effect, trigger, o) in enumerate(spec.get("fx", []), 1):
            what = f"slide {i} effect {j} ({effect} on {shape})"
            add = f"set fx to add effect seq for ({sh(shape)}) fx animation type {effect}"
            if "level" in o:
                add += f" level {LEVEL[o['level']]}"
            add += f" trigger {TRIGGER[trigger]}"
            body = ["set n0 to count of effects of seq", add]
            attempt(L, what, body)
            if o.get("exit"):
                attempt(L, what + " exit", ["set exit animation of fx to true"])
            if "dir" in o:
                attempt(L, what + " direction", [f"set direction of effect parameters of fx to {o['dir']}"])
            if "dur" in o:
                attempt(L, what + " duration", ["repeat with k from n0 + 1 to count of effects of seq",
                                                 f"\tset duration of timing of effect k of seq to {o['dur']}", "end repeat"])
            if "color" in o:
                r, g, b = o["color"]
                attempt(L, what + " color", ["repeat with k from n0 + 1 to count of effects of seq",
                                             # By index: iterating the behaviours by reference crashes PowerPoint 16.115.
                                             "\trepeat with m from 1 to count of animation behaviors of effect k of seq",
                                             "\t\tset bh to animation behavior m of effect k of seq",
                                             "\t\tif (animation behavior type of bh) is animation type color then",
                                             f"\t\t\tset to_color of colors effect of bh to {{{r}, {g}, {b}}}",
                                             "\t\tend if", "\tend repeat", "end repeat"])
            if "path" in o:
                attempt(L, what + " path", [f"set path of motion effect of animation behavior 1 of fx to {q(o['path'])}"])
            if "amount" in o:
                attempt(L, what + " amount", [f"set amount of effect parameters of fx to {o['amount']}"])
            if "size" in o:
                attempt(L, what + " size", [f"set size2 of effect parameters of fx to {o['size']}"])
            if o.get("autorev"):
                attempt(L, what + " autorev", ["set autoreverse of timing of fx to true"])
        for j, (button, shape, effect) in enumerate(spec.get("interactive", []), 1):
            attempt(L, f"slide {i} interactive {j}", [
                "set iseq to add sequence (timeline of s)",
                f"set fx to add effect iseq for ({sh(shape)}) fx animation type {effect} trigger on shape click",
            ])
        t = spec.get("transition")
        if t:
            what = f"slide {i} transition {t['effect']}"
            attempt(L, what, [f"set entry effect of slide show transition of s to entry effect {t['effect']}"])
            if "dur" in t:
                attempt(L, what + " duration", [f"set transition duration of slide show transition of s to {t['dur']}"])
            if "advance" in t:
                attempt(L, what + " advance", [
                    "set advance on time of slide show transition of s to true",
                    f"set advance time of slide show transition of s to {t['advance']}",
                ])
    # An unchanged presentation is not written: touch it, so PowerPoint saves
    # its own form of every deck, the ones without animation too.
    L += ["\tset n to name of shape 1 of slide 1 of pres", "\tset name of shape 1 of slide 1 of pres to n"]
    L += ["\tsave pres", "\tclose pres saving no", "end tell",
          "set AppleScript's text item delimiters to linefeed", "return problems as text"]
    return "\n".join(L) + "\n"


def osa(text, timeout=120):
    return subprocess.run(["osascript", "-e", text], capture_output=True, text=True, timeout=timeout)


def open_in_powerpoint(path):
    """Open in the background and wait until PowerPoint lists it."""
    name = os.path.basename(path)
    subprocess.run(["open", "-g", "-a", "Microsoft PowerPoint", path], check=True)
    for _ in range(60):
        r = osa(f'tell application "Microsoft PowerPoint" to (name of every presentation) contains {q(name)}', 20)
        if r.stdout.strip() == "true":
            return
        time.sleep(0.5)
    raise RuntimeError(f"PowerPoint did not open {path}")


def make(name):
    slides = DECKS[name]
    os.makedirs(OUT, exist_ok=True)
    path = os.path.join(OUT, name + ".pptx")
    base_deck(slides, path)
    if name == "agent-written":
        # Keep what the script wrote, with a duplicated shape id, to compare.
        raw = os.path.join(OUT, "agent-written-raw.pptx")
        duplicate_an_id(path, raw)
        base_deck(slides, path)
        duplicate_an_id(path, path)
    open_in_powerpoint(path)
    with tempfile.NamedTemporaryFile("w", suffix=".applescript", delete=False) as f:
        f.write(script(name, slides))
    r = subprocess.run(["osascript", f.name], capture_output=True, text=True, timeout=300)
    os.unlink(f.name)
    if r.returncode != 0:
        # Leave nothing of ours open in PowerPoint.
        osa(f'tell application "Microsoft PowerPoint" to close presentation {q(name + ".pptx")} saving no', 30)
        raise RuntimeError(f"{name}: {r.stderr.strip()}")
    problems = [p for p in r.stdout.strip().splitlines() if p]
    print(f"{name}: {os.path.getsize(path)} bytes, {len(slides)} slides" + (f", {len(problems)} refused" if problems else ""))
    for p in problems:
        print("  refused:", p)


def duplicate_an_id(src, dst):
    """Give two shapes of slide 2 the same id, as careless generators do."""
    import re
    import shutil
    import zipfile

    tmp = dst + ".tmp"
    with zipfile.ZipFile(src) as zin, zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED) as zout:
        for item in zin.infolist():
            data = zin.read(item.filename)
            if item.filename == "ppt/slides/slide2.xml":
                text = data.decode("utf-8")
                ids = re.findall(r'<p:cNvPr id="(\d+)"', text)
                if len(ids) >= 3:
                    text = text.replace(f'<p:cNvPr id="{ids[2]}"', f'<p:cNvPr id="{ids[1]}"', 1)
                data = text.encode("utf-8")
            zout.writestr(item, data)
    shutil.move(tmp, dst)


if __name__ == "__main__":
    names = sys.argv[1:] or list(DECKS)
    for n in names:
        make(n)
