import { describe, expect, it } from "vitest";
import {
  a1ToBlock,
  blockToA1,
  a1Column,
  a1ColumnNumber,
  a1Range,
  clockLabel,
  escapeCell,
  parseA1,
  parseClock,
  parseLocator,
  sheetFragment,
  tableFragment,
  tableLabel,
  timeFragment,
  tsvQuote,
  xywhFragment,
} from "./locator";

describe("formatting round-trips through the parser", () => {
  it("table blocks (RFC 7111 syntax, data rows)", () => {
    const cases = [
      { b: { r0: 5, r1: 9, c0: 1, c1: 4, wholeRows: true }, f: "row=5-9", t: { row: 5, endRow: 9 } },
      { b: { r0: 5, r1: 5, c0: 1, c1: 4, wholeRows: true }, f: "row=5", t: { row: 5 } },
      { b: { r0: 5, r1: 5, c0: 2, c1: 2, wholeRows: false }, f: "cell=5,2", t: { row: 5, col: 2 } },
      {
        b: { r0: 5, r1: 9, c0: 2, c1: 4, wholeRows: false },
        f: "cell=5,2-9,4",
        t: { row: 5, col: 2, endRow: 9, endCol: 4 },
      },
    ];
    for (const c of cases) {
      expect(tableFragment(c.b)).toBe(c.f);
      expect(parseLocator(c.f, "de.csv")).toEqual({ table: c.t });
    }
    expect(tableLabel(cases[0].b)).toBe("rows 5–9");
    expect(tableLabel(cases[2].b)).toBe("cell 5,2");
  });
  it("spreadsheet ranges", () => {
    expect(a1Column(1)).toBe("A");
    expect(a1Column(26)).toBe("Z");
    expect(a1Column(27)).toBe("AA");
    expect(a1Column(703)).toBe("AAA");
    for (const n of [1, 2, 26, 27, 52, 53, 702, 703, 16384]) expect(a1ColumnNumber(a1Column(n))).toBe(n);
    expect(a1Range(2, 2, 9, 6)).toBe("B2:F9");
    expect(a1Range(4, 3, 4, 3)).toBe("C4");
    const f = sheetFragment("Q1 Summary & more", a1Range(2, 2, 9, 6));
    expect(f).toBe("sheet=Q1%20Summary%20%26%20more&range=B2:F9");
    expect(parseLocator(f, "book.xlsx")).toEqual({
      sheet: "Q1 Summary & more",
      range: { row: 2, col: 2, endRow: 9, endCol: 6 },
    });
  });
  it("grid blocks ⇄ A1, wherever the sheet's used range starts", () => {
    const b = { r0: 1, r1: 8, c0: 2, c1: 6, wholeRows: false };
    // Used range from A1: the header is row 1, data row 1 is row 2.
    expect(blockToA1(b, [0, 0])).toBe("B2:F9");
    expect(a1ToBlock({ row: 2, col: 2, endRow: 9, endCol: 6 }, [0, 0])).toEqual({ row: 1, col: 2, endRow: 8, endCol: 6 });
    // Used range from C4 (a title block above, a margin column left).
    expect(blockToA1(b, [3, 2])).toBe("D5:H12");
    expect(a1ToBlock({ row: 5, col: 4, endRow: 12, endCol: 8 }, [3, 2])).toEqual({ row: 1, col: 2, endRow: 8, endCol: 6 });
    // The header row itself lands on the first data row.
    expect(a1ToBlock({ row: 1, col: 1 }, [0, 0])).toEqual({ row: 1, col: 1 });
  });
  it("media moments and ranges", () => {
    expect(timeFragment(12.5)).toBe("t=12.5");
    expect(timeFragment(12.345, 20)).toBe("t=12.35,20");
    expect(timeFragment(20, 10)).toBe("t=20");
    expect(parseLocator(timeFragment(3723.5, 3730), "a.mp4")).toEqual({ time: { start: 3723.5, end: 3730 } });
    expect(clockLabel(12)).toBe("0:12");
    expect(clockLabel(12.5)).toBe("0:12.5");
    expect(clockLabel(3723.5)).toBe("1:02:03.5");
    expect(clockLabel(6.25)).toBe("0:06.25");
    expect(clockLabel(61.05)).toBe("1:01.05");
  });
  it("regions in whole units", () => {
    expect(xywhFragment({ x: 71.6, y: 90.2, w: 200.1, h: 119.9 })).toBe("xywh=72,90,200,120");
    expect(parseLocator(`page=3&${xywhFragment({ x: 72, y: 90, w: 200, h: 120 })}`, "p.pdf")).toEqual({
      page: 3,
      region: { x: 72, y: 90, w: 200, h: 120 },
    });
  });
});

describe("parsing edge cases", () => {
  it("a heading slug or a line range is not a locator", () => {
    expect(parseLocator("#installation", "doc.md")).toBeNull();
    expect(parseLocator("L12-L20", "x.rs")).toBeNull();
    expect(parseLocator("zoom=100", "p.pdf")).toBeNull();
  });
  it("`cell=` follows the extension", () => {
    expect(parseLocator("cell=7", "nb.ipynb")).toEqual({ cell: 7 });
    expect(parseLocator("cell=7", "NB.IPYNB")).toEqual({ cell: 7 });
    expect(parseLocator("cell=7", "de.csv")).toBeNull();
    expect(parseLocator("cell=5,2", "nb.ipynb")).toBeNull();
  });
  it("clocks", () => {
    expect(parseClock("90")).toBe(90);
    expect(parseClock("1:30")).toBe(90);
    expect(parseClock("01:02:03.25")).toBe(3723.25);
    expect(parseClock("1:75")).toBeNull();
    expect(parseClock("x")).toBeNull();
  });
  it("A1 forms", () => {
    expect(parseA1("F9:B2")).toEqual({ row: 2, col: 2, endRow: 9, endCol: 6 });
    expect(parseA1("3:9")).toEqual({ row: 3, endRow: 9 });
    expect(parseA1("")).toBeNull();
    expect(parseA1("A1:B2:C3")).toBeNull();
  });
});

describe("tsvQuote", () => {
  const limits = { maxRows: 50, maxCols: 20, maxChars: 8 * 1024 };
  it("is TSV with literal escapes, header first, on one line", () => {
    const q = tsvQuote(
      ["gene", "log2FC"],
      [
        ["TP53", "2.1"],
        ["a\tb", 'say "hi"\nback\\slash'],
      ],
      limits,
    );
    expect(q).toBe('gene\\tlog2FC\\nTP53\\t2.1\\na\\tb\\tsay \\"hi\\"\\nback\\\\slash');
    expect(q).not.toMatch(/[\t\n\r]/);
  });
  it("escapes cells so no control character survives", () => {
    expect(escapeCell("a\u0007b\r\nc")).toBe("a b\\r\\nc");
  });
  it("caps rows and columns honestly", () => {
    const rows = Array.from({ length: 80 }, (_, r) => Array.from({ length: 25 }, (_, c) => `${r}.${c}`));
    const q = tsvQuote(null, rows, limits);
    const lines = q.split("\\n");
    expect(lines.length).toBe(51);
    expect(lines[50]).toBe("…");
    expect(lines[0].split("\\t").length).toBe(21);
    expect(lines[0].endsWith("\\t…")).toBe(true);
  });
  it("stays within the character budget", () => {
    const rows = Array.from({ length: 50 }, () => ["x".repeat(400)]);
    const q = tsvQuote(["h"], rows, limits);
    expect(q.length).toBeLessThanOrEqual(8 * 1024);
    expect(q.endsWith("\\n…")).toBe(true);
  });
  it("says so when the block starts before the loaded rows", () => {
    expect(tsvQuote(["h"], [["1"]], limits, true)).toBe("h\\n…\\n1");
  });
});
