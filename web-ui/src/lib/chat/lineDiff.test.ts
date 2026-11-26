import { describe, expect, it } from "vitest";
import { diffLines, diffStat } from "./lineDiff";

const shape = (rows: ReturnType<typeof diffLines>) =>
  rows.map((r) => `${r.kind === "ctx" ? " " : r.kind === "add" ? "+" : r.kind === "del" ? "-" : "~"}${r.text}`);

describe("diffLines", () => {
  it("shows an appended line against a whole-file before and after", () => {
    const rows = diffLines("alpha\nbeta\ngamma\n", "alpha\nbeta\ngamma\ndelta\n");
    expect(shape(rows)).toEqual(["~1", " beta", " gamma", "+delta"]);
    expect(diffStat(rows)).toEqual({ added: 1, removed: 0 });
  });

  it("pairs a changed line as a removal then an addition", () => {
    const rows = diffLines("a\nb\nc", "a\nB\nc");
    expect(shape(rows)).toEqual([" a", "-b", "+B", " c"]);
  });

  it("folds long unchanged runs between two changes", () => {
    const before = Array.from({ length: 12 }, (_, i) => `l${i}`);
    const after = [...before];
    after[1] = "x";
    after[10] = "y";
    expect(shape(diffLines(before.join("\n"), after.join("\n"), 1))).toEqual([
      " l0",
      "-l1",
      "+x",
      " l2",
      "~6",
      " l9",
      "-l10",
      "+y",
      " l11",
    ]);
  });

  it("says nothing moved when only the final newline differs", () => {
    expect(shape(diffLines("a\nb", "a\nb\n"))).toEqual(["~2"]);
    expect(diffLines("", "")).toEqual([]);
  });

  it("reads a new file as additions only", () => {
    expect(shape(diffLines("", "one\ntwo\n"))).toEqual(["+one", "+two"]);
  });

  it("falls back to remove-all, add-all when the middle is too large to compare", () => {
    const a = Array.from({ length: 600 }, (_, i) => `a${i}`).join("\n");
    const b = Array.from({ length: 600 }, (_, i) => `b${i}`).join("\n");
    const rows = diffLines(a, b);
    expect(diffStat(rows)).toEqual({ added: 600, removed: 600 });
    expect(rows[0].kind).toBe("del");
    expect(rows[rows.length - 1].kind).toBe("add");
  });
});
