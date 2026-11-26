import { describe, expect, it } from "vitest";
import { askedText } from "./questionText";

describe("askedText", () => {
  it("reads a markdown link as its label", () => {
    expect(askedText("What should the helper in [calc.py](file:///w/demo/calc.py) be named?")).toBe(
      "What should the helper in calc.py be named?",
    );
    expect(askedText("See [the spec](https://example.com/a_(b)) first?")).toBe("See the spec first?");
  });

  it("drops bold markers and leaves code spans alone", () => {
    expect(askedText("Use **Rust** or keep `[x](y)` as `**is**`?")).toBe("Use Rust or keep `[x](y)` as `**is**`?");
  });

  it("leaves plain text and bare brackets as they are", () => {
    expect(askedText("Keep items[0] (the first)?")).toBe("Keep items[0] (the first)?");
  });
});
