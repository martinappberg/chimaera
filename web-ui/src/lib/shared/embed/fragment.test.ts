import { describe, expect, it } from "vitest";
import { parseFileRef, revealOf } from "../fileRef";
import fixture from "../fileRefs.fixture.json";
import {
  fragmentLabel,
  fragmentReveal,
  parseEmbedFragment,
  rangeOutside,
  tableSlice,
  tableWindow,
} from "./fragment";

describe("parseEmbedFragment", () => {
  it("reads GitHub line ranges", () => {
    expect(parseEmbedFragment("L10-L30", "a.rs")).toEqual({ lines: { start: 10, end: 30 } });
    expect(parseEmbedFragment("#L12", "a.rs")).toEqual({ lines: { start: 12, end: 12 } });
    expect(parseEmbedFragment("L12C3-L14C1", "a.rs")).toEqual({ lines: { start: 12, end: 14, col: 3 } });
    expect(parseEmbedFragment("L5-20", "a.rs")).toEqual({ lines: { start: 5, end: 20 } });
    expect(parseEmbedFragment("L9-L3", "a.rs")).toEqual({ lines: { start: 9, end: 9 } });
    expect(parseEmbedFragment("L0", "a.rs")).toEqual({});
  });

  it("reads PDF pages and regions", () => {
    expect(parseEmbedFragment("page=3", "p.pdf")).toEqual({ at: { page: 3 } });
    expect(parseEmbedFragment("page=4&xywh=160,120,320,240", "p.pdf")).toEqual({
      at: { page: 4, region: { x: 160, y: 120, w: 320, h: 240 } },
    });
    expect(parseEmbedFragment("xywh=percent:10,10,50,50", "a.png").at?.region).toEqual({
      x: 10,
      y: 10,
      w: 50,
      h: 50,
      percent: true,
    });
    expect(parseEmbedFragment("xywh=1,2,0,5", "a.png")).toEqual({});
    expect(parseEmbedFragment("page=0", "p.pdf")).toEqual({});
  });

  it("reads media moments in seconds or clock time", () => {
    expect(parseEmbedFragment("t=30,45", "a.mp4")).toEqual({ at: { time: { start: 30, end: 45 } } });
    expect(parseEmbedFragment("t=1:02", "a.mp4")).toEqual({ at: { time: { start: 62 } } });
    expect(parseEmbedFragment("t=,10", "a.mp4")).toEqual({ at: { time: { start: 0, end: 10 } } });
    expect(parseEmbedFragment("t=npt:1:00:05.5", "a.mp4").at?.time).toEqual({ start: 3605.5 });
    expect(parseEmbedFragment("t=20,10", "a.mp4")).toEqual({ at: { time: { start: 20 } } });
    expect(parseEmbedFragment("t=nope", "a.mp4")).toEqual({});
  });

  it("reads table blocks, the RFC 7111 way", () => {
    expect(parseEmbedFragment("row=5-9", "de.tsv")).toEqual({ at: { table: { row: 5, endRow: 9 } } });
    expect(parseEmbedFragment("row=4", "de.tsv")).toEqual({ at: { table: { row: 4 } } });
    // A backwards span keeps its start, as a reference does.
    expect(parseEmbedFragment("row=9-4", "de.tsv")).toEqual({ at: { table: { row: 9 } } });
    expect(parseEmbedFragment("col=2-3", "de.csv")).toEqual({ at: { table: { col: 2, endCol: 3 } } });
    expect(parseEmbedFragment("cell=5,2-9,4", "de.csv")).toEqual({
      at: { table: { row: 5, col: 2, endRow: 9, endCol: 4 } },
    });
  });

  it("keeps RFC 7111's open end, which a locator drops", () => {
    expect(parseEmbedFragment("row=4-*", "de.csv")).toEqual({ at: { table: { row: 4 } }, rowsToEnd: true });
    // Prose may have peeled the asterisk as bold markup.
    expect(parseEmbedFragment("row=4-", "de.csv").rowsToEnd).toBe(true);
    expect(parseEmbedFragment("row=4-*;8-9", "de.csv").rowsToEnd).toBe(true);
    expect(parseEmbedFragment("row=4", "de.csv").rowsToEnd).toBeUndefined();
    expect(parseEmbedFragment("row=4-*&row=6-7", "de.csv")).toEqual({ at: { table: { row: 6, endRow: 7 } } });
  });

  it("reads sheets and A1 ranges", () => {
    expect(parseEmbedFragment("sheet=Summary%20Q3&range=B2:F9", "b.xlsx")).toEqual({
      at: { sheet: "Summary Q3", range: { row: 2, col: 2, endRow: 9, endCol: 6 } },
    });
    expect(parseEmbedFragment("range=AA10", "b.xlsx").at?.range).toEqual({ row: 10, col: 27 });
    expect(parseEmbedFragment("range=A:C", "b.xlsx").at?.range).toEqual({ col: 1, endCol: 3 });
    expect(parseEmbedFragment("range=$B$2:$D$4", "b.xlsx").at?.range).toEqual({ row: 2, col: 2, endRow: 4, endCol: 4 });
    expect(parseEmbedFragment("range=Genes!B2", "b.xlsx").at).toEqual({ sheet: "Genes", range: { row: 2, col: 2 } });
    expect(parseEmbedFragment("range=B2:??", "b.xlsx")).toEqual({});
  });

  it("reads cells by the file's kind, slides, and falls back to a heading anchor", () => {
    expect(parseEmbedFragment("cell=7", "nb.ipynb")).toEqual({ at: { cell: 7 } });
    // Not a notebook: `cell=` is RFC 7111's `row,col`, and `7` is not one.
    expect(parseEmbedFragment("cell=7", "de.csv")).toEqual({});
    expect(parseEmbedFragment("cell=5,2", "de.csv")).toEqual({ at: { table: { row: 5, col: 2 } } });
    expect(parseEmbedFragment("slide=3", "deck.md")).toEqual({ at: { slide: 3 } });
    expect(parseEmbedFragment("Results", "notes.md")).toEqual({ anchor: "Results" });
    expect(parseEmbedFragment("#my-section%20two", "notes.md")).toEqual({ anchor: "my-section two" });
    expect(parseEmbedFragment("", "a")).toEqual({});
    expect(parseEmbedFragment(null, "a")).toEqual({});
    expect(parseEmbedFragment("bogus=1", "a")).toEqual({});
  });
});

describe("fragmentLabel", () => {
  it("names the piece shown", () => {
    const label = (f: string, path = "x") => fragmentLabel(parseEmbedFragment(f, path));
    expect(label("L10-L30")).toBe("lines 10–30");
    expect(label("L7")).toBe("line 7");
    expect(label("page=3&xywh=1,2,3,4")).toBe("page 3 · region");
    expect(label("row=2-*", "d.csv")).toBe("rows 2–end");
    expect(label("row=5-9", "d.csv")).toBe("rows 5–9");
    expect(label("row=4", "d.csv")).toBe("row 4");
    expect(label("cell=5,2", "d.csv")).toBe("cell 5,2");
    expect(label("cell=5,2-9,4", "d.csv")).toBe("cells 5,2–9,4");
    expect(label("col=3", "d.csv")).toBe("column 3");
    expect(label("col=2-4", "d.csv")).toBe("columns 2–4");
    expect(label("sheet=S&range=A1:F20", "b.xlsx")).toBe("S!A1:F20");
    expect(label("sheet=Q1%20Summary", "b.xlsx")).toBe("Q1 Summary");
    expect(label("range=C:A", "b.xlsx")).toBe("A:C");
    expect(label("range=9:3", "b.xlsx")).toBe("3:9");
    expect(label("t=30,45")).toBe("0:30–0:45");
    expect(label("t=3725")).toBe("from 1:02:05");
    expect(label("cell=2", "nb.ipynb")).toBe("cell 2");
    expect(label("slide=3", "deck.md")).toBe("slide 3");
    expect(label("Results")).toBe("# Results");
    expect(fragmentLabel({})).toBe("");
  });
});

describe("fragmentReveal: open in a pane at the same spot", () => {
  it("hands each viewer its own anchor", () => {
    const reveal = (f: string, path: string) => fragmentReveal(parseEmbedFragment(f, path));
    expect(reveal("L10-L30", "a.rs")).toEqual({ line: 10, endLine: 30 });
    expect(reveal("L12C3", "a.rs")).toEqual({ line: 12, col: 3 });
    expect(reveal("page=3", "p.pdf")).toEqual({ line: 1, page: 3 });
    expect(reveal("t=5,9", "a.mp4")).toEqual({ line: 1, time: { start: 5, end: 9 } });
    expect(reveal("xywh=1,2,3,4", "a.png")).toEqual({ line: 1, region: { x: 1, y: 2, w: 3, h: 4 } });
    // A percent region reaches the viewer as one (it was dropped before).
    expect(reveal("xywh=percent:10,10,50,50", "a.png")).toEqual({
      line: 1,
      region: { x: 10, y: 10, w: 50, h: 50, percent: true },
    });
    // Table rows as RFC 7111 rows (the header line is row 1), not a line.
    expect(reveal("row=5-9", "de.tsv")).toEqual({ line: 1, table: { row: 5, endRow: 9 } });
    expect(reveal("row=5-*", "de.tsv")).toEqual({ line: 1, table: { row: 5 } });
    // A spreadsheet range in sheet coordinates: the view places it on its grid.
    expect(reveal("sheet=S&range=B2:C3", "b.xlsx")).toEqual({
      line: 1,
      sheet: "S",
      range: { row: 2, col: 2, endRow: 3, endCol: 3 },
    });
    expect(reveal("cell=2", "nb.ipynb")).toEqual({ line: 1, cell: 2 });
    expect(reveal("slide=4", "deck.md")).toEqual({ line: 1, slide: 4 });
    expect(reveal("Results", "notes.md")).toBeUndefined();
    expect(fragmentReveal({})).toBeUndefined();
  });

  it("is the Reveal a reference to the same fragment opens", () => {
    const cases: [string, string][] = [
      ["src/app.rs", "L10-L30"],
      ["src/app.rs", "L12C3-L14"],
      ["src/app.rs", "L0"],
      ["paper.pdf", "page=4&xywh=160,120,320,240"],
      ["figs/umap.png", "xywh=percent:25,25,50,50"],
      ["demo.mp4", "t=npt:1:02:03.5,1:02:10"],
      ["results/de.tsv", "row=5-9"],
      ["results/de.tsv", "row=5-*"],
      ["results/de.csv", "cell=5,2-9,4"],
      ["results/de.csv", "col=2"],
      ["summary.xlsx", "sheet=Q1%20Summary&range=B2:F9"],
      ["summary.xlsx", "range=Genes!$B$2:$F$9"],
      ["analysis.ipynb", "cell=7"],
      ["deck.md", "slide=3"],
      ["notes.md", "installation"],
    ];
    for (const [path, frag] of cases) {
      const viaRef = revealOf(parseFileRef(`${path}#${frag}`, { delimited: true }));
      expect(fragmentReveal(parseEmbedFragment(frag, path)), `${path}#${frag}`).toEqual(viaRef);
    }
  });

  it("agrees with every fragment in fileRefs.fixture.json", () => {
    let n = 0;
    for (const c of fixture.refs) {
      const hash = c.input.indexOf("#");
      // Only a clean `path#fragment`: wrappers and trailing punctuation are
      // the reference parser's business, not a card's.
      if (c.expect === null || hash < 0 || /^[(]|[.)]$/.test(c.input)) continue;
      const frag = c.input.slice(hash + 1);
      const expected = revealOf(c.expect as Parameters<typeof revealOf>[0]);
      expect(fragmentReveal(parseEmbedFragment(frag, c.expect.path)), c.input).toEqual(expected);
      n += 1;
    }
    expect(n).toBeGreaterThanOrEqual(30);
  });
});

describe("tableSlice / tableWindow", () => {
  const slice = (f: string, path: string, origin: [number, number] = [0, 0]) =>
    tableSlice(parseEmbedFragment(f, path), origin);

  it("counts the header as row 1, per RFC 7111", () => {
    expect(tableWindow(slice("row=2-4", "d.csv"), true)).toEqual({ offset: 0, limit: 3 });
    expect(tableWindow(slice("row=5-7", "d.csv"), true)).toEqual({ offset: 3, limit: 3 });
    // row=1 selects the header: the data starts right after it.
    expect(tableWindow(slice("row=1-3", "d.csv"), true)).toEqual({ offset: 0, limit: 2 });
    expect(tableWindow(slice("cell=5,2-9,4", "d.csv"), true)).toEqual({ offset: 3, limit: 5 });
  });

  it("starts data at row 1 without a header", () => {
    expect(tableWindow(slice("row=1-3", "x.bed"), false)).toEqual({ offset: 0, limit: 3 });
  });

  it("peeks without rows, from an open start, and caps a long selection", () => {
    expect(tableWindow(undefined, true)).toEqual({ offset: 0, limit: 10 });
    expect(tableWindow(slice("col=2", "d.csv"), true, 6)).toEqual({ offset: 0, limit: 6 });
    expect(tableWindow(slice("row=2-10000", "d.csv"), true)).toEqual({ offset: 0, limit: 50 });
    expect(tableWindow(slice("row=12-*", "d.csv"), true)).toEqual({ offset: 10, limit: 10 });
  });

  it("places an A1 range on the grid through the sheet's origin", () => {
    // Used range from A1: A1 row 2 is the first data row.
    expect(slice("range=B2:F9", "b.xlsx")).toEqual({ row: 2, col: 2, endRow: 9, endCol: 6 });
    expect(tableWindow(slice("range=B2:F9", "b.xlsx"), true)).toEqual({ offset: 0, limit: 8 });
    // Used range from C4 (header row 4, first column C): D5:E6 is the grid's
    // first two data rows, its second and third columns.
    expect(slice("sheet=S&range=D5:E6", "b.xlsx", [3, 2])).toEqual({ row: 2, col: 2, endRow: 3, endCol: 3 });
    expect(tableWindow(slice("range=D5:E6", "b.xlsx", [3, 2]), true)).toEqual({ offset: 0, limit: 2 });
    expect(tableWindow(slice("range=C10:C12", "b.xlsx", [3, 2]), true)).toEqual({ offset: 5, limit: 3 });
    // A row= on a spreadsheet is the grid's own RFC 7111 row, origin or not.
    expect(slice("row=3", "b.xlsx", [3, 2])).toEqual({ row: 3 });
  });

  it("tells a range beside the sheet's data from one on it", () => {
    const r = (a1: string) => parseEmbedFragment(`range=${a1}`, "b.xlsx").at!.range!;
    expect(rangeOutside(r("B2:C3"), [3, 2])).toBe(true); // above the header row
    expect(rangeOutside(r("A5:B9"), [3, 2])).toBe(true); // left of column C
    expect(rangeOutside(r("B2:D6"), [3, 2])).toBe(false); // overlaps: clamped
    expect(rangeOutside(r("C4"), [3, 2])).toBe(false); // the header cell
    expect(rangeOutside(r("A:B"), [3, 2])).toBe(true);
    expect(rangeOutside(r("5:9"), [3, 2])).toBe(false);
    expect(rangeOutside(r("B2:C3"), [0, 0])).toBe(false);
  });
});
