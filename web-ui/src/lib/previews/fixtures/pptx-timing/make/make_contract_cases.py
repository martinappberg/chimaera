#!/usr/bin/env python3
"""The contract-clarification cases (R18, R21, R22, R23a, R32): python make_contract_cases.py ../cases"""
# Writes the contract-clarification cases (README R18, R21, R22, R23a, R32) into cases/.
import sys, itertools
OUT = sys.argv[1]
ids = itertools.count(3)
def sp(i, paras=1):
    ps = "".join(f'<a:p><a:r><a:rPr lang="en-US"/><a:t>P{n}</a:t></a:r></a:p>' for n in range(paras))
    return (f'<p:sp><p:nvSpPr><p:cNvPr id="{i}" name="S{i}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="{457200 + (i-2)*1828800}" y="1097280"/>'
            f'<a:ext cx="1645920" cy="2011680"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/>{ps}</p:txBody></p:sp>')
def tgt(spid, para=None):
    return f'<p:spTgt spid="{spid}"/>' if para is None else f'<p:spTgt spid="{spid}"><p:txEl><p:pRg st="{para[0]}" end="{para[1]}"/></p:txEl></p:spTgt>'
def cbhvr(t, attr, dur="1", delay=None):
    st = f'<p:stCondLst><p:cond delay="{delay}"/></p:stCondLst>' if delay is not None else ""
    return f'<p:cBhvr><p:cTn id="{next(ids)}" dur="{dur}" fill="hold">{st}</p:cTn><p:tgtEl>{t}</p:tgtEl><p:attrNameLst><p:attrName>{attr}</p:attrName></p:attrNameLst></p:cBhvr>'
def vis(t, val="visible", attr="style.visibility"):
    return f'<p:set>{cbhvr(t, attr)}<p:to><p:strVal val="{val}"/></p:to></p:set>'
def clr_by(t):
    return f'<p:animClr clrSpc="rgb" dir="cw">{cbhvr(t, "fillcolor", "500")}<p:by><p:rgb r="1000" g="0" b="0"/></p:by></p:animClr>'
def scale(t):
    return f'<p:animScale><p:cBhvr><p:cTn id="{next(ids)}" dur="500" fill="hold"/><p:tgtEl>{t}</p:tgtEl></p:cBhvr><p:by x="150000" y="150000"/></p:animScale>'
def motion(t, path, dur, delay="0"):
    return (f'<p:animMotion origin="layout" path="{path}" pathEditMode="relative" ptsTypes=""><p:cBhvr><p:cTn id="{next(ids)}" dur="{dur}" fill="hold"><p:stCondLst><p:cond delay="{delay}"/></p:stCondLst></p:cTn>'
            f'<p:tgtEl>{t}</p:tgtEl><p:attrNameLst><p:attrName>ppt_x</p:attrName><p:attrName>ppt_y</p:attrName></p:attrNameLst></p:cBhvr></p:animMotion>')
def effect(cls, preset, behaviours, node="clickEffect"):
    return (f'<p:par><p:cTn id="{next(ids)}" presetID="{preset}" presetClass="{cls}" presetSubtype="0" fill="hold" nodeType="{node}"><p:stCondLst><p:cond delay="0"/></p:stCondLst>'
            f'<p:childTnLst>{"".join(behaviours)}</p:childTnLst></p:cTn></p:par>')
def group(effects):
    return (f'<p:par><p:cTn id="{next(ids)}" fill="hold"><p:stCondLst><p:cond delay="indefinite"/></p:stCondLst><p:childTnLst><p:par><p:cTn id="{next(ids)}" fill="hold">'
            f'<p:stCondLst><p:cond delay="0"/></p:stCondLst><p:childTnLst>{"".join(effects)}</p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:par>')
def igroup(effects):
    return (f'<p:par><p:cTn id="{next(ids)}" fill="hold"><p:stCondLst><p:cond delay="0"/></p:stCondLst><p:childTnLst><p:par><p:cTn id="{next(ids)}" fill="hold">'
            f'<p:stCondLst><p:cond delay="0"/></p:stCondLst><p:childTnLst>{"".join(effects)}</p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:par>')
def slide(comment, shapes, groups, interactive=""):
    seq = (f'<p:seq concurrent="1" nextAc="seek"><p:cTn id="2" dur="indefinite" nodeType="mainSeq"><p:childTnLst>{"".join(groups)}</p:childTnLst></p:cTn>'
           '<p:prevCondLst><p:cond evt="onPrev" delay="0"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond></p:prevCondLst><p:nextCondLst><p:cond evt="onNext" delay="0"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond></p:nextCondLst></p:seq>')
    if interactive:
        seq += (f'<p:seq concurrent="1" nextAc="seek"><p:cTn id="{next(ids)}" restart="whenNotActive" fill="hold" evtFilter="cancelBubble" nodeType="interactiveSeq"><p:stCondLst><p:cond evt="onClick" delay="0"><p:tgtEl><p:spTgt spid="2"/></p:tgtEl></p:cond></p:stCondLst>'
                f'<p:endSync evt="end" delay="0"><p:rtn val="all"/></p:endSync><p:childTnLst>{interactive}</p:childTnLst></p:cTn><p:nextCondLst><p:cond evt="onNext" delay="0"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond></p:nextCondLst></p:seq>')
    return ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            f'<!-- {comment} -->\n'
            '<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">'
            '<p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>' + "".join(shapes) +
            '</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr><p:timing><p:tnLst><p:par><p:cTn id="1" dur="indefinite" restart="never" nodeType="tmRoot"><p:childTnLst>'
            + seq + '</p:childTnLst></p:cTn></p:par></p:tnLst></p:timing></p:sld>\n')
def write(name, text):
    open(f"{OUT}/{name}.xml", "w").write(text)

S3 = [sp(2), sp(3), sp(4)]
# R23a: an attrName is trimmed of XML's four whitespace characters only.
write("text-whitespace", slide("R23a: an element's text is trimmed of space, tab, LF, CR only. S2's attrName carries tab/LF/CR/space and is a visibility set; S3's carries a no-break space (&#160;) and S4's a U+FEFF, so theirs are not.", S3,
      [group([effect("entr", 1, [vis(tgt(2), attr="\t\n\r style.visibility \r\n")]), effect("entr", 1, [vis(tgt(3), attr="&#160;style.visibility")], "withEffect"), effect("entr", 1, [vis(tgt(4), attr="style.visibility&#xFEFF;")], "withEffect")])]))
# R21: the by-chain cap holds in an interactive sequence as in the main one.
write("colour-chain-interactive", slide("R21: 17 p:animClr by changes on one target and attribute within one interactive sequence: over the chain cap, the slide plays plain (colour-too-large).", S3,
      [group([effect("entr", 1, [vis(tgt(3))])])], igroup([effect("emph", 19, [clr_by(tgt(4)) for _ in range(17)])])))
write("colour-chain-split", slide("R21: 16 by changes on one target and attribute in the main sequence and 16 more in an interactive sequence: each sequence is counted on its own, so neither is over the cap.", S3,
      [group([effect("emph", 19, [clr_by(tgt(4)) for _ in range(16)])])], igroup([effect("emph", 19, [clr_by(tgt(4)) for _ in range(16)])])))
# R21: xmlns is not an attribute for the colour cap.
eight = 'val="FF0000" x1="1" x2="2" x3="3" x4="4" x5="5" x6="6" x7="7"'
to8 = (f'<p:animClr clrSpc="rgb" dir="cw">{cbhvr(tgt(2), "fillcolor", "500")}<p:to><a:srgbClr xmlns="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:q="urn:x" {eight}/></p:to></p:animClr>')
write("colour-xmlns", slide("R21: a colour with 8 unprefixed attributes plus a default namespace declaration (xmlns) and a prefixed one: declarations are not attributes, so it is within the cap.", S3,
      [group([effect("emph", 19, [to8])])]))
# R18/R10: a path's order is its behaviour's end, not its effect's.
write("path-behaviour-end", slide("R18: two paths in one effect; the first ends at 2000 ms, the second at 500 ms. The path that ends last (the first) decides, though both effects' ends are equal.", S3,
      [group([effect("path", 0, [motion(tgt(2), "M 0 0 L 0.1 0 E", "2000"), motion(tgt(2), "M 0 0 L 0 0.2 E", "500")])])]))
# R32: any behaviour on a paragraph range splits the shape, held or not.
write("split-any-behaviour", slide("R32: S2's only paragraph behaviour is a visibility set (not a held change); S2 is still split, so the whole-shape grow is held under 2:bg.", [sp(2, 2), sp(3), sp(4)],
      [group([effect("entr", 1, [vis(tgt(2, (0, 0)))])]), group([effect("emph", 6, [scale(tgt(2))])])]))
# R21/R22: the values cap. 25 click groups; group 1 has N sets on paragraphs 0-255 (256 keys x 25 states each);
# groups 2-25 one set on S3 (1 key x (26 - k) states): 300 in all.
def values_case(name, n, comment):
    groups = [group([effect("entr", 1, [vis(tgt(2, (0, 255))) for _ in range(n)])])]
    groups += [group([effect("entr", 1, [vis(tgt(3))])]) for _ in range(24)]
    write(name, slide(comment, [sp(2, 2), sp(3), sp(4)], groups))
values_case("values-cap", 40, "R21/R22: 40 x 256 x 25 + 300 = 256,300 values, over 250,000: the slide plays plain (too-many-values).")
values_case("values-under-cap", 39, "R21: 39 x 256 x 25 + 300 = 249,900 values, within 250,000: the slide is read.")
