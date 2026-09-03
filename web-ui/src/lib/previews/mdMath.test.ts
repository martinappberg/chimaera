import { describe, expect, it } from "vitest";
import { parser } from "@lezer/markdown";
import { mathExtension } from "./mdMath";

const mathParser = parser.configure(mathExtension);

/** Every math node as `Name:source`, plus any Escape parsed inside one. */
function math(src: string): string[] {
  const out: string[] = [];
  mathParser.parse(src).iterate({
    enter(n) {
      if (n.name === "InlineMath" || n.name === "DisplayMath")
        out.push(`${n.name}:${src.slice(n.from, n.to)}`);
      else if (n.name === "Escape") out.push(`Escape:${src.slice(n.from, n.to)}`);
    },
  });
  return out;
}

describe("dollar math (mirrors comrak's math_dollars)", () => {
  it("parses inline and display math on one line", () => {
    expect(math("a $x+y$ b")).toEqual(["InlineMath:$x+y$"]);
    expect(math("$$x = y$$")).toEqual(["DisplayMath:$$x = y$$"]);
  });

  it("lets a $$ block span the lines of its paragraph", () => {
    const src = "$$\np(\\theta \\mid y) \;=\; \\frac{a}{b},\n\\qquad\nx\n$$";
    expect(math(src)).toEqual([`DisplayMath:${src}`]);
  });

  it("never highlights escapes or emphasis inside an equation", () => {
    expect(math("$\;a_b\\,$")).toEqual(["InlineMath:$\;a_b\\,$"]);
    expect(math("$$\n\;x\\,\n$$")).toEqual(["DisplayMath:$$\n\;x\\,\n$$"]);
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

  it("leaves an unterminated $$ as text without swallowing later math", () => {
    expect(math("$$ open, then $x$")).toEqual(["InlineMath:$x$"]);
  });
});
