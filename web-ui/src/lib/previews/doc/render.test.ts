import { describe, expect, it } from "vitest";
import { docParser } from "./parser";
import {
  bodyText,
  documentContext,
  footnotesOf,
  frontmatterOf,
  githubSlug,
  htmlOfNode,
  htmlRuns,
  outlineOf,
  docText,
} from "./model";
import {
  anchorSourceLine,
  escapeHref,
  HtmlTarget,
  renderFootnotes,
  renderHtml,
  renderRun,
  topLevel,
  urlAllowed,
  wikilinkHref,
} from "./render";

/** Every node of a parse as `Name:source`, for shape pins. */
function nodes(src: string, names: string[]): string[] {
  const out: string[] = [];
  docParser.parse(src).iterate({
    enter(n) {
      if (names.includes(n.name)) out.push(`${n.name}:${src.slice(n.from, n.to)}`);
    },
  });
  return out;
}

/** A render through a context built incrementally from `prev`. */
function renderFrom(prev: string, next: string): string {
  const a = bodyText(prev).text;
  const b = bodyText(next).text;
  const cx = documentContext(b, { text: a, tree: documentContext(a).tree });
  const t = new HtmlTarget();
  const root = t.el("div");
  const env = { t, cx };
  for (const run of htmlRuns(topLevel(cx), (n) => htmlOfNode(n, cx))) renderRun(root, run, env);
  renderFootnotes(root, footnotesOf(cx), env);
  return t.serializeChildren(root);
}

describe("the shared parser's extensions", () => {
  it("reads footnote references and definitions, a definition ending the paragraph before it", () => {
    expect(nodes("a[^1] and [^x.y]\n", ["FootnoteReference"])).toEqual([
      "FootnoteReference:[^1]",
      "FootnoteReference:[^x.y]",
    ]);
    expect(nodes("para\n[^1]: def\n    more\n", ["Paragraph", "FootnoteDefinition"])).toEqual([
      "Paragraph:para",
      "FootnoteDefinition:[^1]: def\n    more",
      "Paragraph:def\n    more",
    ]);
    // A label with a space is no footnote: an ordinary (shortcut) link.
    expect(nodes("a[^b c]\n", ["FootnoteReference", "Link"])).toEqual(["Link:[^b c]"]);
  });

  it("reads wikilinks and embeds on one line", () => {
    expect(nodes("[[a]] [[b|c]] ![[d.png]] [[x\ny]]\n", ["Wikilink"])).toEqual([
      "Wikilink:[[a]]",
      "Wikilink:[[b|c]]",
      "Wikilink:![[d.png]]",
    ]);
  });

  it("strikes single tildes like comrak, never three", () => {
    expect(nodes("~a b~ ~~c~~ ~~~d~~~\n", ["Strikethrough", "Subscript"])).toEqual([
      "Strikethrough:~a b~",
      "Strikethrough:~~c~~",
    ]);
  });
});

describe("frontmatter", () => {
  it("follows the daemon's shape", () => {
    expect(frontmatterOf("---\ntitle: x\n---\nbody")).toEqual({ raw: "title: x", end: 16 });
    expect(frontmatterOf("---\ntitle: x\n---")?.raw).toBe("title: x");
    expect(frontmatterOf("---\n\nintro\n\n---\n")).toBeNull(); // no key line
    expect(frontmatterOf("---\ntitle: x\n--- \n")).toBeNull(); // closer not exactly ---
    expect(frontmatterOf("---\ntitle: x\n----\n")).toBeNull();
    expect(frontmatterOf(`---\ntitle: x\n${"k: v\n".repeat(198)}---\n`)).toBeNull(); // past 200 lines
  });

  it("is blanked, not cut, so every offset and line still names the source", () => {
    const src = "---\r\ntitle: x\r\n---\r\n# H\r\n";
    const { text, frontmatter } = bodyText(src);
    expect(frontmatter?.raw).toBe("title: x");
    expect(text.split("\n").length).toBe(src.split("\r\n").length);
    expect(text.indexOf("# H")).toBe(src.replace(/\r\n/g, "\n").indexOf("# H"));
    expect(renderHtml(src).html).toContain('data-sourcepos="4:1-4:3"');
  });
});

describe("ids and hrefs", () => {
  it("slugs like GitHub (comrak's anchorizer)", () => {
    expect(githubSlug("Hello, World!")).toBe("hello-world");
    expect(githubSlug("Ünï code, (yes)!")).toBe("ünï-code-yes");
    expect(githubSlug("A & B 🎉 x")).toBe("a--b--x");
  });

  it("escapes hrefs like comrak and keeps only the daemon's schemes", () => {
    expect(escapeHref("my file.md")).toBe("my%20file.md");
    expect(escapeHref("/é?a=1&b='x'")).toBe("/%C3%A9?a=1&b='x'");
    expect(escapeHref("50%25 and 5% off")).toBe("50%25%20and%205%25%20off");
    expect(urlAllowed("javascript:alert(1)")).toBe(false);
    expect(urlAllowed("data:image/png;base64,x")).toBe(false);
    expect(urlAllowed("https://x.org")).toBe(true);
    expect(urlAllowed("../rel/a.md")).toBe(true);
  });

  it("points a wikilink at its file and heading", () => {
    expect(wikilinkHref("Other Note", null)).toBe("Other%20Note.md");
    expect(wikilinkHref("notes/plan", "Next Steps")).toBe("notes/plan.md#next-steps");
    expect(wikilinkHref("plot.png", null)).toBe("plot.png");
    expect(wikilinkHref("v1.2", null)).toBe("v1.2.md");
    expect(wikilinkHref("", "Here")).toBe("#here");
  });

  it("maps an anchor back to its source line for the editor modes", () => {
    const src = "---\na: 1\n---\n# Top\n\ntext[^n]\n\n## Top\n\n[^n]: note\n";
    expect(anchorSourceLine(src, "top")).toBe(4);
    expect(anchorSourceLine(src, "top-1")).toBe(8);
    expect(anchorSourceLine(src, "fn-n")).toBe(10);
    expect(anchorSourceLine(src, "fnref-n")).toBe(6);
    expect(anchorSourceLine(src, "nowhere")).toBeNull();
  });

  it("gives the live editor's tree the reading view's heading ids", () => {
    const src = "# A\n\n## A\n\n### B [x]\n\n[x]: /u\n";
    const tree = docParser.parse(src);
    expect(outlineOf(tree, docText(src)).map((h) => `${h.level}:${h.text}:${h.id}`)).toEqual([
      "1:A:a",
      "2:A:a-1",
      "3:B x:b-x",
    ]);
  });
});

describe("raw HTML around markdown", () => {
  it("groups a run an HTML block opens until its tags close", () => {
    const src = "<details>\n<summary>S</summary>\n\n- a\n\n</details>\n\nafter\n";
    const cx = documentContext(src);
    const runs = htmlRuns(topLevel(cx), (n) => htmlOfNode(n, cx)).map((r) => r.map((n) => n.name).join("+"));
    expect(runs).toEqual(["HTMLBlock+BulletList+HTMLBlock", "Paragraph"]);
    // An unclosed wrapper takes the rest of its container, as a browser would.
    const open = documentContext("<div>\n\ntext\n\nmore\n");
    expect(htmlRuns(topLevel(open), (n) => htmlOfNode(n, open)).length).toBe(1);
  });
});

describe("incremental parsing renders what a fresh parse does", () => {
  const base = [
    "---",
    "title: T",
    "---",
    "# Title",
    "",
    "Para one with a ref[^a] and [link][r].",
    "",
    "- item",
    "  - nested",
    "",
    "> [!NOTE]",
    "> quoted",
    "",
    "```js",
    "let x = 1;",
    "```",
    "",
    "| a | b |",
    "|---|---|",
    "| 1 | 2 |",
    "",
    "$$",
    "x^2",
    "$$",
    "",
    "[r]: /u",
    "[^a]: The note.",
    "",
  ].join("\n");
  const edits: [string, (s: string) => string][] = [
    ["an edit inside a paragraph", (s) => s.replace("Para one", "Para ONE")],
    ["a new heading that renumbers a slug", (s) => s.replace("# Title\n", "# Title\n\n# Title\n")],
    ["a line added at the top", (s) => s.replace("# Title", "Intro\n\n# Title")],
    ["a fence opened above other blocks", (s) => s.replace("- item", "```\n- item")],
    ["a definition removed", (s) => s.replace("[r]: /u\n", "")],
    ["a footnote reference added first", (s) => s.replace("# Title\n", "# Title\n\nEarly[^a].\n")],
    ["the frontmatter closed differently", (s) => s.replace("---\n# Title", "--- \n# Title")],
    ["an HTML wrapper opened", (s) => s.replace("> [!NOTE]", "<details>\n\n> [!NOTE]")],
  ];
  for (const [name, edit] of edits) {
    it(name, () => {
      const next = edit(base);
      expect(renderFrom(base, next)).toBe(renderHtml(next).html);
      // …and back again.
      expect(renderFrom(next, base)).toBe(renderHtml(base).html);
    });
  }
});
