#!/usr/bin/env python3
"""Writes the flat-fill deck behind colors.json: sixteen rectangles filled
with accent1 (4F81BD) through one or two colour modifiers each, no line,
no shadow. Export it from PowerPoint as a movie (Save As Movie) and read
each rectangle's middle to get the `film` values (BT.709 limited-range
decode reads about 3 low per channel).

    python make_colors.py out.pptx
"""
import sys
from lxml import etree
from pptx import Presentation
from pptx.enum.shapes import MSO_SHAPE
from pptx.oxml.ns import qn
from pptx.util import Inches

MODS = [("tint25", [("tint", 25000)]), ("tint50", [("tint", 50000)]), ("tint75", [("tint", 75000)]),
        ("shade25", [("shade", 25000)]), ("shade50", [("shade", 50000)]), ("shade75", [("shade", 75000)]),
        ("satMod130", [("satMod", 130000)]), ("lumMod75", [("lumMod", 75000)]),
        ("lumMod60off40", [("lumMod", 60000), ("lumOff", 40000)]), ("lumMod40off60", [("lumMod", 40000), ("lumOff", 60000)]),
        ("lumMod50", [("lumMod", 50000)]), ("plain", []), ("tint50satMod130", [("tint", 50000), ("satMod", 130000)]),
        ("shade94satMod135", [("shade", 94000), ("satMod", 135000)]), ("tint100", [("tint", 100000)]), ("shade100", [("shade", 100000)])]

prs = Presentation()
prs.slide_width, prs.slide_height = Inches(13.333), Inches(7.5)
s = prs.slides.add_slide(prs.slide_layouts[6])
for i, (name, mods) in enumerate(MODS):
    r, c = divmod(i, 4)
    sh = s.shapes.add_shape(MSO_SHAPE.RECTANGLE, Inches(0.5 + c * 3.2), Inches(0.4 + r * 1.8), Inches(3.0), Inches(1.6))
    sh.name = name
    sh.line.fill.background()
    sh.shadow.inherit = False
    sp = sh._element.spPr
    for t in ("a:solidFill", "a:gradFill", "a:noFill"):
        for el in sp.findall(qn(t)):
            sp.remove(el)
    fill = etree.Element(qn("a:solidFill"))
    clr = etree.SubElement(fill, qn("a:schemeClr"))
    clr.set("val", "accent1")
    for m, v in mods:
        etree.SubElement(clr, qn("a:" + m)).set("val", str(v))
    sp.find(qn("a:prstGeom")).addnext(fill)
prs.save(sys.argv[1])
