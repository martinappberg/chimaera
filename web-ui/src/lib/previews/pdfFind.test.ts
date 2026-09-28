import { describe, expect, it } from "vitest";
import { findInPage, findPattern, itemRanges, pageText } from "./pdfFind";

const items = [
  { str: "The quick ", hasEOL: false },
  { str: "brown", hasEOL: true },
  { str: "fox jumps", hasEOL: false },
  { str: "", hasEOL: true },
  { str: "over the ﬁgure", hasEOL: false },
];

describe("pageText", () => {
  it("joins items in order with a space per end of line", () => {
    const pt = pageText(items);
    expect(pt.text).toBe("The quick brown fox jumps over the ﬁgure");
    expect(pt.starts).toEqual([0, 10, 16, 25, 26]);
    expect(pt.lengths).toEqual([10, 5, 9, 0, 14]);
  });
});

describe("findPattern", () => {
  it("is case-insensitive and blank-safe", () => {
    expect(findPattern("   ")).toBeNull();
    expect("QUICK".match(findPattern("quick")!)).not.toBeNull();
  });
  it("escapes regex syntax", () => {
    expect("a+b (c)".match(findPattern("a+b (c)")!)?.[0]).toBe("a+b (c)");
    expect("aab".match(findPattern(".b")!)).toBeNull();
  });
  it("lets words run together or break across lines", () => {
    expect("foobar".match(findPattern("foo bar")!)?.[0]).toBe("foobar");
    expect("foo  \n bar".match(findPattern("foo bar")!)?.[0]).toBe("foo  \n bar");
  });
  it("matches ligature glyphs", () => {
    expect("the ﬁgure".match(findPattern("figure")!)?.[0]).toBe("ﬁgure");
    expect("eﬀect".match(findPattern("EFFECT")!)?.[0]).toBe("eﬀect");
    expect("oﬃce".match(findPattern("office")!)?.[0]).toBe("oﬃce");
  });
});

describe("matches map back onto text items", () => {
  const pt = pageText(items);
  it("splits a match that spans items", () => {
    // "brown fox": the end of item 1, the EOL space, the start of item 2.
    const [m] = findInPage(pt, findPattern("brown fox")!, 10);
    expect(m).toEqual([
      { item: 1, from: 0, to: 5 },
      { item: 2, from: 0, to: 3 },
    ]);
  });
  it("finds every match on the page, up to the limit", () => {
    const re = findPattern("o")!;
    expect(findInPage(pt, re, 100)).toHaveLength(3);
    expect(findInPage(pt, re, 2)).toHaveLength(2);
    expect(findInPage(pt, re, 0)).toHaveLength(0);
  });
  it("locates a ligature match inside its item", () => {
    expect(findInPage(pt, findPattern("figure")!, 10)).toEqual([[{ item: 4, from: 9, to: 14 }]]);
  });
  it("skips empty items and clips to the range", () => {
    expect(itemRanges(pt, 24, 27)).toEqual([
      { item: 2, from: 8, to: 9 },
      { item: 4, from: 0, to: 1 },
    ]);
    expect(itemRanges(pt, 25, 26)).toEqual([]);
  });
});
