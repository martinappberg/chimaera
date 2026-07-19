import { Marked } from "marked";
import { describe, expect, it } from "vitest";

import { markdownMath, splitUserMath } from "./math";

const parser = new Marked(markdownMath);

describe("markdownMath", () => {
  it("renders agent dollar and slash delimiters", () => {
    const html = parser.parse(
      "inline $x^2$ and \\(y + 1\\)\n\n$$\nz = 3\n$$\n\n\\[w = 4\\]\n",
      { async: false },
    ) as string;

    expect(html.match(/<math/g)).toHaveLength(4);
    expect(html.match(/display="block"/g)).toHaveLength(2);
  });

  it("leaves code and currency literal", () => {
    const html = parser.parse("`$inline$` and $5 plus $10\n\n```sh\necho '$fenced$'\n```\n", {
      async: false,
    }) as string;

    expect(html).not.toContain("<math");
    expect(html).toContain("$inline$");
    expect(html).toContain("$fenced$");
  });

  it("finds valid math after an earlier currency delimiter", () => {
    const html = parser.parse("costs $5 and then $x$.", { async: false }) as string;

    expect(html.match(/<math/g)).toHaveLength(1);
    expect(html).toContain("$5 and then");
  });
});

describe("splitUserMath", () => {
  it("recognizes the four chat math delimiter forms without changing prose", () => {
    expect(splitUserMath("a \\(x+1\\) b \\[y^2\\] c $z$ d $$q=2$$ e")).toEqual([
      { kind: "text", text: "a " },
      { kind: "math", source: "x+1", display: false },
      { kind: "text", text: " b " },
      { kind: "math", source: "y^2", display: true },
      { kind: "text", text: " c " },
      { kind: "math", source: "z", display: false },
      { kind: "text", text: " d " },
      { kind: "math", source: "q=2", display: true },
      { kind: "text", text: " e" },
    ]);
  });

  it("leaves currency, escaped dollars, and unmatched delimiters verbatim", () => {
    for (const text of ["costs $5 and $10", String.raw`costs \$5`, String.raw`unfinished \(x`]) {
      expect(splitUserMath(text)).toEqual([{ kind: "text", text }]);
    }
  });
});
