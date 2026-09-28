/**
 * A small, strict-enough XML reader for draw.io files: elements, attributes
 * (entity-decoded), text and CDATA; comments, processing instructions and
 * DOCTYPEs are skipped (no entities are ever defined or expanded, so nothing
 * external is ever referenced). Pure, so the parsing is testable without a
 * DOM, and bounded: past `MAX_NODES` elements the rest is ignored.
 */

export interface XmlNode {
  name: string;
  attrs: Record<string, string>;
  children: XmlNode[];
  text: string;
}

const MAX_NODES = 200_000;

const NAMED: Record<string, string> = { lt: "<", gt: ">", amp: "&", quot: '"', apos: "'" };

export function decodeEntities(s: string): string {
  if (!s.includes("&")) return s;
  return s.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (m, body: string) => {
    if (body[0] === "#") {
      const code = body[1] === "x" || body[1] === "X" ? parseInt(body.slice(2), 16) : parseInt(body.slice(1), 10);
      return Number.isFinite(code) && code > 0 && code <= 0x10ffff ? String.fromCodePoint(code) : m;
    }
    return NAMED[body.toLowerCase()] ?? m;
  });
}

const ATTR = /([^\s=/>]+)\s*(?:=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g;

export function parseXml(src: string): XmlNode {
  const root: XmlNode = { name: "#root", attrs: {}, children: [], text: "" };
  const stack: XmlNode[] = [root];
  let i = 0;
  let count = 0;
  const len = src.length;
  while (i < len) {
    const lt = src.indexOf("<", i);
    const top = stack[stack.length - 1];
    if (lt === -1) {
      top.text += decodeEntities(src.slice(i));
      break;
    }
    if (lt > i) top.text += decodeEntities(src.slice(i, lt));
    if (src.startsWith("<!--", lt)) {
      const end = src.indexOf("-->", lt + 4);
      i = end === -1 ? len : end + 3;
      continue;
    }
    if (src.startsWith("<![CDATA[", lt)) {
      const end = src.indexOf("]]>", lt + 9);
      top.text += src.slice(lt + 9, end === -1 ? len : end);
      i = end === -1 ? len : end + 3;
      continue;
    }
    if (src.startsWith("<?", lt) || src.startsWith("<!", lt)) {
      const end = src.indexOf(">", lt + 2);
      i = end === -1 ? len : end + 1;
      continue;
    }
    const gt = findTagEnd(src, lt + 1);
    if (gt === -1) throw new Error("the XML ends inside a tag");
    const inner = src.slice(lt + 1, gt);
    i = gt + 1;
    if (inner.startsWith("/")) {
      const name = inner.slice(1).trim();
      // Close up to the matching element (tolerates a stray close tag).
      for (let k = stack.length - 1; k > 0; k--) {
        if (stack[k].name === name) {
          stack.length = k;
          break;
        }
      }
      continue;
    }
    const selfClosing = inner.endsWith("/");
    const body = selfClosing ? inner.slice(0, -1) : inner;
    const m = /^\s*([^\s/>]+)/.exec(body);
    if (m === null) continue;
    if (++count > MAX_NODES) break;
    const node: XmlNode = { name: m[1], attrs: {}, children: [], text: "" };
    ATTR.lastIndex = m[0].length;
    for (let a = ATTR.exec(body); a !== null; a = ATTR.exec(body)) {
      node.attrs[a[1]] = decodeEntities(a[2] ?? a[3] ?? a[4] ?? "");
    }
    top.children.push(node);
    if (!selfClosing) stack.push(node);
  }
  return root;
}

/** The `>` closing a tag that starts at `from`, skipping quoted `>`s. */
function findTagEnd(src: string, from: number): number {
  let quote = "";
  for (let i = from; i < src.length; i++) {
    const c = src[i];
    if (quote !== "") {
      if (c === quote) quote = "";
    } else if (c === '"' || c === "'") quote = c;
    else if (c === ">") return i;
  }
  return -1;
}

/** Depth-first search for elements named `name`. */
export function findAll(node: XmlNode, name: string, out: XmlNode[] = []): XmlNode[] {
  for (const c of node.children) {
    if (c.name === name) out.push(c);
    findAll(c, name, out);
  }
  return out;
}

export function findFirst(node: XmlNode, name: string): XmlNode | null {
  for (const c of node.children) {
    if (c.name === name) return c;
    const hit = findFirst(c, name);
    if (hit !== null) return hit;
  }
  return null;
}
