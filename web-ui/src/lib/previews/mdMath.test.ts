import { describe, expect, it } from "vitest";
import { markdownLanguage } from "@codemirror/lang-markdown";
import type { MarkdownParser } from "@lezer/markdown";
import { mathExtension } from "./mdMath";

// The APP's parser configuration (GFM + sub/superscript + emoji), not bare
// CommonMark: extension ORDER is part of what these pins protect.
const mathParser = (markdownLanguage.parser as MarkdownParser).configure(mathExtension);

/** Every math node as `Name:source`, plus any Escape, Emphasis, or list the
 *  parser produced — inside an equation there must be none. */
function math(src: string): string[] {
  const out: string[] = [];
  mathParser.parse(src).iterate({
    enter(n) {
      if (n.name === "InlineMath" || n.name === "DisplayMath" || n.name === "MathBlock")
        out.push(`${n.name}:${src.slice(n.from, n.to)}`);
      else if (n.name === "Escape" || n.name === "Emphasis" || n.name === "BulletList")
        out.push(`${n.name}:${src.slice(n.from, n.to)}`);
    },
  });
  return out;
}

// These are JS string literals: one LaTeX backslash is written `\\`.
describe("dollar math (mirrors comrak's math_dollars)", () => {
  it("parses inline and display math on one line", () => {
    expect(math("a $x+y$ b")).toEqual(["InlineMath:$x+y$"]);
    expect(math("$$x = y$$")).toEqual(["DisplayMath:$$x = y$$"]);
  });

  it("lets $$ display math span the lines of its paragraph", () => {
    // Opened mid-line it is inline display math across the paragraph…
    const src = "so $$\np(\\theta \\mid y) \\;=\\; \\frac{a}{b},\n\\qquad\nx\n$$";
    expect(math(src)).toEqual([`DisplayMath:${src.slice(3)}`]);
    // …and opened at a line start it is a block (see below).
    const block = "$$\np(\\theta \\mid y) \\;=\\; \\frac{a}{b},\n\\qquad\nx\n$$";
    expect(math(block)).toEqual([`MathBlock:${block}`]);
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

  it("parses a $$ block whose continuation line would start a list", () => {
    // The reported case: `+ \\left(…` is a bullet to markdown, so inline `$$`
    // pairing across the paragraph fails; the block form keeps it raw.
    const src = "$$ E = \\frac{a}{b}\n+ \\left(1-w\\right) . $$";
    expect(math(src)).toEqual([`MathBlock:${src}`]);
    expect(math("$$\nx\n$$")).toEqual(["MathBlock:$$\nx\n$$"]);
  });

  it("keeps a single-line $$…$$ inline, and a $$ block out of list items", () => {
    expect(math("$$x$$")).toEqual(["DisplayMath:$$x$$"]);
    expect(math("- $$\n  x\n  $$")).toEqual(["BulletList:- $$\n  x\n  $$", "DisplayMath:$$\n  x\n  $$"]);
    expect(math("$$$\nx\n$$$")).toEqual([]);
  });

  it("opens a $$ block inside a blockquote and after prose", () => {
    expect(math("> $$\n> x\n> $$")).toEqual(["MathBlock:$$\n> x\n> $$"]);
    expect(math("prose\n$$\nx\n$$\nafter")).toEqual(["MathBlock:$$\nx\n$$"]);
  });

  it("runs an unterminated $$ block to the end, like a fence", () => {
    expect(math("$$\nx\n\ny")).toEqual(["MathBlock:$$\nx\n\ny"]);
    expect(math("```\n$$\nx\n$$\n```")).toEqual([]);
  });

  // Known live/reading divergences, pinned so a change is deliberate: lezer's
  // GFM Autolink and superscript runs start BEFORE the `$` and scan past it,
  // while comrak (no superscript extension) parses the math first.
  it("loses math swallowed by an earlier autolink or superscript run (documented)", () => {
    expect(math("see http://x.com/$a$ now")).toEqual([]);
    expect(math("x^2,$a^b$ done")).toEqual([]);
    expect(math("x^2 and $a^b$ done")).toEqual(["InlineMath:$a^b$"]);
  });
});
