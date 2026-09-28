import { describe, expect, it } from "vitest";
import { lineLevel, splitLines, wholeLines } from "./logText";

const enc = (s: string) => new TextEncoder().encode(s);

describe("lineLevel", () => {
  it("flags failures from the usual cluster tools", () => {
    for (const line of [
      "Traceback (most recent call last):",
      "ValueError: bad input",
      "slurmstepd: error: *** JOB 123 ON node7 CANCELLED AT 2026-09-25T10:00:00 DUE TO TIME LIMIT ***",
      "srun: error: node7: task 0: Out Of Memory",
      "ERROR ~ Error executing process > 'ALIGN (1)'",
      "Segmentation fault (core dumped)",
      "[FATAL] cannot open file",
    ]) {
      expect(lineLevel(line), line).toBe("error");
    }
  });

  it("flags warnings", () => {
    expect(lineLevel("UserWarning: this is deprecated")).toBe("warn");
    expect(lineLevel("WARN  [main] retrying")).toBe("warn");
  });

  it("leaves clean summaries and ordinary lines alone", () => {
    for (const line of ["0 errors, 0 warnings", "Completed with no errors", "failed: 0", "epoch 3 loss 0.12", ""]) {
      expect(lineLevel(line), line).toBeNull();
    }
    // …but a real failure on the same line still counts.
    expect(lineLevel("0 warnings, 2 errors")).toBe("error");
  });
});

describe("wholeLines", () => {
  it("drops the torn first line of a slice read mid-file", () => {
    const b = enc("tail of a line\nwhole one\nwhole two\npartial");
    const { start, end } = wholeLines(b, false, false);
    expect(new TextDecoder().decode(b.subarray(start, end))).toBe("whole one\nwhole two\n");
  });

  it("keeps the first line at the file start and the last at the file end", () => {
    const b = enc("first\nsecond");
    expect(wholeLines(b, true, true)).toEqual({ start: 0, end: b.length });
  });

  it("keeps a newline-free slice whole (one very long line)", () => {
    const b = enc("x".repeat(100));
    expect(wholeLines(b, false, false)).toEqual({ start: 0, end: 100 });
  });

  it("never splits a multi-byte character", () => {
    const b = enc("ünï\ncødé\n");
    const { start, end } = wholeLines(b, false, false);
    expect(new TextDecoder("utf-8", { fatal: true }).decode(b.subarray(start, end))).toBe("cødé\n");
  });
});

describe("splitLines", () => {
  it("does not open an empty line after a final newline", () => {
    expect(splitLines("a\nb\n")).toEqual(["a", "b"]);
    expect(splitLines("a\n\nb")).toEqual(["a", "", "b"]);
    expect(splitLines("")).toEqual([]);
  });
});
