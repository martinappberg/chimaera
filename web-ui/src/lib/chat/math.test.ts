import { describe, expect, it } from "vitest";

import { splitUserMath } from "./math";

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
