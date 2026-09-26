import { describe, expect, it } from "vitest";
import { applyChanges, lineChanges, lineTokens, merge3 } from "./merge";

describe("lineTokens", () => {
  it("keeps each line's terminator and a trailing unterminated line", () => {
    expect(lineTokens("")).toEqual([]);
    expect(lineTokens("a")).toEqual(["a"]);
    expect(lineTokens("a\nb\n")).toEqual(["a\n", "b\n"]);
    expect(lineTokens("a\n\nb")).toEqual(["a\n", "\n", "b"]);
  });
});

describe("merge3", () => {
  const base = "# Title\n\nintro\n\n## A\none\n\n## B\ntwo\n";

  it("takes a change made on one side only", () => {
    const mine = base.replace("one", "ONE");
    expect(merge3(base, mine, base)).toEqual({ clean: true, text: mine });
    expect(merge3(base, base, mine)).toEqual({ clean: true, text: mine });
  });

  it("combines edits to separate sections", () => {
    const mine = base.replace("intro", "my intro");
    const disk = base.replace("two", "agent two");
    expect(merge3(base, mine, disk)).toEqual({
      clean: true,
      text: "# Title\n\nmy intro\n\n## A\none\n\n## B\nagent two\n",
    });
  });

  it("takes an identical change made on both sides once", () => {
    const both = base.replace("one", "uno");
    expect(merge3(base, both, both)).toEqual({ clean: true, text: both });
    const mine = both.replace("intro", "INTRO");
    expect(merge3(base, mine, both)).toEqual({ clean: true, text: mine });
  });

  it("reports overlapping edits as a conflict", () => {
    const mine = base.replace("one", "mine");
    const disk = base.replace("one", "theirs");
    expect(merge3(base, mine, disk)).toEqual({ clean: false, conflicts: 1 });
  });

  it("treats edits to adjacent lines as a conflict (not provably independent)", () => {
    const b = "a\nb\nc\n";
    expect(merge3(b, "A\nb\nc\n", "a\nB\nc\n").clean).toBe(false);
    expect(merge3(b, "A\nb\nc\n", "a\nb\nC\n")).toEqual({ clean: true, text: "A\nb\nC\n" });
  });

  it("merges insertions at different places", () => {
    expect(merge3("a\nb\n", "a\nNEW\nb\n", "a\nb\nNEW2\n")).toEqual({
      clean: true,
      text: "a\nNEW\nb\nNEW2\n",
    });
  });

  it("handles a final-newline change as a line change", () => {
    // Disk dropped the final newline of the last line; mine edited the first.
    expect(merge3("a\nb\nc\n", "A\nb\nc\n", "a\nb\nc")).toEqual({ clean: true, text: "A\nb\nc" });
  });

  it("merges into and out of empty documents", () => {
    expect(merge3("", "", "new\n")).toEqual({ clean: true, text: "new\n" });
    expect(merge3("", "x\n", "y\n").clean).toBe(false);
    expect(merge3("a\n", "a\n", "")).toEqual({ clean: true, text: "" });
  });

  it("merges a large mostly-shared document quickly", () => {
    const lines = Array.from({ length: 20_000 }, (_, i) => (i % 3 === 0 ? "\n" : `line ${i}\n`));
    const big = lines.join("");
    const mine = big.replace("line 100\n", "line one hundred\n");
    const disk = big.replace("line 19999\n", "the end\n");
    const t0 = performance.now();
    const r = merge3(big, mine, disk);
    expect(performance.now() - t0).toBeLessThan(1000);
    expect(r).toEqual({
      clean: true,
      text: big.replace("line 100\n", "line one hundred\n").replace("line 19999\n", "the end\n"),
    });
    const t1 = performance.now();
    expect(applyChanges(big, lineChanges(big, disk))).toBe(disk);
    expect(performance.now() - t1).toBeLessThan(1000);
  });

  it("gives up safely (conflict, not a hang) on a pathologically repetitive document", () => {
    const rep = "x\n".repeat(30_000);
    const t0 = performance.now();
    const r = merge3(rep, `A\n${rep}`, `${rep}B\n`);
    expect(performance.now() - t0).toBeLessThan(2000);
    // Either outcome is safe; a clean result must be the correct one.
    if (r.clean) expect(r.text).toBe(`A\n${rep}B\n`);
    const t1 = performance.now();
    expect(applyChanges(rep, lineChanges(rep, `${rep.slice(2)}y\n`))).toBe(`${rep.slice(2)}y\n`);
    expect(performance.now() - t1).toBeLessThan(2000);
  });
});

describe("lineChanges", () => {
  const cases: [string, string][] = [
    ["", ""],
    ["", "a\n"],
    ["a\n", ""],
    ["a\nb\nc\n", "a\nB\nc\n"],
    ["a\nb\nc\n", "a\nb\nc\nd\n"],
    ["a\nb\nc\n", "x\na\nb\nc\n"],
    ["a\nb\nc", "a\nb\nc\n"],
    ["a\nb\nc\n", "c\nb\na\n"],
    ["same\n", "same\n"],
  ];
  it.each(cases)("turns %j into %j", (before, after) => {
    expect(applyChanges(before, lineChanges(before, after))).toBe(after);
  });

  it("leaves untouched lines outside every change (cursor anchors survive)", () => {
    const before = "keep1\nkeep2\nold\nkeep3\n";
    const changes = lineChanges(before, "keep1\nkeep2\nnew\nkeep3\n");
    expect(changes).toEqual([{ from: 12, to: 16, insert: "new\n" }]);
  });

  it("returns ascending, non-overlapping changes", () => {
    const changes = lineChanges("a\nb\nc\nd\ne\n", "A\nb\nC\nd\nE\n");
    for (let i = 1; i < changes.length; i++) {
      expect(changes[i].from).toBeGreaterThanOrEqual(changes[i - 1].to);
    }
    expect(changes.length).toBe(3);
  });
});
