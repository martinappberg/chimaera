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

  it("closes before closing brackets, quotes and hyphens, and opens after opening ones", () => {
    for (const [text, n] of [
      ["$\\hat\\beta_0$ (och $\\hat\\beta_1$)", 2],
      ["the pair ($x$, $y$); then [$z$]", 3],
      ["a small $p$-value", 1],
      ['the "$k$" in $k$-means', 2],
      ["one line\n$x$ starts the next", 1],
    ] as const) {
      const html = parser.parse(text, { async: false }) as string;
      expect(html.match(/<math/g), text).toHaveLength(n);
    }
  });

  it("types math in a table cell next to a parenthesis", () => {
    const html = parser.parse("| Data | Val |\n|---|---|\n| 3a | $\\hat\\beta_0$ (och $\\hat\\beta_1$) |\n", {
      async: false,
    }) as string;
    expect(html.match(/<math/g)).toHaveLength(2);
    expect(html).not.toContain("$");
  });

  it("keeps currency ranges and glued dollars literal", () => {
    for (const text of ["costs $5-$10 today", "pay $5 (or $10)", "a$x$b"]) {
      const html = parser.parse(text, { async: false }) as string;
      expect(html, text).not.toContain("<math");
    }
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
