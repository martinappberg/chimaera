import { describe, expect, it } from "vitest";
import { markdownLanguage } from "@codemirror/lang-markdown";
import type { MarkdownParser } from "@lezer/markdown";
import { countDollarPairs, isMath, mathDelimiters, mathExtension } from "./mdMath";
import fixture from "./mathBlocks.fixture.json";

// The APP's parser configuration (GFM + sub/superscript + emoji), not bare
// CommonMark: extension ORDER is part of what these pins protect.
const mathParser = (markdownLanguage.parser as MarkdownParser).configure(mathExtension);

/** Every math node as `containers/Name:source` — the enclosing block-context
 *  nodes first, none at top level; an unclosed block as `MathBlock(unclosed)`
 *  (the fixture's `editor` shape) — plus any Escape or Emphasis the parser
 *  produced: inside an equation there must be none. */
function math(src: string): string[] {
  const out: string[] = [];
  mathParser.parse(src).iterate({
    enter(n) {
      if (isMath(n.type)) {
        const name = n.name === "MathBlock" && mathDelimiters(n.node) === null ? `${n.name}(unclosed)` : n.name;
        const path = [name];
        for (let a = n.node.parent; a !== null && !a.type.isTop; a = a.parent)
          if (a.type.is("BlockContext")) path.unshift(a.name);
        out.push(`${path.join("/")}:${src.slice(n.from, n.to)}`);
      } else if (n.name === "Escape" || n.name === "Emphasis")
        out.push(`${n.name}:${src.slice(n.from, n.to)}`);
    },
  });
  return out;
}

/** 1-based lines none of whose content any leaf-level node covers. A math
 *  node dump cannot see a parser that consumes a line without giving it a
 *  node — the one after a block's closer, say — so this backstops every
 *  case. (A partly covered line passes: a closer's tail has no node.) */
function uncoveredLines(src: string): number[] {
  const spans: [number, number][] = [];
  mathParser.parse(src).iterate({
    enter(n) {
      if (n.type.isTop || n.type.is("BlockContext")) return;
      spans.push([n.from, n.to]);
      return false; // a node covers its children
    },
  });
  const out: number[] = [];
  let pos = 0;
  src.split("\n").forEach((line, i) => {
    let covered = false;
    for (let k = 0; k < line.length && !covered; k++)
      if (!/\s/.test(line[k])) covered = spans.some(([f, t]) => f <= pos + k && pos + k < t);
    if (!covered && /\S/.test(line)) out.push(i + 1);
    pos += line.length + 1;
  });
  return out;
}

// These are JS string literals: one LaTeX backslash is written `\\`.
describe("dollar math (mirrors comrak's math_dollars)", () => {
  it("parses inline and display math on one line", () => {
    expect(math("a $x+y$ b")).toEqual(["InlineMath:$x+y$"]);
    expect(math("$$x = y$$")).toEqual(["DisplayMath:$$x = y$$"]);
  });

  it("lets inline math cross a soft line break, like comrak", () => {
    expect(math("x $a\nb$ y")).toEqual(["InlineMath:$a\nb$"]);
  });

  it("never parses escapes or emphasis inside an equation", () => {
    expect(math("$\\;a_b\\,$")).toEqual(["InlineMath:$\\;a_b\\,$"]);
    expect(math("$*x*$ and\n\n$$\n\\;x\\,\n$$")).toEqual([
      "InlineMath:$*x*$",
      "MathBlock:$$\n\\;x\\,\n$$",
    ]);
    // Control: the same constructs outside math do parse.
    expect(math("\\; and *x*")).toEqual(["Escape:\\;", "Emphasis:*x*"]);
  });

  it("keeps currency and loose dollars as text", () => {
    expect(math("$5 and $10")).toEqual([]);
    expect(math("$ x$")).toEqual([]); // space after the opener
    expect(math("$x $")).toEqual([]); // space before the closer
    expect(math("$x$5")).toEqual([]); // closer followed by a digit
    expect(math("$x")).toEqual([]); // unterminated
  });

  it("treats \\$ as a literal dollar", () => {
    expect(math("\\$x$")).toEqual(["Escape:\\$"]);
    expect(math("$a\\$b$")).toEqual(["InlineMath:$a\\$b$"]);
  });

  it("closes inline math on the first $, leaving the rest to re-open", () => {
    expect(math("$a$$b$")).toEqual(["InlineMath:$a$", "InlineMath:$b$"]);
  });

  it("makes three or more dollars plain text, like comrak", () => {
    expect(math("$$$x$$$")).toEqual([]);
    expect(math("$$$$")).toEqual([]);
  });

  it("closes display math only on $$, not a lone $ inside", () => {
    expect(math("$$ a $ b $$")).toEqual(["DisplayMath:$$ a $ b $$"]);
  });

  it("defers to code spans", () => {
    expect(math("`$x$` and `$$y$$`")).toEqual([]);
  });

  it("leaves an unterminated inline $$ as text without swallowing later math", () => {
    expect(math("an $$ open, then $x$")).toEqual(["InlineMath:$x$"]);
  });

  it("counts $$ pairs the way the reading render does", () => {
    expect(countDollarPairs("a $$ b")).toBe(1);
    expect(countDollarPairs("$$x$$")).toBe(2);
    expect(countDollarPairs("$$$ and $$$$")).toBe(0);
    expect(countDollarPairs("`$$` then $$")).toBe(1);
  });
});

// The editor's half of the shared case list; its `about` documents the fields.
describe("$$ blocks (mathBlocks.fixture.json, shared with fs.rs)", () => {
  it("has cases", () => expect(fixture.cases.length).toBeGreaterThan(0));
  for (const c of fixture.cases) {
    it(c.note, () => {
      expect(math(c.input)).toEqual(c.editor);
      expect(uncoveredLines(c.input)).toEqual([]);
    });
  }
});
