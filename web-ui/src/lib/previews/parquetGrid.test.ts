import { describe, expect, it } from "vitest";
import {
  ByteCache,
  chunkSpan,
  contentRangeTotal,
  formatCell,
  formatDate,
  groupSpans,
  groupsForRange,
  pageBounds,
  walkCovers,
} from "./parquetGrid";
import { viewKindFor } from "./files";

describe("parquet paging", () => {
  const spans = groupSpans([100, 50, 0, 25]);

  it("lays row groups out end to end", () => {
    expect(spans.map((s) => [s.start, s.rows])).toEqual([
      [0, 100],
      [100, 50],
      [150, 0],
      [150, 25],
    ]);
  });

  it("finds the groups a page of rows touches, group-relative", () => {
    expect(groupsForRange(spans, 90, 20).map((g) => [g.span.index, g.from, g.to])).toEqual([
      [0, 90, 100],
      [1, 0, 10],
    ]);
    // Empty groups are never read; a range past the end touches nothing.
    expect(groupsForRange(spans, 140, 20).map((g) => [g.span.index, g.from, g.to])).toEqual([
      [1, 40, 50],
      [3, 0, 10],
    ]);
    expect(groupsForRange(spans, 500, 10)).toEqual([]);
    expect(groupsForRange(spans, 0, 175).length).toBe(3);
  });

  it("clamps a grid page to the file", () => {
    expect(pageBounds(0, 200, 1_000_000)).toEqual({ start: 0, end: 200 });
    expect(pageBounds(999_950, 200, 1_000_000)).toEqual({ start: 999_950, end: 1_000_000 });
    expect(pageBounds(2_000_000, 200, 1_000_000)).toEqual({ start: 1_000_000, end: 1_000_000 });
    expect(pageBounds(-5, 10, 100)).toEqual({ start: 0, end: 10 });
  });

  it("knows when a walked page list covers a read", () => {
    expect(walkCovers(20_000, false, 200)).toBe(true);
    expect(walkCovers(20_000, false, 20_001)).toBe(false);
    expect(walkCovers(15_000, true, 999_999)).toBe(true);
  });

  it("reads the total size from Content-Range", () => {
    expect(contentRangeTotal("bytes 100-199/16060561")).toBe(16_060_561);
    expect(contentRangeTotal("bytes */42")).toBe(42);
    expect(contentRangeTotal("bytes 0-1/*")).toBeNull();
    expect(contentRangeTotal(null)).toBeNull();
  });

  it("routes .parquet to its viewer, office and boards to theirs", () => {
    expect(viewKindFor("/d/x.parquet")).toBe("parquet");
    expect(viewKindFor("/d/r.docx")).toBe("docx");
    expect(viewKindFor("/d/old.doc")).toBe("binary");
    expect(viewKindFor("/d/deck.pptx")).toBe("pptx");
    expect(viewKindFor("/d/old.ppt")).toBe("binary");
    expect(viewKindFor("/d/b.canvas")).toBe("board");
    expect(viewKindFor("/d/s.excalidraw")).toBe("board");
    expect(viewKindFor("/d/s.excalidraw.json")).toBe("board");
    expect(viewKindFor("/d/plain.json")).toBe("text");
    expect(viewKindFor("/d/f.drawio")).toBe("board");
    expect(viewKindFor("/d/f.drawio.svg")).toBe("image");
    expect(viewKindFor("/d/f.drawio.png")).toBe("image");
  });
});

describe("the byte cache", () => {
  const buf = (n: number, fill = 0) => new Uint8Array(n).fill(fill).buffer;

  it("serves a request from any cached range that holds it", () => {
    const c = new ByteCache(1000);
    c.put(100, buf(100, 7));
    expect(c.get(100, 200)?.byteLength).toBe(100);
    const inner = c.get(120, 130);
    expect(inner?.byteLength).toBe(10);
    expect(new Uint8Array(inner!)[0]).toBe(7);
    expect(c.get(90, 110)).toBeNull();
    expect(c.get(150, 250)).toBeNull();
  });

  it("drops the least recently used ranges past its cap", () => {
    const c = new ByteCache(250);
    c.put(0, buf(100));
    c.put(1000, buf(100));
    c.get(0, 10); // touch the first
    c.put(2000, buf(100));
    expect(c.size).toBe(200);
    expect(c.get(0, 10)).not.toBeNull();
    expect(c.get(1000, 1010)).toBeNull();
    // Too big to cache at all.
    c.put(5000, buf(300));
    expect(c.get(5000, 5010)).toBeNull();
  });

  it("lets a new range swallow the ones inside it", () => {
    const c = new ByteCache(1000);
    c.put(10, buf(10));
    c.put(30, buf(10));
    c.put(0, buf(100));
    expect(c.size).toBe(100);
  });
});

describe("cells", () => {
  it("formats values as grid text", () => {
    expect(formatCell(null)).toBe("");
    expect(formatCell(undefined)).toBe("");
    expect(formatCell(12345678901234567890n)).toBe("12345678901234567890");
    expect(formatCell(true)).toBe("true");
    expect(formatCell(0.1 + 0.2)).toBe("0.30000000000000004");
    expect(formatCell(26.063236236572266, "float32")).toBe("26.06324");
    expect(formatCell(new Uint8Array([1, 0, 255]))).toBe("01 00 ff");
    expect(formatCell({ x: 1n, y: [1, 2], z: 2n ** 60n })).toBe('{"x":1,"y":[1,2],"z":"1152921504606846976"}');
    // A binary column decoded as text shows its control bytes.
    expect(formatCell("\u0000\u0001ab\tc")).toBe("␀␁ab\tc");
    expect(formatCell("x".repeat(5000)).length).toBe(4001);
  });

  it("writes timestamps in UTC and dates without a clock", () => {
    const d = new Date(Date.UTC(2026, 0, 2, 3, 4, 5, 60));
    expect(formatDate(d, "value")).toBe("2026-01-02 03:04:05.060");
    expect(formatDate(new Date(Date.UTC(2026, 0, 2)), "value")).toBe("2026-01-02 00:00:00");
    expect(formatCell(new Date(Date.UTC(2026, 0, 2)), "date")).toBe("2026-01-02");
    expect(formatDate(new Date(NaN), "value")).toBe("invalid date");
  });
});

describe("chunkSpan", () => {
  const meta = (dict: bigint | undefined, data: bigint, size: bigint) => ({
    dictionary_page_offset: dict,
    data_page_offset: data,
    total_compressed_size: size,
  });

  it("starts at the dictionary page when there is one", () => {
    expect(chunkSpan(meta(4n, 104n, 500n), 10_000)).toEqual({ start: 4, end: 504 });
    expect(chunkSpan(meta(undefined, 104n, 500n), 10_000)).toEqual({ start: 104, end: 604 });
  });

  it("reads a dictionary offset of 0 as none, like hyparquet", () => {
    // Not byte 0 of the file: that is the `PAR1` magic.
    expect(chunkSpan(meta(0n, 104n, 500n), 10_000)).toEqual({ start: 104, end: 604 });
  });

  it("refuses a span that isn't a chunk of this file", () => {
    // Inside the leading magic, past the footer, empty.
    expect(chunkSpan(meta(undefined, 2n, 500n), 10_000)).toBeNull();
    expect(chunkSpan(meta(undefined, 9_800n, 500n), 10_000)).toBeNull();
    expect(chunkSpan(meta(undefined, 104n, 0n), 10_000)).toBeNull();
    // A dictionary after its data page, or a data page outside the chunk.
    expect(chunkSpan(meta(900n, 104n, 500n), 10_000)).toBeNull();
    expect(chunkSpan(meta(4n, 700n, 500n), 10_000)).toBeNull();
    // Offsets past what a double holds exactly.
    expect(chunkSpan(meta(undefined, 2n ** 60n, 500n), 2 ** 62)).toBeNull();
    // At the footer's edge is fine.
    expect(chunkSpan(meta(undefined, 9_500n, 500n), 10_000)).toEqual({ start: 9_500, end: 10_000 });
  });
});
