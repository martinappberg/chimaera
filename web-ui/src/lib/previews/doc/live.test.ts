import { describe, expect, it } from "vitest";
import { Text } from "@codemirror/state";
import { docParser } from "./parser";
import { frontmatterOf } from "./model";
import {
  alignPrefix,
  bodyTree,
  crossesFrontmatter,
  edgeChain,
  HeightCache,
  hashString,
  nodesIn,
  revealedOf,
  runEdges,
  segmentAt,
  segmentsOf,
  shapeKey,
  sourceOffset,
  topNodes,
  visibleSource,
} from "./live";

function parse(src: string) {
  const doc = Text.of(src.split("\n"));
  const tree = docParser.parse(src);
  const fm = frontmatterOf(src);
  return { doc, tree, fmEnd: fm?.end ?? 0 };
}

/** Segments as `kind:[first line text]…[last line text]` rows. */
function segs(src: string): string[] {
  const { doc, tree, fmEnd } = parse(src);
  return segmentsOf(tree, doc, fmEnd).map((s) => `${s.kind}:${JSON.stringify(doc.sliceString(s.from, s.to))}`);
}

describe("segmentsOf", () => {
  it("tiles the document on whole lines, blank lines after a block belonging to it", () => {
    const src = "# Title\n\nPara one\ncontinues.\n\n- a\n- b\n\n\n```js\nx\n```\n";
    expect(segs(src)).toEqual([
      'block:"# Title\\n"',
      'block:"Para one\\ncontinues.\\n"',
      'block:"- a\\n- b\\n\\n"',
      'block:"```js\\nx\\n```\\n"',
    ]);
    const { doc, tree } = parse(src);
    const list = segmentsOf(tree, doc, 0);
    for (let i = 1; i < list.length; i++) expect(list[i].from).toBe(list[i - 1].to + 1);
    expect(list[0].from).toBe(0);
    expect(list[list.length - 1].to).toBe(doc.length);
  });

  it("gives leading blank lines to the first block and frontmatter its own segment", () => {
    expect(segs("\n\nHello\n")).toEqual(['block:"\\n\\nHello\\n"']);
    expect(segs("---\ntitle: x\n---\n\nBody\n")).toEqual(['front:"---\\ntitle: x\\n---\\n"', 'block:"Body\\n"']);
  });

  it("ends frontmatter on its closing line when a fence or HTML block opened in the YAML runs on", () => {
    // The editor reads the YAML as markdown: the fence opened there runs to
    // the end, the `<div>` to the next blank line.
    const fence = "---\ntitle: x\nexample: |\n  ```\n---\n\n# Body\n\npara\n";
    const html = '---\ntitle: x\ndesc: >\n  <div class="x">\n---\n# Heading right after\nPara\n\nnext para\n';
    // Never under the properties panel: what the node swallowed stays source.
    expect(segs(fence)).toEqual(['front:"---\\ntitle: x\\nexample: |\\n  ```\\n---"', 'source:"\\n# Body\\n\\npara\\n"']);
    expect(segs(html)).toEqual([
      'front:"---\\ntitle: x\\ndesc: >\\n  <div class=\\"x\\">\\n---"',
      'source:"# Heading right after\\nPara\\n"',
      'block:"next para\\n"',
    ]);
    // The body parsed on its own, as reading parses it, segments as blocks.
    const body = (src: string): string[] => {
      const { doc, tree, fmEnd } = parse(src);
      const own = bodyTree(tree, doc, fmEnd);
      expect(own).not.toBeNull();
      return segmentsOf(own ?? tree, doc, fmEnd).map((s) => `${s.kind}:${JSON.stringify(doc.sliceString(s.from, s.to))}`);
    };
    expect(body(fence)).toEqual([
      'front:"---\\ntitle: x\\nexample: |\\n  ```\\n---\\n"',
      'block:"# Body\\n"',
      'block:"para\\n"',
    ]);
    expect(body(html)).toEqual([
      'front:"---\\ntitle: x\\ndesc: >\\n  <div class=\\"x\\">\\n---"',
      'block:"# Heading right after"',
      'block:"Para\\n"',
      'block:"next para\\n"',
    ]);
    // Frontmatter the editor's tree ends on (a closed fence, a setext
    // underline): its own tree serves.
    const closed = parse("---\ntitle: x\nexample: |\n  ```\n  code\n  ```\n---\n\n# Body\n");
    expect(crossesFrontmatter(closed.tree, closed.fmEnd)).toBe(false);
    expect(bodyTree(closed.tree, closed.doc, closed.fmEnd)).toBeNull();
  });

  it("keeps definitions and comments as source and groups an HTML-opened run", () => {
    expect(segs("See [a].\n\n[a]: https://x.org\n\n<!-- c -->\n")).toEqual([
      'block:"See [a].\\n"',
      'source:"[a]: https://x.org\\n"',
      'source:"<!-- c -->\\n"',
    ]);
    expect(segs("<details>\n\nInside **md**.\n\n</details>\n\nAfter\n")).toEqual([
      'block:"<details>\\n\\nInside **md**.\\n\\n</details>\\n"',
      'block:"After\\n"',
    ]);
  });

  it("leaves what a partial parse has not reached as raw source", () => {
    const src = "# A\n\npara\n\n## B\n\nmore text\n";
    const doc = Text.of(src.split("\n"));
    const partial = docParser.startParse(src);
    partial.stopAt(8);
    let tree = null;
    while (tree === null) tree = partial.advance();
    const list = segmentsOf(tree, doc, 0);
    expect(list[list.length - 1].kind).toBe("raw");
    expect(list[list.length - 1].to).toBe(doc.length);
  });

  it("finds a segment by position", () => {
    const { doc, tree } = parse("a\n\nb\n\nc\n");
    const list = segmentsOf(tree, doc, 0);
    expect(segmentAt(list, 0)).toBe(0);
    expect(segmentAt(list, 2)).toBe(0); // the blank line after `a`
    expect(segmentAt(list, 3)).toBe(1);
    expect(segmentAt(list, doc.length)).toBe(2);
  });

  it("finds a run's nodes again from its range", () => {
    const src = "<div>\n\nx\n\n</div>\n\ny\n";
    const { doc, tree } = parse(src);
    const [first, second] = segmentsOf(tree, doc, 0);
    expect(nodesIn(tree, first.blockFrom, first.blockTo).map((n) => n.name)).toEqual([
      "HTMLBlock",
      "Paragraph",
      "HTMLBlock",
    ]);
    expect(nodesIn(tree, second.blockFrom, second.blockTo).map((n) => n.name)).toEqual(["Paragraph"]);
  });
});

describe("revealedOf", () => {
  const { doc, tree } = parse("# T\n\none\n\ntwo\n\nthree\n\n[x]: y\n");
  const list = segmentsOf(tree, doc, 0);
  const at = (line: number) => doc.line(line).from;

  it("reveals the segment a cursor is in, and what never renders", () => {
    expect(revealedOf(list, [{ from: at(3), to: at(3) }])).toEqual([false, true, false, false, true]);
    // The blank line after a block is that block's.
    expect(revealedOf(list, [{ from: at(4), to: at(4) }])).toEqual([false, true, false, false, true]);
  });

  it("reveals every segment a selection spans, and each range of several", () => {
    expect(revealedOf(list, [{ from: at(3), to: at(7) }])).toEqual([false, true, true, true, true]);
    expect(
      revealedOf(list, [
        { from: at(1), to: at(1) },
        { from: at(7), to: at(7) + 2 },
      ]),
    ).toEqual([true, false, false, true, true]);
    expect(revealedOf(list, [])).toEqual([false, false, false, false, true]);
  });
});

describe("edge chains", () => {
  const chains = (src: string) => {
    const { doc, tree } = parse(src);
    return topNodes(tree).map((n) => `${shapeKey(edgeChain(n, doc, "head"))} | ${shapeKey(edgeChain(n, doc, "tail"))}`);
  };

  it("names the element each block renders as", () => {
    expect(chains("# H\n\np\n\n---\n\n> q\n\n> [!NOTE]\n> n\n\n```mermaid\na\n```\n\n$$\nx\n$$\n\n| a |\n|---|\n")).toEqual([
      "h1 | h1",
      "p | p",
      "hr | hr",
      "blockquote | blockquote",
      "div.markdown-alert.markdown-alert-note | div.markdown-alert.markdown-alert-note",
      "div.md-mermaid | div.md-mermaid",
      "p | p",
      "table | table",
    ]);
  });

  it("follows a loose list into its items' paragraphs, stopping at a tight item", () => {
    expect(chains("- a\n- b\n")).toEqual(["ul>li | ul>li"]);
    expect(chains("1. a\n\n2. b\n")).toEqual(["ol>li>p | ol>li>p"]);
    expect(chains("- a\n  - nested\n")).toEqual(["ul>li | ul>li>ul>li"]);
  });

  it("reads raw HTML's outer tags, and a run's wrapper as both edges", () => {
    const { doc, tree } = parse("<details>\n\nx\n\n</details>\n");
    const run = topNodes(tree);
    expect(shapeKey(runEdges(run.slice(0, 1), doc).head)).toBe("details");
    const edges = runEdges(run, doc);
    expect(shapeKey(edges.tail)).toBe("details");
  });
});

describe("HeightCache", () => {
  it("returns what was measured and evicts the least recently used", () => {
    const c = new HeightCache(2);
    c.set("a", 10);
    c.set("b", 20);
    expect(c.get("a")).toBe(10); // a is now the most recent
    c.set("c", 30);
    expect(c.get("b")).toBeUndefined();
    expect(c.get("a")).toBe(10);
    expect(c.get("c")).toBe(30);
    expect(c.size).toBe(2);
  });

  it("hashes keys stably and distinctly", () => {
    expect(hashString("paragraph one")).toBe(hashString("paragraph one"));
    expect(hashString("paragraph one")).not.toBe(hashString("paragraph two"));
  });
});

describe("rendered text back to the source", () => {
  const offset = (src: string, prefix: string, line = 1) => {
    const { doc, tree } = parse(src);
    const l = doc.line(line);
    return sourceOffset(tree, doc, l.from, l.to, prefix);
  };

  it("drops syntax from the visible source", () => {
    const src = "## A **b** [c](d.md) `e` \\*f\n";
    const { doc, tree } = parse(src);
    expect(visibleSource(tree, doc, 0, doc.line(1).to).text).toBe("A b c e *f");
  });

  it("aligns a rendered prefix past marks, link destinations and soft breaks", () => {
    const src = "Some **bold** text and a [link](https://example.com/x) here\nsecond line.";
    expect(offset(src, "Some bo")).toBe(src.indexOf("ld**"));
    expect(offset(src, "Some bold text and a li")).toBe(src.indexOf("nk]"));
    expect(offset(src, "Some bold text and a link h")).toBe(src.indexOf("ere"));
    const { doc, tree } = parse(src);
    expect(sourceOffset(tree, doc, 0, doc.length, "Some bold text and a link here sec")).toBe(src.indexOf("ond"));
  });

  it("lands a click in a heading or list item after its marker", () => {
    expect(offset("# Title here\n", "Title h")).toBe("# Title h".length);
    expect(offset("- [ ] a task\n", "a t")).toBe("- [ ] a t".length);
    expect(offset("# Title\n", "")).toBe(2);
  });

  it("skips rendered characters with no source near by", () => {
    expect(alignPrefix("x + 1", "𝑥+1")).toBe(5);
    expect(alignPrefix("abc", "zzz")).toBe(0);
  });
});
