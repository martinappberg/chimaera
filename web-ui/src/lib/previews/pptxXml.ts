/**
 * Minimal-diff edits to a PowerPoint part's XML, applied as text splices.
 *
 * The viewer's edits (move, resize, text, slide order, delete) must leave
 * every byte they don't mean to change exactly as it was: the file is the
 * truth, a git diff of the extracted package should show only the edit, and
 * PowerPoint must read the result as its own. So nothing here re-serializes
 * a DOM — `XMLSerializer` would rewrite namespace declarations, quoting,
 * entities and whitespace all over the part. Instead `scan` builds an element
 * tree that carries source offsets, each patcher computes splices against
 * those offsets, and `applySplices` rewrites only those spans.
 *
 * Names resolve through the `xmlns` declarations in scope, so a part that
 * binds DrawingML to a prefix other than `a:` still edits correctly; strict
 * OOXML namespaces are accepted beside the transitional ones. A part with a
 * DOCTYPE (never written by Office) or that doesn't scan cleanly throws
 * `PptxXmlError`, and the editor then leaves the deck read-only.
 */

export class PptxXmlError extends Error {}

const NS_P = new Set([
  "http://schemas.openxmlformats.org/presentationml/2006/main",
  "http://purl.oclc.org/ooxml/presentationml/main",
]);
const NS_A = new Set([
  "http://schemas.openxmlformats.org/drawingml/2006/main",
  "http://purl.oclc.org/ooxml/drawingml/main",
]);
const NS_MC = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const A_URI = "http://schemas.openxmlformats.org/drawingml/2006/main";

export interface XmlAttr {
  /** Qualified name as written. */
  qname: string;
  /** Offsets of the value between its quotes. */
  valueStart: number;
  valueEnd: number;
}

export interface XmlEl {
  qname: string;
  local: string;
  /** Namespace URI ("" when unbound). */
  ns: string;
  /** Offset of `<`. */
  start: number;
  /** Offset just past the start tag's `>`. */
  openEnd: number;
  /** Offset of the end tag's `<` (`openEnd` for a self-closed element). */
  closeStart: number;
  /** Offset just past the element. */
  end: number;
  selfClosing: boolean;
  attrs: XmlAttr[];
  children: XmlEl[];
  parent: XmlEl | null;
  /** Prefix → URI in scope at this element (own declarations included). */
  scope: Map<string, string>;
}

export interface Splice {
  start: number;
  end: number;
  text: string;
}

// --- scanning --------------------------------------------------------------------

function fail(msg: string): never {
  throw new PptxXmlError(msg);
}

const NAME_END = /[\s/>]/;

/** Parse `xml` into an offset-carrying element tree; returns the root. */
export function scan(xml: string): XmlEl {
  const stack: XmlEl[] = [];
  let root: XmlEl | null = null;
  let i = 0;
  const n = xml.length;
  while (i < n) {
    const lt = xml.indexOf("<", i);
    if (lt < 0) break;
    if (xml.startsWith("<?", lt)) {
      const e = xml.indexOf("?>", lt + 2);
      if (e < 0) fail("unterminated processing instruction");
      i = e + 2;
      continue;
    }
    if (xml.startsWith("<!--", lt)) {
      const e = xml.indexOf("-->", lt + 4);
      if (e < 0) fail("unterminated comment");
      i = e + 3;
      continue;
    }
    if (xml.startsWith("<![CDATA[", lt)) {
      const e = xml.indexOf("]]>", lt + 9);
      if (e < 0) fail("unterminated CDATA");
      i = e + 3;
      continue;
    }
    if (xml.startsWith("<!", lt)) fail("a DOCTYPE or declaration is not supported");
    if (xml[lt + 1] === "/") {
      const gt = xml.indexOf(">", lt);
      if (gt < 0) fail("unterminated end tag");
      const qname = xml.slice(lt + 2, gt).trim();
      const top = stack.pop();
      if (top === undefined || top.qname !== qname) fail(`mismatched end tag </${qname}>`);
      top.closeStart = lt;
      top.end = gt + 1;
      i = gt + 1;
      continue;
    }
    // Start or empty tag.
    let j = lt + 1;
    while (j < n && !NAME_END.test(xml[j]!)) j++;
    const qname = xml.slice(lt + 1, j);
    if (qname === "") fail("empty tag name");
    const attrs: XmlAttr[] = [];
    let selfClosing = false;
    for (;;) {
      while (j < n && /\s/.test(xml[j]!)) j++;
      if (j >= n) fail("unterminated start tag");
      const c = xml[j];
      if (c === ">") {
        j++;
        break;
      }
      if (c === "/") {
        if (xml[j + 1] !== ">") fail("stray / in tag");
        selfClosing = true;
        j += 2;
        break;
      }
      const ns = j;
      while (j < n && !/[\s=/>]/.test(xml[j]!)) j++;
      const aname = xml.slice(ns, j);
      while (j < n && /\s/.test(xml[j]!)) j++;
      if (xml[j] !== "=") fail(`attribute ${aname} without a value`);
      j++;
      while (j < n && /\s/.test(xml[j]!)) j++;
      const q = xml[j];
      if (q !== '"' && q !== "'") fail(`unquoted attribute ${aname}`);
      const vEnd = xml.indexOf(q, j + 1);
      if (vEnd < 0) fail("unterminated attribute value");
      attrs.push({ qname: aname, valueStart: j + 1, valueEnd: vEnd });
      j = vEnd + 1;
    }
    const parent = stack[stack.length - 1] ?? null;
    let scope = parent?.scope ?? new Map<string, string>();
    for (const a of attrs) {
      if (a.qname === "xmlns" || a.qname.startsWith("xmlns:")) {
        if (scope === parent?.scope) scope = new Map(scope);
        scope.set(a.qname === "xmlns" ? "" : a.qname.slice(6), decodeEntities(xml.slice(a.valueStart, a.valueEnd)));
      }
    }
    const colon = qname.indexOf(":");
    const prefix = colon < 0 ? "" : qname.slice(0, colon);
    const el: XmlEl = {
      qname,
      local: colon < 0 ? qname : qname.slice(colon + 1),
      ns: scope.get(prefix) ?? "",
      start: lt,
      openEnd: j,
      closeStart: j,
      end: j,
      selfClosing,
      attrs,
      children: [],
      parent,
      scope,
    };
    if (parent !== null) parent.children.push(el);
    else if (root === null) root = el;
    else fail("more than one root element");
    if (!selfClosing) stack.push(el);
    i = j;
  }
  if (stack.length > 0) fail(`unclosed element <${stack[stack.length - 1]!.qname}>`);
  if (root === null) fail("no root element");
  return root;
}

export function applySplices(xml: string, splices: readonly Splice[]): string {
  const sorted = [...splices].sort((a, b) => a.start - b.start || a.end - b.end);
  for (let k = 1; k < sorted.length; k++) {
    if (sorted[k]!.start < sorted[k - 1]!.end) fail("overlapping edits");
  }
  let out = "";
  let at = 0;
  for (const s of sorted) {
    out += xml.slice(at, s.start) + s.text;
    at = s.end;
  }
  return out + xml.slice(at);
}

// --- entities -------------------------------------------------------------------

const NAMED: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'" };

export function decodeEntities(raw: string): string {
  return decodeWithMap(raw).text;
}

/** Decoded text plus, for each decoded UTF-16 index (and one past the end),
 *  its offset in `raw`, so an edit to decoded text maps back to raw bytes. */
export function decodeWithMap(raw: string): { text: string; map: number[] } {
  let text = "";
  const map: number[] = [];
  let i = 0;
  while (i < raw.length) {
    const c = raw[i]!;
    if (c === "&") {
      const semi = raw.indexOf(";", i);
      const body = semi < 0 ? "" : raw.slice(i + 1, semi);
      let ch: string | null = null;
      if (body.startsWith("#x") || body.startsWith("#X")) {
        const cp = Number.parseInt(body.slice(2), 16);
        if (Number.isFinite(cp) && cp >= 0 && cp <= 0x10ffff) ch = String.fromCodePoint(cp);
      } else if (body.startsWith("#")) {
        const cp = Number.parseInt(body.slice(1), 10);
        if (Number.isFinite(cp) && cp >= 0 && cp <= 0x10ffff) ch = String.fromCodePoint(cp);
      } else if (body in NAMED) ch = NAMED[body]!;
      if (ch !== null) {
        for (let k = 0; k < ch.length; k++) map.push(i);
        text += ch;
        i = semi + 1;
        continue;
      }
    }
    map.push(i);
    text += c;
    i++;
  }
  map.push(raw.length);
  return { text, map };
}

/** Escape text for element content. Control characters XML 1.0 can't carry
 *  are dropped (a pasted form feed would make the part unreadable). */
export function escapeText(s: string): string {
  return s
    .replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f￾￿]/g, "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

// --- tree helpers -------------------------------------------------------------------

function isP(el: XmlEl, local: string): boolean {
  return el.local === local && NS_P.has(el.ns);
}
function isA(el: XmlEl, local: string): boolean {
  return el.local === local && NS_A.has(el.ns);
}
function childP(el: XmlEl, local: string): XmlEl | undefined {
  return el.children.find((c) => isP(c, local));
}
function childA(el: XmlEl, local: string): XmlEl | undefined {
  return el.children.find((c) => isA(c, local));
}
export function attr(xml: string, el: XmlEl, qname: string): string | null {
  const a = el.attrs.find((x) => x.qname === qname);
  return a === undefined ? null : decodeEntities(xml.slice(a.valueStart, a.valueEnd));
}
function* walk(el: XmlEl): Generator<XmlEl> {
  yield el;
  for (const c of el.children) yield* walk(c);
}

/** The qualified name for `local` in DrawingML, as this part spells it at `at`;
 *  null when no prefix is bound (the caller then declares one). */
function aName(at: XmlEl, local: string): string | null {
  for (const [prefix, uri] of at.scope) if (NS_A.has(uri)) return prefix === "" ? local : `${prefix}:${local}`;
  return null;
}

const SHAPE_KINDS = new Set(["sp", "pic", "graphicFrame", "cxnSp", "grpSp", "contentPart"]);

/** The shape element's `cNvPr` id, if it is a shape. */
function shapeId(xml: string, el: XmlEl): string | null {
  if (!SHAPE_KINDS.has(el.local) || !NS_P.has(el.ns)) return null;
  const nv = el.children.find((c) => c.local.startsWith("nv") && NS_P.has(c.ns));
  const c = nv === undefined ? undefined : childP(nv, "cNvPr");
  return c === undefined ? null : attr(xml, c, "id");
}

/**
 * Every element for shape `id` in the part. Usually one; a shape inside
 * `mc:AlternateContent` exists once per branch (Choice and Fallback), and an
 * edit has to land in each or PowerPoint and older readers disagree.
 */
export function findShapes(xml: string, root: XmlEl, id: string): XmlEl[] {
  const out: XmlEl[] = [];
  for (const el of walk(root)) if (shapeId(xml, el) === id) out.push(el);
  return out;
}

// --- geometry -------------------------------------------------------------------

export interface Xfrm {
  x: number;
  y: number;
  cx: number;
  cy: number;
}

/** The element that holds the shape's `a:xfrm`, and the xfrm if present. */
function xfrmHost(el: XmlEl): { host: XmlEl | null; xfrm: XmlEl | null } {
  if (el.local === "graphicFrame") {
    const x = childP(el, "xfrm") ?? null;
    return { host: el, xfrm: x };
  }
  const host = (el.local === "grpSp" ? childP(el, "grpSpPr") : childP(el, "spPr")) ?? null;
  return { host, xfrm: host === null ? null : (childA(host, "xfrm") ?? null) };
}

function intAttr(xml: string, el: XmlEl | undefined, name: string): number | null {
  if (el === undefined) return null;
  const v = attr(xml, el, name);
  if (v === null || !/^-?\d+$/.test(v.trim())) return null;
  return Number(v.trim());
}

/** The shape's own offset and extent in EMU; null when it inherits them
 *  (a placeholder positioned by its layout) or the part doesn't say. */
export function readXfrm(xml: string, id: string): Xfrm | null {
  const root = scan(xml);
  const el = findShapes(xml, root, id)[0];
  if (el === undefined) return null;
  const { xfrm } = xfrmHost(el);
  if (xfrm === null) return null;
  const off = childA(xfrm, "off");
  const ext = childA(xfrm, "ext");
  const x = intAttr(xml, off, "x");
  const y = intAttr(xml, off, "y");
  const cx = intAttr(xml, ext, "cx");
  const cy = intAttr(xml, ext, "cy");
  if (x === null || y === null || cx === null || cy === null) return null;
  return { x, y, cx, cy };
}

function setAttrSplices(xml: string, el: XmlEl, values: Record<string, string>): Splice[] {
  const out: Splice[] = [];
  const missing: string[] = [];
  for (const [name, v] of Object.entries(values)) {
    const a = el.attrs.find((x) => x.qname === name);
    if (a === undefined) missing.push(`${name}="${v}"`);
    else if (xml.slice(a.valueStart, a.valueEnd) !== v) out.push({ start: a.valueStart, end: a.valueEnd, text: v });
  }
  if (missing.length > 0) {
    // Before `/>` or `>`, after the last attribute.
    const at = el.selfClosing ? el.openEnd - 2 : el.openEnd - 1;
    out.push({ start: at, end: at, text: ` ${missing.join(" ")}` });
  }
  return out;
}

/** Splices that set shape `id`'s offset and extent (integer EMU). */
export function setXfrmSplices(xml: string, root: XmlEl, id: string, to: Xfrm): Splice[] {
  for (const v of [to.x, to.y, to.cx, to.cy]) if (!Number.isInteger(v)) fail("geometry must be whole EMU");
  if (to.cx < 0 || to.cy < 0) fail("a negative size");
  const shapes = findShapes(xml, root, id);
  if (shapes.length === 0) fail(`no shape ${id}`);
  const out: Splice[] = [];
  for (const el of shapes) {
    const { host, xfrm } = xfrmHost(el);
    if (host === null) fail(`shape ${id} has no shape properties`);
    const offV = { x: String(to.x), y: String(to.y) };
    const extV = { cx: String(to.cx), cy: String(to.cy) };
    if (xfrm !== null) {
      const off = childA(xfrm, "off");
      const ext = childA(xfrm, "ext");
      if (off === undefined || ext === undefined) {
        if (xfrm.selfClosing || el.local === "grpSp") fail(`shape ${id} has an incomplete transform`);
        const name = (l: string) => aName(xfrm, l) ?? fail("DrawingML namespace not bound");
        if (off === undefined) {
          out.push({ start: xfrm.openEnd, end: xfrm.openEnd, text: `<${name("off")} x="${offV.x}" y="${offV.y}"/>` });
        } else out.push(...setAttrSplices(xml, off, offV));
        if (ext === undefined) {
          const at = off === undefined ? xfrm.openEnd : off.end;
          out.push({ start: at, end: at, text: `<${name("ext")} cx="${extV.cx}" cy="${extV.cy}"/>` });
        } else out.push(...setAttrSplices(xml, ext, extV));
        continue;
      }
      out.push(...setAttrSplices(xml, off, offV), ...setAttrSplices(xml, ext, extV));
      continue;
    }
    // No transform: the shape inherits its place from the layout. PowerPoint's
    // own move writes an a:xfrm as the first child of p:spPr, and so does this.
    if (el.local === "grpSp" || el.local === "graphicFrame") fail(`shape ${id} has no transform to edit`);
    const bound = aName(host, "xfrm");
    const decl = bound === null ? ` xmlns:a="${A_URI}"` : "";
    const q = (l: string) => (bound === null ? `a:${l}` : (aName(host, l) as string));
    const inner =
      `<${q("off")} x="${offV.x}" y="${offV.y}"/>` + `<${q("ext")} cx="${extV.cx}" cy="${extV.cy}"/>`;
    const block = `<${q("xfrm")}${decl}>${inner}</${q("xfrm")}>`;
    if (host.selfClosing) {
      const startTag = xml.slice(host.start, host.openEnd - 2).replace(/\s+$/, "");
      out.push({ start: host.start, end: host.end, text: `${startTag}>${block}</${host.qname}>` });
    } else out.push({ start: host.openEnd, end: host.openEnd, text: block });
  }
  return out;
}

export function setXfrm(xml: string, id: string, to: Xfrm): string {
  return applySplices(xml, setXfrmSplices(xml, scan(xml), id, to));
}

// --- delete ---------------------------------------------------------------------

/** Remove shape `id` (with its `mc:AlternateContent` wrapper when it has
 *  one), and the whitespace-only text just before it so no blank line is left. */
export function deleteShape(xml: string, id: string): string {
  const root = scan(xml);
  const shapes = findShapes(xml, root, id);
  if (shapes.length === 0) fail(`no shape ${id}`);
  const spans = new Map<number, Splice>();
  for (const el of shapes) {
    let target = el;
    for (let p = el.parent; p !== null; p = p.parent) {
      if (p.local === "AlternateContent" && p.ns === NS_MC) target = p;
    }
    let start = target.start;
    while (start > 0 && /[ \t\r\n]/.test(xml[start - 1]!)) start--;
    if (start > 0 && xml[start - 1] !== ">") start = target.start;
    spans.set(target.start, { start, end: target.end, text: "" });
  }
  return applySplices(xml, [...spans.values()]);
}

// --- slide order ------------------------------------------------------------------

/** The `r:id` of each slide in `presentation.xml`'s `p:sldIdLst`, in order. */
export function slideOrder(xml: string): string[] {
  const root = scan(xml);
  const lst = childP(root, "sldIdLst");
  if (lst === undefined) return [];
  return lst.children.filter((c) => isP(c, "sldId")).map((c) => relId(xml, c) ?? fail("a slide without r:id"));
}

function relId(xml: string, el: XmlEl): string | null {
  for (const a of el.attrs) {
    const colon = a.qname.indexOf(":");
    if (colon < 0 || a.qname.slice(colon + 1) !== "id") continue;
    const uri = el.scope.get(a.qname.slice(0, colon)) ?? "";
    if (/relationships$/.test(uri)) return decodeEntities(xml.slice(a.valueStart, a.valueEnd));
  }
  return null;
}

/** Put the slides in `order` (a permutation of `slideOrder(xml)`): each
 *  `p:sldId` element's text moves whole; the whitespace between them stays. */
export function reorderSlides(xml: string, order: readonly string[]): string {
  const root = scan(xml);
  const lst = childP(root, "sldIdLst") ?? fail("no slide list");
  const ids = lst.children.filter((c) => isP(c, "sldId"));
  if (ids.length !== lst.children.length) fail("the slide list holds something besides slides");
  const byRel = new Map<string, string>();
  for (const el of ids) byRel.set(relId(xml, el) ?? fail("a slide without r:id"), xml.slice(el.start, el.end));
  if (order.length !== ids.length || new Set(order).size !== order.length || order.some((r) => !byRel.has(r))) {
    fail("not a reordering of this deck's slides");
  }
  return applySplices(
    xml,
    ids.map((el, k) => ({ start: el.start, end: el.end, text: byRel.get(order[k]!)! })),
  );
}

// --- text -----------------------------------------------------------------------

/** A line break inside a paragraph (`a:br`), as the text model writes it. */
export const LINE_BREAK = "\u000b";

interface Seg {
  kind: "run" | "br";
  el: XmlEl;
  /** The run's `a:t` (runs and fields). */
  t: XmlEl | null;
  /** Decoded text ("\v" for a break). */
  text: string;
}

interface Para {
  el: XmlEl;
  segs: Seg[];
  text: string;
}

function textOf(xml: string, t: XmlEl | null): string {
  return t === null || t.selfClosing ? "" : decodeEntities(xml.slice(t.openEnd, t.closeStart));
}

function paragraphs(xml: string, txBody: XmlEl): Para[] {
  return txBody.children
    .filter((c) => isA(c, "p"))
    .map((p) => {
      const segs: Seg[] = [];
      for (const c of p.children) {
        if (isA(c, "r") || isA(c, "fld")) {
          const t = childA(c, "t") ?? null;
          segs.push({ kind: "run", el: c, t, text: textOf(xml, t) });
        } else if (isA(c, "br")) segs.push({ kind: "br", el: c, t: null, text: LINE_BREAK });
      }
      return { el: p, segs, text: segs.map((s) => s.text).join("") };
    });
}

function txBodyOf(el: XmlEl): XmlEl | null {
  return el.local === "sp" ? (childP(el, "txBody") ?? null) : null;
}

/** Shape `id`'s text, one string per paragraph (`LINE_BREAK` for `a:br`);
 *  null when it isn't a shape with a text body. */
export function readText(xml: string, id: string): string[] | null {
  const el = findShapes(xml, scan(xml), id)[0];
  const body = el === undefined ? null : txBodyOf(el);
  return body === null ? null : paragraphs(xml, body).map((p) => p.text);
}

/** Every text shape's paragraphs in one scan of the part (shape id → text). */
export function readTexts(xml: string): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const el of walk(scan(xml))) {
    const id = shapeId(xml, el);
    if (id === null || out.has(id)) continue;
    const body = txBodyOf(el);
    if (body !== null) out.set(id, paragraphs(xml, body).map((p) => p.text));
  }
  return out;
}

function commonPrefix(a: string, b: string): number {
  let k = 0;
  while (k < a.length && k < b.length && a[k] === b[k]) k++;
  // Never split a surrogate pair.
  if (k > 0 && k < a.length && /[\udc00-\udfff]/.test(a[k]!)) k--;
  return k;
}

function commonSuffix(a: string, b: string, max: number): number {
  let k = 0;
  while (k < max && a[a.length - 1 - k] === b[b.length - 1 - k]) k++;
  if (k > 0 && k < a.length && /[\ud800-\udbff]/.test(a[a.length - 1 - k]!)) k--;
  return k;
}

/** The raw text of an element's child, or "" when it has none. */
function rawChild(xml: string, el: XmlEl | undefined): string {
  return el === undefined ? "" : xml.slice(el.start, el.end);
}

/** `a:endParaRPr` → `a:rPr`, keeping its attributes and children verbatim. */
function asRunProps(xml: string, end: XmlEl, rPrName: string): string {
  const raw = xml.slice(end.start, end.end);
  if (end.selfClosing) return `<${rPrName}${raw.slice(1 + end.qname.length)}`;
  const inner = raw.slice(end.openEnd - end.start, end.closeStart - end.start);
  const attrsPart = raw.slice(1 + end.qname.length, end.openEnd - end.start);
  return `<${rPrName}${attrsPart}${inner}</${rPrName}>`;
}

/** Runs (and breaks) for `text`, formatted with `rPr` (raw, may be ""). */
function runsFor(text: string, rPr: string, q: (l: string) => string): string {
  const parts = text.split(LINE_BREAK);
  return parts
    .map((part, k) => {
      const run = part === "" ? "" : `<${q("r")}>${rPr}<${q("t")}>${escapeText(part)}</${q("t")}></${q("r")}>`;
      return (k > 0 ? `<${q("br")}>${rPr}</${q("br")}>` : "") + run;
    })
    .join("");
}

/** The run properties a new run in `para` should carry: its last run's, else
 *  its end-of-paragraph properties, else none. */
function runPropsFor(xml: string, para: Para, q: (l: string) => string): string {
  for (let k = para.segs.length - 1; k >= 0; k--) {
    const s = para.segs[k]!;
    if (s.kind === "run" && isA(s.el, "r")) return rawChild(xml, childA(s.el, "rPr"));
  }
  const end = childA(para.el, "endParaRPr");
  return end === undefined ? "" : asRunProps(xml, end, q("rPr"));
}

/** Splices turning paragraph `para`'s text into `next`, touching only the
 *  runs the change falls in. */
function paragraphSplices(xml: string, para: Para, next: string, q: (l: string) => string): Splice[] {
  const old = para.text;
  if (old === next) return [];
  const pre = commonPrefix(old, next);
  const suf = commonSuffix(old, next, Math.min(old.length, next.length) - pre);
  const from = pre;
  const to = old.length - suf;
  const insert = next.slice(pre, next.length - suf);

  // An empty paragraph (or one whose runs are all breaks/fields with no text
  // to anchor on) gets fresh runs before its end properties.
  if (para.segs.length === 0) {
    const end = childA(para.el, "endParaRPr");
    const at = end?.start ?? (para.el.selfClosing ? -1 : para.el.closeStart);
    const text = runsFor(insert, runPropsFor(xml, para, q), q);
    if (at < 0) {
      const startTag = xml.slice(para.el.start, para.el.openEnd - 2).replace(/\s+$/, "");
      return [{ start: para.el.start, end: para.el.end, text: `${startTag}>${text}</${para.el.qname}>` }];
    }
    return [{ start: at, end: at, text }];
  }

  const out: Splice[] = [];
  let pos = 0;
  let placed = false;
  /** Where a run goes when no run can take the insertion. */
  let fallbackAt: number | null = null;
  for (let k = 0; k < para.segs.length; k++) {
    const s = para.segs[k]!;
    const a = pos;
    const b = pos + s.text.length;
    pos = b;
    if (fallbackAt === null && a >= from) fallbackAt = s.el.start;
    const lo = Math.max(a, from);
    const hi = Math.min(b, to);
    const touches = lo < hi;
    // Where an insertion goes: the run the change starts in, or — for a pure
    // insertion at a boundary — the run before it (typing continues that
    // run's formatting), the first run when the change is at the very start.
    const host =
      !placed &&
      s.kind === "run" &&
      s.t !== null &&
      !s.t.selfClosing &&
      ((from >= a && from < b) || (from === b && (to === from || from === old.length)) || (from === 0 && a === 0));
    if (host) {
      placed = true;
      const cutTo = Math.min(to, b);
      const { map } = decodeWithMap(xml.slice(s.t!.openEnd, s.t!.closeStart));
      const base = s.t!.openEnd;
      const rawFrom = base + map[from - a]!;
      const rawTo = base + map[cutTo - a]!;
      const brk = insert.indexOf(LINE_BREAK);
      if (brk < 0) {
        const remaining = s.text.length - (cutTo - from) + insert.length;
        if (remaining === 0 && para.segs.length > 1) out.push({ start: s.el.start, end: s.el.end, text: "" });
        else out.push({ start: rawFrom, end: rawTo, text: escapeText(insert) });
      } else {
        // A break in the inserted text splits this run around new a:br elements.
        const rPr = isA(s.el, "r") ? rawChild(xml, childA(s.el, "rPr")) : "";
        const head = s.text.slice(0, from - a) + insert;
        const tail = s.text.slice(cutTo - a);
        out.push({ start: s.el.start, end: s.el.end, text: runsFor(head + tail, rPr, q) });
      }
      continue;
    }
    if (!touches) continue;
    if (lo === a && hi === b) {
      out.push({ start: s.el.start, end: s.el.end, text: "" });
      continue;
    }
    if (s.kind === "run" && s.t !== null) {
      const { map } = decodeWithMap(xml.slice(s.t.openEnd, s.t.closeStart));
      out.push({ start: s.t.openEnd + map[lo - a]!, end: s.t.openEnd + map[hi - a]!, text: "" });
    }
  }
  if (!placed && insert !== "") {
    // Only breaks or text-less runs around the insertion point: add a run.
    const at = fallbackAt ?? childA(para.el, "endParaRPr")?.start ?? para.el.closeStart;
    out.push({ start: at, end: at, text: runsFor(insert, runPropsFor(xml, para, q), q) });
  }
  return out;
}

function newParagraph(xml: string, like: Para, text: string, q: (l: string) => string): string {
  const pPr = rawChild(xml, childA(like.el, "pPr"));
  const end = childA(like.el, "endParaRPr");
  return `<${like.el.qname}>${pPr}${runsFor(text, runPropsFor(xml, like, q), q)}${rawChild(xml, end)}</${like.el.qname}>`;
}

/**
 * Set shape `id`'s text to `next` (one string per paragraph, `LINE_BREAK`
 * for a line break). Unchanged paragraphs are untouched; a changed one
 * changes only inside the runs the edit falls in; a new paragraph copies its
 * neighbour's paragraph and run properties; a removed one is cut whole.
 */
export function setText(xml: string, id: string, next: readonly string[]): string {
  const root = scan(xml);
  const shapes = findShapes(xml, root, id);
  if (shapes.length === 0) fail(`no shape ${id}`);
  if (next.length === 0) fail("a text body needs at least one paragraph");
  const out: Splice[] = [];
  for (const el of shapes) {
    const body = txBodyOf(el) ?? fail(`shape ${id} has no text`);
    const paras = paragraphs(xml, body);
    if (paras.length === 0) fail(`shape ${id} has no paragraphs`);
    const q = (l: string) => aName(body, l) ?? fail("DrawingML namespace not bound");
    const old = paras.map((p) => p.text);
    let head = 0;
    while (head < old.length && head < next.length && old[head] === next[head]) head++;
    let tail = 0;
    while (
      tail < old.length - head &&
      tail < next.length - head &&
      old[old.length - 1 - tail] === next[next.length - 1 - tail]
    ) {
      tail++;
    }
    const oldMid = paras.slice(head, paras.length - tail);
    const newMid = next.slice(head, next.length - tail);
    const pairs = Math.min(oldMid.length, newMid.length);
    for (let k = 0; k < pairs; k++) out.push(...paragraphSplices(xml, oldMid[k]!, newMid[k]!, q));
    if (newMid.length > pairs) {
      const like = oldMid[pairs - 1] ?? paras[head - 1] ?? paras[head]!;
      const at = oldMid[pairs - 1]?.el.end ?? (head > 0 ? paras[head - 1]!.el.end : paras[0]!.el.start);
      const added = newMid.slice(pairs).map((t) => newParagraph(xml, like, t, q));
      out.push({ start: at, end: at, text: added.join("") });
    }
    for (const p of oldMid.slice(pairs)) out.push({ start: p.el.start, end: p.el.end, text: "" });
  }
  return applySplices(xml, out);
}
