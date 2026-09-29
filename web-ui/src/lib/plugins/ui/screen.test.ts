import { describe, expect, it } from "vitest";
import { diffLines, nodes, str, tone } from "./screen";

describe("a screen's small rules", () => {
  it("reads tones and nodes defensively", () => {
    expect(tone("warn")).toBe("warn");
    expect(tone("purple")).toBe("neutral");
    expect(nodes([{ type: "text" }, "x", null, { notype: 1 }])).toHaveLength(1);
    expect(nodes("nope")).toEqual([]);
    expect(str(3)).toBe("3");
    expect(str({}, "d")).toBe("d");
  });
});

describe("the diff node", () => {
  it("marks lines, and words within an edited paragraph in prose", () => {
    const code = diffLines("a\nb\nc", "a\nB\nc", false);
    expect(code.map((l) => l.kind)).toEqual(["same", "del", "add", "same"]);
    const prose = diffLines("the quick brown fox", "the quick red fox", true);
    expect(prose).toHaveLength(1);
    const line = prose[0];
    expect(line.kind).toBe("change");
    if (line.kind === "change") {
      const words = line.words.filter((w) => w.kind !== "same").map((w) => `${w.kind}:${w.text}`);
      expect(words).toEqual(["del:brown", "add:red"]);
    }
  });

  it("stays bounded on huge inputs", () => {
    const big = Array.from({ length: 3000 }, (_, i) => `line ${i}`).join("\n");
    const out = diffLines(big, `${big}\nmore`, false);
    expect(out.length).toBeGreaterThan(3000);
  });
});
