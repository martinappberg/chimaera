import { describe, expect, it } from "vitest";

import { sliceLines } from "./slice";

const text = "one\ntwo\nthree\nfour";

describe("sliceLines", () => {
  it("takes an inclusive, 1-based range", () => {
    expect(sliceLines(text, 2, 3)).toBe("two\nthree");
    expect(sliceLines(text, 1, 1)).toBe("one");
  });

  it("runs to the end when the end is before the start or past the last line", () => {
    expect(sliceLines(text, 3, 0)).toBe("three\nfour");
    expect(sliceLines(text, 3, 99)).toBe("three\nfour");
  });

  it("is empty out of range, and normalizes line endings", () => {
    expect(sliceLines(text, 9, 10)).toBe("");
    expect(sliceLines(text, 0, 2)).toBe("");
    expect(sliceLines("a\r\nb\r\nc", 2, 2)).toBe("b");
  });
});
