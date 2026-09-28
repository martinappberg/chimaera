import { describe, expect, it } from "vitest";
import {
  anchorIds,
  classifyHref,
  createModeMemory,
  decodeAnchor,
  frontmatterLineSpan,
  parseFrontmatter,
  parseLineFragment,
  parseSourcepos,
  placeOffset,
  revealIndex,
  spanLines,
  stripFences,
  taskBoxAt,
} from "./mdDoc";
import { EditorState } from "@codemirror/state";

describe("placeOffset", () => {
  it("keeps a block in view at its offset", () => {
    expect(placeOffset(12, 90, 22)).toBe(12);
    expect(placeOffset(0, 90, 22)).toBe(0);
  });
  it("keeps the share of a block scrolled partly by", () => {
    // A 90px figure 40px past the edge is one 22px source line.
    expect(placeOffset(-40, 90, 22)).toBeCloseTo(-9.78, 2);
    expect(placeOffset(-9.78, 22, 90)).toBeCloseTo(-40, 1);
    expect(placeOffset(-30, 60, 60)).toBe(-30);
  });
  it("keeps the gap below a block wholly past the edge", () => {
    expect(placeOffset(-40, 27, 27)).toBe(-40);
    expect(placeOffset(-40, 27, 58)).toBe(-71);
  });
  it("falls back to the offset without heights", () => {
    expect(placeOffset(-40, 0, 22)).toBe(-40);
    expect(placeOffset(-40, 90, 0)).toBe(-40);
  });
});

describe("parseFrontmatter", () => {
  it("reads scalars, flow lists and booleans", () => {
    const fm = parseFrontmatter(
      'title: "Results: round 2"\ntags: [rna-seq, "a, b", \'c\']\ndraft: false\ndate: 2026-09-25\nn: 3 # replicates\nurl: https://example.org/x#frag',
    );
    expect(fm).toEqual([
      { key: "title", value: { kind: "text", text: "Results: round 2" } },
      { key: "tags", value: { kind: "list", items: ["rna-seq", "a, b", "c"] } },
      { key: "draft", value: { kind: "bool", value: false } },
      { key: "date", value: { kind: "text", text: "2026-09-25" } },
      { key: "n", value: { kind: "text", text: "3" } },
      { key: "url", value: { kind: "text", text: "https://example.org/x#frag" } },
    ]);
  });

  it("reads block lists, indented or at the key's column", () => {
    expect(parseFrontmatter("tags:\n  - one\n  - two\naliases:\n- x\n- 'y'")).toEqual([
      { key: "tags", value: { kind: "list", items: ["one", "two"] } },
      { key: "aliases", value: { kind: "list", items: ["x", "y"] } },
    ]);
  });

  it("keeps what it can't flatten as raw text", () => {
    expect(parseFrontmatter("author:\n  name: Ada\n  orcid: 0000\ntitle: T")).toEqual([
      { key: "author", value: { kind: "raw", text: "name: Ada\norcid: 0000" } },
      { key: "title", value: { kind: "text", text: "T" } },
    ]);
    expect(parseFrontmatter("people:\n  - name: Ada\n  - name: Bo")?.[0].value.kind).toBe("raw");
    expect(parseFrontmatter("m: {a: 1}")?.[0].value).toEqual({ kind: "raw", text: "{a: 1}" });
    expect(parseFrontmatter("l: [a, [b]]")?.[0].value).toEqual({ kind: "raw", text: "[a, [b]]" });
  });

  it("reads block scalars and folded plain scalars", () => {
    expect(parseFrontmatter("abstract: |\n  line one\n  line two\nnote: >\n  folded\n  text")).toEqual([
      { key: "abstract", value: { kind: "text", text: "line one\nline two" } },
      { key: "note", value: { kind: "text", text: "folded text" } },
    ]);
    expect(parseFrontmatter("title: a long\n  title")?.[0].value).toEqual({
      kind: "text",
      text: "a long title",
    });
  });

  it("treats empty and null values as empty text, quoted true as text", () => {
    expect(parseFrontmatter("a:\nb: ~\nc: 'true'")).toEqual([
      { key: "a", value: { kind: "text", text: "" } },
      { key: "b", value: { kind: "text", text: "" } },
      { key: "c", value: { kind: "text", text: "true" } },
    ]);
  });

  it("tolerates fences, comments and blank lines", () => {
    expect(parseFrontmatter("---\n# comment\n\ntitle: x\n---\n")).toEqual([
      { key: "title", value: { kind: "text", text: "x" } },
    ]);
  });

  it("gives up (null) on a shape it does not know", () => {
    expect(parseFrontmatter("- a\n- b")).toBeNull();
    expect(parseFrontmatter("  indented: 1")).toBeNull();
    expect(parseFrontmatter("key:value")).toBeNull();
  });
});

describe("frontmatter spans", () => {
  it("counts the block's lines, fences included", () => {
    expect(frontmatterLineSpan("---\ntitle: x\ntags: [a]\n---\n")).toBe(4);
    expect(frontmatterLineSpan("title: x\ntags: [a]\n")).toBe(4);
    expect(frontmatterLineSpan("")).toBe(2);
  });

  it("strips the fences it finds", () => {
    expect(stripFences("---\na: 1\n---\n")).toBe("a: 1");
    expect(stripFences("a: 1")).toBe("a: 1");
  });
});

describe("parseSourcepos", () => {
  it("reads comrak's line range", () => {
    expect(parseSourcepos("3:1-5:10")).toEqual({ start: 3, end: 5 });
    expect(parseSourcepos(" 7:4-7:0 ")).toEqual({ start: 7, end: 7 });
    expect(parseSourcepos("9:1-8:2")).toEqual({ start: 9, end: 9 });
  });

  it("rejects anything else", () => {
    expect(parseSourcepos(null)).toBeNull();
    expect(parseSourcepos("")).toBeNull();
    expect(parseSourcepos("0:0-0:0")).toBeNull();
    expect(parseSourcepos("3-5")).toBeNull();
  });

  it("spans a selection's two ends", () => {
    expect(spanLines({ start: 8, end: 9 }, { start: 3, end: 4 })).toEqual({ start: 3, end: 9 });
    expect(spanLines(null, { start: 3, end: 4 })).toEqual({ start: 3, end: 4 });
    expect(spanLines(null, null)).toBeNull();
  });
});

describe("revealIndex", () => {
  // ul 10-20 > li 10-12 > p 10-12, li 14-20 > p 14-15, p 17-20; then h2 22
  const ranges = [
    { start: 1, end: 1 },
    { start: 10, end: 20 },
    { start: 10, end: 12 },
    { start: 10, end: 12 },
    { start: 14, end: 20 },
    { start: 14, end: 15 },
    { start: 17, end: 20 },
    null,
    { start: 22, end: 22 },
  ];

  it("picks the tightest block holding the line, deepest on a tie", () => {
    expect(revealIndex(ranges, 11)).toBe(3);
    expect(revealIndex(ranges, 15)).toBe(5);
    expect(revealIndex(ranges, 18)).toBe(6);
    expect(revealIndex(ranges, 1)).toBe(0);
  });

  it("falls to the enclosing block, then the next, then the last", () => {
    expect(revealIndex(ranges, 16)).toBe(4);
    expect(revealIndex(ranges, 5)).toBe(1);
    expect(revealIndex(ranges, 99)).toBe(8);
    expect(revealIndex([null], 3)).toBe(-1);
    expect(revealIndex([], 3)).toBe(-1);
  });
});

describe("anchors and fragments", () => {
  it("decodes an anchor", () => {
    expect(decodeAnchor("#My%20Heading")).toBe("My Heading");
    expect(decodeAnchor("fn-1")).toBe("fn-1");
    expect(decodeAnchor("#%E0%A4%A")).toBe("%E0%A4%A");
  });

  it("names the prefixed id first", () => {
    expect(anchorIds("intro")).toEqual(["user-content-intro", "intro"]);
    expect(anchorIds("user-content-fn-1")).toEqual(["user-content-fn-1"]);
    expect(anchorIds("Intro")).toEqual(["user-content-Intro", "Intro", "user-content-intro"]);
    expect(anchorIds("")).toEqual([]);
  });

  it("parses GitHub line fragments", () => {
    expect(parseLineFragment("L12")).toEqual({ line: 12 });
    expect(parseLineFragment("#L12-L20")).toEqual({ line: 12, endLine: 20 });
    expect(parseLineFragment("L12-20")).toEqual({ line: 12, endLine: 20 });
    expect(parseLineFragment("L3C5-L4C1")).toEqual({ line: 3, col: 5, endLine: 4 });
    expect(parseLineFragment("L20-L12")).toEqual({ line: 20 });
    expect(parseLineFragment("L0")).toBeNull();
    expect(parseLineFragment("l12")).toBeNull();
    expect(parseLineFragment("results")).toBeNull();
  });
});

describe("classifyHref", () => {
  it("sorts same-document targets", () => {
    expect(classifyHref("#fn-1")).toEqual({ kind: "anchor", anchor: "fn-1" });
    expect(classifyHref("#My%20Note")).toEqual({ kind: "anchor", anchor: "My Note" });
    expect(classifyHref("#L5-L7")).toEqual({ kind: "lines", reveal: { line: 5, endLine: 7 } });
    expect(classifyHref("#")).toEqual({ kind: "none" });
    expect(classifyHref("?plain=1#usage")).toEqual({ kind: "anchor", anchor: "usage" });
  });

  it("sorts schemes", () => {
    expect(classifyHref("https://x.org/a")).toEqual({ kind: "web", url: "https://x.org/a" });
    expect(classifyHref("www.x.org")).toEqual({ kind: "web", url: "www.x.org" });
    expect(classifyHref("mailto:a@b.c")).toEqual({ kind: "native" });
    expect(classifyHref("javascript:alert(1)")).toEqual({ kind: "none" });
    expect(classifyHref("vscode://file/x")).toEqual({ kind: "none" });
    expect(classifyHref("")).toEqual({ kind: "none" });
  });

  it("decodes file paths and keeps the fragment raw", () => {
    expect(classifyHref("other.md")).toEqual({ kind: "path", path: "other.md", fragment: null });
    expect(classifyHref(" ../a%20b.md#Sec%20One ")).toEqual({
      kind: "path",
      path: "../a b.md",
      fragment: "Sec%20One",
    });
    expect(classifyHref("x.py?plain=1#L3")).toEqual({ kind: "path", path: "x.py", fragment: "L3" });
    expect(classifyHref("notes%23v2.md")).toEqual({ kind: "path", path: "notes#v2.md", fragment: null });
    expect(classifyHref("a.md#")).toEqual({ kind: "path", path: "a.md", fragment: null });
    expect(classifyHref("/docs/x.md")).toEqual({ kind: "path", path: "/docs/x.md", fragment: null });
  });

  it("reads local file URLs only", () => {
    expect(classifyHref("file:///tmp/a%20b.md#L2")).toEqual({
      kind: "path",
      path: "/tmp/a b.md",
      fragment: "L2",
    });
    expect(classifyHref("file://host/share/x.md")).toEqual({ kind: "none" });
  });
});

describe("createModeMemory", () => {
  function fakeStorage() {
    const map = new Map<string, string>();
    return {
      map,
      getItem: (k: string) => map.get(k) ?? null,
      setItem: (k: string, v: string) => void map.set(k, v),
    };
  }

  it("remembers per path and forgets the least recently used", () => {
    const store = fakeStorage();
    const mem = createModeMemory(() => store, "k", 3);
    mem.set("/a", "live");
    mem.set("/b", "source");
    mem.set("/c", "reading");
    expect(mem.get("/a")).toBe("live"); // bumps /a to newest
    mem.set("/d", "live"); // evicts /b, now the oldest
    expect(mem.get("/b")).toBeNull();
    expect(mem.get("/a")).toBe("live");
    expect(mem.get("/c")).toBe("reading");
    expect(mem.get("/d")).toBe("live");
    mem.set("/a", "source");
    expect(mem.get("/a")).toBe("source");
    expect(JSON.parse(store.map.get("k") ?? "[]")).toHaveLength(3);
  });

  it("survives corrupt data and hostile entries", () => {
    const store = fakeStorage();
    const mem = createModeMemory(() => store, "k");
    store.map.set("k", "{not json");
    expect(mem.get("/a")).toBeNull();
    store.map.set("k", JSON.stringify([["/a", "edit"], ["/b", "live"], "x", [1, "live"]]));
    expect(mem.get("/a")).toBeNull();
    expect(mem.get("/b")).toBe("live");
  });

  it("never throws when storage does", () => {
    const throwing = createModeMemory(() => {
      throw new Error("SecurityError");
    });
    expect(throwing.get("/a")).toBeNull();
    expect(() => throwing.set("/a", "live")).not.toThrow();
    const full = createModeMemory(() => ({
      getItem: () => null,
      setItem: () => {
        throw new Error("QuotaExceededError");
      },
    }));
    expect(() => full.set("/a", "live")).not.toThrow();
    expect(createModeMemory(() => null).get("/a")).toBeNull();
  });
});

describe("taskBoxAt", () => {
  it("finds a list item's box, in quotes and ordered lists too", () => {
    const text = "# T\n\n- [ ] one\n- [x] two\n> 1. [X] quoted\n* not [ ] a box\n";
    expect(taskBoxAt(text, 3)).toEqual({ text: "- [ ] one", at: 2, done: false });
    expect(taskBoxAt(text, 4)).toEqual({ text: "- [x] two", at: 2, done: true });
    expect(taskBoxAt(text, 5)).toEqual({ text: "> 1. [X] quoted", at: 5, done: true });
    expect(taskBoxAt(text, 6)).toBeNull();
    expect(taskBoxAt(text, 1)).toBeNull();
    expect(taskBoxAt(text, 99)).toBeNull();
  });

  it("reads a CRLF (or CR) file's line as the editor holds it", () => {
    for (const eol of ["\r\n", "\r"]) {
      const text = ["# Tasks", "", "- [ ] first", "- [x] second", ""].join(eol);
      // CodeMirror splits on every line break and keeps none of it.
      const doc = EditorState.create({ doc: text }).doc;
      for (const n of [3, 4]) {
        const box = taskBoxAt(text, n);
        expect(box?.text).toBe(doc.line(n).text);
        expect(doc.sliceString(doc.line(n).from + (box?.at ?? 0), doc.line(n).from + (box?.at ?? 0) + 3)).toMatch(/^\[[ x]\]$/);
      }
      expect(taskBoxAt(text, 4)?.done).toBe(true);
    }
  });
});
