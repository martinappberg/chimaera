import { describe, expect, it } from "vitest";
import fixture from "./parity.fixture.json";
import { renderHtml } from "./render";

/**
 * The reading view's parity corpus, client half: every case through the
 * renderer's string target, compared under the corpus normalization. The
 * daemon half (`fs.rs markdown_parity_tests`) runs the same cases through
 * comrak + ammonia with the same steps; keep the two normalizers in step.
 */

interface Case {
  note: string;
  md: string;
  html: string;
  diverges?: { side: "client" | "server"; reason: string };
}

/** The only attributes compared (what the view's CSS and logic key on). */
const KEEP_ATTRS = new Set(["align", "alt", "class", "data-task", "href", "id", "src", "start"]);

/** Whitespace next to these tags is layout, not content. */
const BLOCK_TAGS = new Set([
  "address", "article", "aside", "blockquote", "br", "dd", "details", "div", "dl", "dt",
  "figcaption", "figure", "footer", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hr", "li",
  "ol", "p", "pre", "section", "summary", "table", "tbody", "td", "tfoot", "th", "thead", "tr",
  "ul",
]);

/** Named references decoded (numeric ones always): the serializer's own
 *  plus the few the corpus writes. */
const ENTITIES: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
  nbsp: " ",
  copy: "©",
  hellip: "…",
  mdash: "—",
  ndash: "–",
};

function decodeEntities(s: string): string {
  return s.replace(/&(#[xX][0-9a-fA-F]+|#\d+|[A-Za-z0-9]+);/g, (m, name: string) => {
    if (name.startsWith("#")) {
      const code = /^#[xX]/.test(name) ? parseInt(name.slice(2), 16) : Number(name.slice(1));
      try {
        return String.fromCodePoint(code);
      } catch {
        return m;
      }
    }
    return ENTITIES[name] ?? m;
  });
}

type Token =
  | { kind: "text"; text: string }
  | { kind: "tag"; name: string; close: boolean; attrs: [string, string][] };

/** Tags and text; comments dropped; a `<` that opens no tag is text. */
function tokenize(html: string): Token[] {
  const tokens: Token[] = [];
  let textFrom = 0;
  let i = 0;
  const ws = (c: string | undefined): boolean => c === " " || c === "\t" || c === "\n" || c === "\r";
  while (i < html.length) {
    if (html[i] !== "<") {
      i++;
      continue;
    }
    if (html.startsWith("<!--", i)) {
      tokens.push({ kind: "text", text: html.slice(textFrom, i) });
      const end = html.indexOf("-->", i);
      i = end < 0 ? html.length : end + 3;
      textFrom = i;
      continue;
    }
    const close = html[i + 1] === "/";
    let j = i + 1 + (close ? 1 : 0);
    if (!/[A-Za-z]/.test(html[j] ?? "")) {
      i++;
      continue;
    }
    const nameFrom = j;
    while (j < html.length && /[A-Za-z0-9]/.test(html[j])) j++;
    const name = html.slice(nameFrom, j).toLowerCase();
    const attrs: [string, string][] = [];
    let closed = false;
    while (j < html.length) {
      const c = html[j];
      if (c === ">") {
        j++;
        closed = true;
        break;
      }
      if (ws(c) || c === "/") {
        j++;
        continue;
      }
      const from = j;
      while (j < html.length && !ws(html[j]) && !"=>/".includes(html[j])) j++;
      const attr = html.slice(from, j).toLowerCase();
      while (ws(html[j])) j++;
      let value = "";
      if (html[j] === "=") {
        j++;
        while (ws(html[j])) j++;
        const q = html[j];
        if (q === '"' || q === "'") {
          const end = html.indexOf(q, j + 1);
          const stop = end < 0 ? html.length : end;
          value = html.slice(j + 1, stop);
          j = Math.min(stop + 1, html.length);
        } else {
          const vFrom = j;
          while (j < html.length && !ws(html[j]) && html[j] !== ">") j++;
          value = html.slice(vFrom, j);
        }
      }
      attrs.push([attr, decodeEntities(value)]);
    }
    if (!closed) break;
    tokens.push({ kind: "text", text: html.slice(textFrom, i) });
    tokens.push({ kind: "tag", name, close, attrs });
    i = j;
    textFrom = j;
  }
  tokens.push({ kind: "text", text: html.slice(textFrom) });
  return tokens;
}

/** The corpus normalization: lowercase tags, only KEEP_ATTRS (sorted,
 *  `class` whitespace collapsed), entities decoded and re-escaped one way,
 *  ASCII whitespace collapsed, whitespace beside a block tag dropped,
 *  comments gone. `fs.rs` `markdown_parity_tests::normalize` mirrors it. */
export function normalizeHtml(html: string): string {
  const tokens = tokenize(html);
  const isBlock = (t: Token | undefined): boolean => t?.kind === "tag" && BLOCK_TAGS.has(t.name);
  let out = "";
  tokens.forEach((t, k) => {
    if (t.kind === "text") {
      let text = decodeEntities(t.text).replace(/[ \t\n\r\f]+/g, " ");
      if (k === 0 || isBlock(tokens[k - 1])) text = text.replace(/^ +/, "");
      if (k === tokens.length - 1 || isBlock(tokens[k + 1])) text = text.replace(/ +$/, "");
      out += text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
      return;
    }
    out += `<${t.close ? "/" : ""}${t.name}`;
    const kept = t.attrs.filter(([a]) => KEEP_ATTRS.has(a)).sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
    for (const [a, v] of kept) {
      const value = a === "class" ? v.split(/[ \t\n\r\f]+/).filter((s) => s !== "").join(" ") : v;
      out += ` ${a}="${value.replace(/&/g, "&amp;").replace(/"/g, "&quot;")}"`;
    }
    out += ">";
  });
  return out;
}

describe("the corpus normalization", () => {
  it("is the documented one (the Rust test pins the same pairs)", () => {
    expect(
      normalizeHtml(
        '<P data-sourcepos="1:1-1:3" CLASS=" a  b ">x\n  y</P>\n<!-- c --><br />\nz &amp; &lt;&#65;&copy;',
      ),
    ).toBe('<p class="a b">x y</p><br>z &amp; &lt;A©');
    expect(
      normalizeHtml('<a title="t" href="/x?a=1&amp;b=2" rel="noopener">l</a> <em>e</em>'),
    ).toBe('<a href="/x?a=1&amp;b=2">l</a> <em>e</em>');
  });
});

describe("markdown parity corpus (client renderer)", () => {
  const cases = fixture.cases as Case[];
  it("covers every construct", () => {
    expect(cases.length).toBeGreaterThanOrEqual(80);
    for (const c of cases) {
      if (c.diverges !== undefined) {
        expect(["client", "server"]).toContain(c.diverges.side);
        expect(c.diverges.reason).not.toBe("");
      }
    }
  });
  for (const c of cases) {
    const skip = c.diverges?.side === "client";
    (skip ? it.skip : it)(c.note, () => {
      expect(normalizeHtml(renderHtml(c.md).html)).toBe(normalizeHtml(c.html));
    });
  }
});
