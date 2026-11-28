"""Writes the hand-written timing cases, in the structure PowerPoint uses
(modelled on the corpus decks' slide XML)."""
import os, sys
OUT = sys.argv[1]
HEAD = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
NS = ('xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" '
      'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" '
      'xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"')

class Ids:
    def __init__(self): self.n = 0
    def __call__(self):
        self.n += 1
        return self.n

def shape(i, name, x, paras=None, fill=False):
    paras = paras or [name]
    body = ''.join(f'<a:p><a:r><a:rPr lang="en-US" dirty="0"/><a:t>{t}</a:t></a:r></a:p>' for t in paras)
    sppr_fill = '<a:solidFill><a:srgbClr val="4472C4"/></a:solidFill>' if fill else ''
    return (f'<p:sp><p:nvSpPr><p:cNvPr id="{i}" name="{name}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>'
            f'<p:spPr><a:xfrm><a:off x="{457200 + x * 1828800}" y="1097280"/><a:ext cx="1645920" cy="2011680"/></a:xfrm>'
            f'<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>{sppr_fill}</p:spPr>'
            f'<p:txBody><a:bodyPr/><a:lstStyle/>{body}</p:txBody></p:sp>')

def slide(shapes, after_clr='', timing=''):
    tree = ('<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>'
            '<p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>')
    return (HEAD + f'<p:sld {NS}><p:cSld><p:spTree>{tree}{"".join(shapes)}</p:spTree></p:cSld>'
            f'<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr>{after_clr}{timing}</p:sld>\n')

def tgt(spid, bg=False, para=None):
    inner = '<p:bg/>' if bg else (f'<p:txEl><p:pRg st="{para}" end="{para}"/></p:txEl>' if para is not None else '')
    return f'<p:tgtEl><p:spTgt spid="{spid}">{inner}</p:spTgt></p:tgtEl>' if inner else f'<p:tgtEl><p:spTgt spid="{spid}"/></p:tgtEl>'

def ctn_attrs(**kw):
    return ''.join(f' {k.replace("_", "")}="{v}"' for k, v in kw.items() if v is not None)

def setvis(ids, t, to, delay=0):
    return (f'<p:set><p:cBhvr><p:cTn id="{ids()}" dur="1" fill="hold"><p:stCondLst><p:cond delay="{delay}"/></p:stCondLst></p:cTn>'
            f'{t}<p:attrNameLst><p:attrName>style.visibility</p:attrName></p:attrNameLst></p:cBhvr>'
            f'<p:to><p:strVal val="{to}"/></p:to></p:set>')

def fade(ids, t, dur='500', transition='in'):
    return f'<p:animEffect transition="{transition}" filter="fade"><p:cBhvr><p:cTn id="{ids()}" dur="{dur}"/>{t}</p:cBhvr></p:animEffect>'

def rot(ids, t, by, dur, delay=None, **kw):
    cond = f'<p:stCondLst><p:cond delay="{delay}"/></p:stCondLst>' if delay is not None else ''
    a = ctn_attrs(**kw)
    inner = f'<p:cTn id="{ids()}" dur="{dur}" fill="hold"{a}>{cond}</p:cTn>' if cond else f'<p:cTn id="{ids()}" dur="{dur}" fill="hold"{a}/>'
    return f'<p:animRot by="{by}"><p:cBhvr>{inner}{t}<p:attrNameLst><p:attrName>r</p:attrName></p:attrNameLst></p:cBhvr></p:animRot>'

def scale(ids, t, dur, **kw):
    return (f'<p:animScale><p:cBhvr><p:cTn id="{ids()}" dur="{dur}" fill="hold"{ctn_attrs(**kw)}/>{t}</p:cBhvr>'
            f'<p:by x="150000" y="150000"/></p:animScale>')

def motion(ids, t, path, dur=2000):
    return (f'<p:animMotion origin="layout" path="{path}" pathEditMode="relative" ptsTypes="">'
            f'<p:cBhvr><p:cTn id="{ids()}" dur="{dur}" fill="hold"/>{t}<p:attrNameLst><p:attrName>ppt_x</p:attrName>'
            f'<p:attrName>ppt_y</p:attrName></p:attrNameLst></p:cBhvr></p:animMotion>')

def effect(ids, cls, preset, node, behaviours, subtype=0, grp=0, **kw):
    a = f' presetID="{preset}" presetClass="{cls}" presetSubtype="{subtype}"'
    if cls is None: a = ''
    n = f' nodeType="{node}"' if node is not None else ''
    g = f' grpId="{grp}"' if grp is not None else ''
    return (f'<p:par><p:cTn id="{ids()}"{a} fill="hold"{g}{n}{ctn_attrs(**kw)}><p:stCondLst><p:cond delay="0"/></p:stCondLst>'
            f'<p:childTnLst>{"".join(behaviours)}</p:childTnLst></p:cTn></p:par>')

def sub(ids, delay, effects):
    return (f'<p:par><p:cTn id="{ids()}" fill="hold"><p:stCondLst><p:cond delay="{delay}"/></p:stCondLst>'
            f'<p:childTnLst>{"".join(effects)}</p:childTnLst></p:cTn></p:par>')

def group(ids, subs, delay='indefinite'):
    return (f'<p:par><p:cTn id="{ids()}" fill="hold"><p:stCondLst><p:cond delay="{delay}"/></p:stCondLst>'
            f'<p:childTnLst>{"".join(subs)}</p:childTnLst></p:cTn></p:par>')

NAV = ('<p:prevCondLst><p:cond evt="onPrev" delay="0"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond></p:prevCondLst>'
       '<p:nextCondLst><p:cond evt="onNext" delay="0"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond></p:nextCondLst>')

def timing(ids_first, groups, bld='', extra_seqs=''):
    # ids 1 and 2 are the root and the main sequence, as PowerPoint numbers them.
    return ('<p:timing><p:tnLst><p:par><p:cTn id="1" dur="indefinite" restart="never" nodeType="tmRoot"><p:childTnLst>'
            f'<p:seq concurrent="1" nextAc="seek"><p:cTn id="2" dur="indefinite" nodeType="mainSeq"><p:childTnLst>{"".join(groups)}'
            f'</p:childTnLst></p:cTn>{NAV}</p:seq>{extra_seqs}</p:childTnLst></p:cTn></p:par></p:tnLst>'
            f'{"<p:bldLst>" + bld + "</p:bldLst>" if bld else ""}</p:timing>')

def bldp(*spids, extra=''):
    return ''.join(f'<p:bldP spid="{s}" grpId="0" animBg="1"{extra}/>' for s in spids)

def write(name, xml):
    with open(os.path.join(OUT, name), 'w') as f: f.write(xml)

def new_ids():
    i = Ids(); i.n = 2; return i

# --- the interactive case: a trigger reveals 3; the main sequence later shows 3 too.
ids = new_ids()
main = [
    group(ids, [sub(ids, 0, [effect(ids, 'entr', 1, 'clickEffect', [setvis(ids, tgt(4), 'visible')])])]),
    group(ids, [sub(ids, 0, [effect(ids, 'entr', 1, 'clickEffect', [setvis(ids, tgt(3), 'visible')], grp=1)])]),
]
iid = ids()
inter = (f'<p:seq concurrent="1" nextAc="seek"><p:cTn id="{iid}" restart="whenNotActive" fill="hold" evtFilter="cancelBubble" nodeType="interactiveSeq">'
         f'<p:stCondLst><p:cond evt="onClick" delay="0">{tgt(2)}</p:cond></p:stCondLst>'
         '<p:endSync evt="end" delay="0"><p:rtn val="all"/></p:endSync><p:childTnLst>'
         + group(ids, [sub(ids, 0, [effect(ids, 'entr', 10, 'clickEffect', [setvis(ids, tgt(3), 'visible'), fade(ids, tgt(3))])])], delay='0')
         + f'</p:childTnLst></p:cTn><p:nextCondLst><p:cond evt="onClick" delay="0">{tgt(2)}</p:cond></p:nextCondLst></p:seq>')
write('interactive-trigger.xml', slide([shape(2, 'Button', 0), shape(3, 'Reveal', 1), shape(4, 'Clicked', 2)],
      timing=timing(ids, main, bld='<p:bldP spid="3" grpId="0" animBg="1"/><p:bldP spid="4" grpId="0" animBg="1"/><p:bldP spid="3" grpId="1" animBg="1"/>', extra_seqs=inter)))

# --- a filled text box built by paragraph: its background, then paragraphs; then the whole shape leaves.
ids = new_ids()
main = [
    group(ids, [sub(ids, 0, [
        effect(ids, 'entr', 10, 'clickEffect', [setvis(ids, tgt(2, bg=True), 'visible'), fade(ids, tgt(2, bg=True))]),
        effect(ids, 'entr', 10, 'withEffect', [setvis(ids, tgt(2, para=0), 'visible'), fade(ids, tgt(2, para=0))]),
    ])]),
    group(ids, [sub(ids, 0, [effect(ids, 'entr', 10, 'clickEffect', [setvis(ids, tgt(2, para=1), 'visible'), fade(ids, tgt(2, para=1))])])]),
    group(ids, [sub(ids, 0, [effect(ids, 'exit', 10, 'clickEffect', [fade(ids, tgt(2), transition='out'), setvis(ids, tgt(2), 'hidden', delay=499)], grp=1)])]),
]
write('bg-paragraphs.xml', slide([shape(2, 'Box', 0, paras=['First', 'Second'], fill=True), shape(3, 'Still', 2)],
      timing=timing(ids, main, bld='<p:bldP spid="2" grpId="0" build="p" animBg="1"/><p:bldP spid="2" grpId="1" animBg="1"/>')))

# --- repeatCount and autoRev, on behaviours and on effects.
ids = new_ids()
def emph(preset, behaviours, **kw):
    return group(ids, [sub(ids, 0, [effect(ids, 'emph', preset, 'clickEffect', behaviours, **kw)])])
main = [
    emph(8, [rot(ids, tgt(2), 21600000, 500, repeatCount='2000')]),                          # 1000
    emph(8, [rot(ids, tgt(3), 21600000, 500, autoRev='1'), rot(ids, tgt(3), 60000, 100, delay=200)]),  # 1000
    emph(6, [scale(ids, tgt(4), 500)], repeatCount='3000', autoRev='1'),                      # 3000
    emph(8, [rot(ids, tgt(5), 21600000, 333, repeatCount='1500')]),                           # 499.5 -> 500
    emph(8, [rot(ids, tgt(6), 21600000, 333, repeatCount='1500')], repeatCount='1500'),       # 749.25 -> 749
    emph(8, [rot(ids, tgt(2), 21600000, 500)], repeatCount='indefinite'),                     # null
    emph(8, [rot(ids, tgt(3), 21600000, 500, repeatCount='indefinite'), rot(ids, tgt(3), 60000, 100)]),  # null
]
write('repeat-autorev.xml', slide([shape(i, f'S{i}', i - 2) for i in range(2, 7)],
      timing=timing(ids, main, bld=bldp(2, 3, 4, 5, 6))))

# --- effect nodes without nodeType: first in a group is a click, later is with.
ids = new_ids()
main = [
    group(ids, [sub(ids, 0, [
        effect(ids, 'entr', 1, None, [setvis(ids, tgt(2), 'visible')]),
        effect(ids, 'entr', 1, None, [setvis(ids, tgt(3), 'visible')]),
    ])]),
    group(ids, [sub(ids, 0, [effect(ids, 'entr', 1, 'clickEffect', [setvis(ids, tgt(4), 'visible')])]),
                sub(ids, 0, [effect(ids, 'entr', 1, None, [setvis(ids, tgt(5), 'visible')])])]),
]
write('default-trigger.xml', slide([shape(i, f'S{i}', i - 2) for i in range(2, 6)], timing=timing(ids, main, bld=bldp(2, 3, 4, 5))))

# --- empty click groups are skipped, not clicks.
ids = new_ids()
main = [
    group(ids, [sub(ids, 0, [])]),
    group(ids, [sub(ids, 0, [effect(ids, 'entr', 1, 'clickEffect', [setvis(ids, tgt(2), 'visible')])])]),
    group(ids, []),
    group(ids, [sub(ids, 0, [effect(ids, 'entr', 1, 'clickEffect', [setvis(ids, tgt(3), 'visible')])])]),
]
write('empty-group.xml', slide([shape(2, 'A', 0), shape(3, 'B', 1)], timing=timing(ids, main, bld=bldp(2, 3))))

# --- nesting past the cap: a plain slide, transition still read.
ids = new_ids()
inner = effect(ids, 'entr', 1, 'clickEffect', [setvis(ids, tgt(2), 'visible')])
for _ in range(30):
    inner = sub(ids, 0, [inner])
main = [group(ids, [inner])]
write('over-nesting.xml', slide([shape(2, 'Deep', 0)],
      after_clr='<p:transition spd="med"><p:fade/></p:transition>', timing=timing(ids, main, bld=bldp(2))))

# --- transitions.
write('transition-plain.xml', slide([shape(2, 'A', 0)],
      after_clr='<p:transition spd="med" advClick="0" advTm="4000"><p:wipe dir="d"/></p:transition>'))
write('transition-unknown-choice.xml', slide([shape(2, 'A', 0)], after_clr=(
    '<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">'
    '<mc:Choice xmlns:p15="http://schemas.microsoft.com/office/powerpoint/2012/main" Requires="p15">'
    '<p:transition spd="slow" xmlns:p14="http://schemas.microsoft.com/office/powerpoint/2010/main" p14:dur="1250">'
    '<p15:prstTrans prst="curtains"/></p:transition></mc:Choice>'
    '<mc:Fallback><p:transition spd="slow"><p:fade/></p:transition></mc:Fallback></mc:AlternateContent>')))

# --- path edge cases.
ids = new_ids()
def path_click(spid, *paths):
    return group(ids, [sub(ids, 0, [effect(ids, 'path', 0, 'clickEffect', [motion(ids, tgt(spid), p)]) for p in paths[:1]]
                        + [effect(ids, 'path', 0, 'withEffect', [motion(ids, tgt(spid), p)]) for p in paths[1:]])])
main = [
    path_click(2, 'M 0 0 0.1 0.1 Z'),                 # [0, 0]: a second M pair is a line-to
    path_click(3, 'M 0 0 L 0.1 0.2 X 5 5 L 1 1 E'),   # [0.1, 0.2]: unknown command stops
    path_click(4, 'M 0 0 L 0.00025 -0.00025'),        # [0.0003, -0.0003]: half away from zero, no E
    path_click(5, 'M 0 0 L -0.00001 0 E'),            # [0, 0]: never -0
    path_click(6, 'm 0 0 l 0.1 0 l 0 0.1 e'),         # [0.1, 0.1]: relative
    path_click(7, 'M 0 0 L 0.00004 0 E', 'M 0 0 L 0.00004 0 E'),  # [0.0001, 0]: summed, then rounded
]
write('path-edges.xml', slide([shape(i, f'S{i}', i - 2) for i in range(2, 8)], timing=timing(ids, main, bld=bldp(2, 3, 4, 5, 6, 7))))

# --- hostile strings: none of these may reach a prototype or a lookup.
ids = new_ids()
main = [
    group(ids, [sub(ids, 0, [
        effect(ids, 'entr', 1, 'constructor', [setvis(ids, tgt(2), 'visible')]),
        effect(ids, 'entr', 10, '__proto__', [setvis(ids, tgt('__proto__'), 'visible'), fade(ids, tgt('__proto__'))]),
        effect(ids, '__proto__', 1, 'withEffect', [setvis(ids, tgt('toString'), 'visible')]),
        effect(ids, 'entr', 1, 'withEffect', [setvis(ids, tgt('12345678901'), 'visible')]),
        effect(ids, 'entr', 1, 'withEffect', [setvis(ids, tgt(' 3'), 'visible')]),
    ])]),
    group(ids, [sub(ids, ' 500', [effect(ids, 'entr', 10, 'clickEffect', [setvis(ids, tgt(3), 'visible'), fade(ids, tgt(3), dur='1e3')])])]),
    group(ids, [sub(ids, 0, [effect(ids, 'path', 0, 'clickEffect', [motion(ids, tgt('__proto__'), 'M 0 0 L 0.5 0 E')])])]),
]
write('hostile-strings.xml', slide([shape(2, 'A', 0), shape(3, 'B', 1)],
      after_clr='<p:transition spd="__proto__"><p:fade/></p:transition>',
      timing=timing(ids, main, bld='<p:bldP spid="__proto__" grpId="0" animBg="1"/>' + bldp(2, 3))))

# --- p:anim keyframes, held and played by the same rules (R28): a time
# several tavs share takes the last of them, discrete keeps the earlier,
# from/to without tavs, and what contributes nothing. Each effect repeats
# half a time, so its behaviours end at progress 0.5.
ids = new_ids()
def anim(t, attr, tavs=None, dur=1000, **kw):
    a = ctn_attrs(**{k: v for k, v in kw.items() if k in ('calcmode', 'valueType', 'from_', 'to', 'by')})
    a = a.replace(' from=', ' from=')
    lst = ''
    if tavs is not None:
        lst = '<p:tavLst>' + ''.join(
            (f'<p:tav tm="{tm}">' if tm is not None else '<p:tav>') + f'<p:val><p:fltVal val="{v}"/></p:val></p:tav>'
            for tm, v in tavs) + '</p:tavLst>'
    return (f'<p:anim{a}><p:cBhvr><p:cTn id="{ids()}" dur="{dur}" fill="hold"/>{t}'
            f'<p:attrNameLst><p:attrName>{attr}</p:attrName></p:attrNameLst></p:cBhvr>{lst}</p:anim>')
def half(behaviours):
    return group(ids, [sub(ids, 0, [effect(ids, 'emph', 9, 'clickEffect', behaviours, repeatCount='500')])])
main = [
    half([anim(tgt(2), 'style.rotation', [(0, 0), (50000, 90), (50000, 180), (100000, 180)], calcmode='lin', valueType='num')]),  # 180
    half([anim(tgt(3), 'style.opacity', from_='1', to='0.5', calcmode='lin', valueType='num')]),                                 # 0.75
    half([anim(tgt(4), 'style.opacity', [(0, 1), (60000, 0.5), (100000, 0.2)], calcmode='discrete', valueType='num')]),         # 1: held, nothing written
    half([anim(tgt(4), 'style.opacity', [(0, 0.4), (60000, 0.6), (100000, 0.6)], calcmode='discrete', valueType='num')]),       # 0.4
    half([anim(tgt(5), 'style.opacity', [(0, 1), (80000, 0.5), (40000, 0.2)], calcmode='lin', valueType='num')]),               # nothing: out of order
    half([anim(tgt(6), 'style.opacity', [(0, 0.5), (100000, 0.5)], calcmode='lin', valueType='str')]),                          # nothing: not num
    half([anim(tgt(6), 'style.rotation', [(None, 0), (None, 40), (None, 80)], calcmode='lin', valueType='num')]),                # 40: spread evenly
]
write('anim-keys.xml', slide([shape(i, f'S{i}', i - 2) for i in range(2, 7)], timing=timing(ids, main, bld=bldp(2, 3, 4, 5, 6))))

# --- held values on overlapping paragraph ranges (R31: kept per paragraph,
# the change that ends later wins) and a whole-shape fill colour on a
# shape whose text is animated by paragraph (R32: its body, never its text).
ids = new_ids()
def clr(t, attr, val, dur=500):
    return (f'<p:animClr clrSpc="rgb" dir="cw"><p:cBhvr override="childStyle"><p:cTn id="{ids()}" dur="{dur}" fill="hold"/>{t}'
            f'<p:attrNameLst><p:attrName>{attr}</p:attrName></p:attrNameLst></p:cBhvr><p:to><a:srgbClr val="{val}"/></p:to></p:animClr>')
def rng(spid, st, end):
    return f'<p:tgtEl><p:spTgt spid="{spid}"><p:txEl><p:pRg st="{st}" end="{end}"/></p:txEl></p:spTgt></p:tgtEl>'
def emph_click(preset, behaviours):
    return group(ids, [sub(ids, 0, [effect(ids, 'emph', preset, 'clickEffect', behaviours, subtype=2)])])
main = [
    emph_click(3, [clr(rng(2, 0, 1), 'style.color', 'FF0000')]),
    emph_click(3, [clr(rng(2, 1, 2), 'style.color', '0000FF')]),
    emph_click(3, [clr(tgt(2), 'style.color', '00FF00')]),
    emph_click(1, [clr(tgt(2), 'fillcolor', 'FFC000')]),
    emph_click(3, [clr(rng(3, 0, 2), 'style.color', '0000FF', dur=1000), clr(rng(3, 1, 1), 'style.color', 'FF0000')]),
]
write('paragraph-order.xml', slide([shape(2, 'Split', 0, paras=['One', 'Two', 'Three'], fill=True),
                                    shape(3, 'Same click', 1, paras=['One', 'Two', 'Three'])],
      timing=timing(ids, main, bld='<p:bldP spid="2" grpId="0" build="p" animBg="1"/><p:bldP spid="3" grpId="0" build="p"/>')))
