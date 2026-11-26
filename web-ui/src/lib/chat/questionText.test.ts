import { describe, expect, it } from "vitest";
import { askedParts, askedText, optionLabel } from "./questionText";

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

describe("askedParts", () => {
  it("names the file a file link points at, decoded", () => {
    expect(askedParts("Rename [my calc.py](file:///w/my%20demo/calc.py)?")).toEqual([
      { kind: "text", text: "Rename " },
      { kind: "file", text: "my calc.py", path: "/w/my demo/calc.py" },
      { kind: "text", text: "?" },
    ]);
  });

  it("keeps any other link as plain words", () => {
    expect(askedParts("See [the spec](https://example.com/x) or [this](file://host/share/x)?")).toEqual([
      { kind: "text", text: "See the spec or this?" },
    ]);
  });

  it("splits code and bold out, and never reads inside code", () => {
    expect(askedParts("Use **Rust** for `[x](y)`?")).toEqual([
      { kind: "text", text: "Use " },
      { kind: "strong", text: "Rust" },
      { kind: "text", text: " for " },
      { kind: "code", text: "[x](y)" },
      { kind: "text", text: "?" },
    ]);
  });

  it("survives a malformed escape in a file link", () => {
    expect(askedParts("[a](file:///w/%zz)")).toEqual([{ kind: "text", text: "a" }]);
  });
});

describe("optionLabel", () => {
  it("lifts the recommended marker from either end", () => {
    expect(optionLabel("(Recommended) div")).toEqual({ text: "div", recommended: true });
    expect(optionLabel("Use SQLite (Recommended)")).toEqual({ text: "Use SQLite", recommended: true });
  });

  it("leaves every other label, and a label that is only the marker", () => {
    expect(optionLabel("divide")).toEqual({ text: "divide", recommended: false });
    expect(optionLabel("Not (recommended) here")).toEqual({ text: "Not (recommended) here", recommended: false });
    expect(optionLabel("(Recommended)")).toEqual({ text: "(Recommended)", recommended: false });
  });
});
