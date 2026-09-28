import { describe, expect, it } from "vitest";
import {
  autoColumnWidths,
  formatRowCount,
  jumpOffset,
  JUMP_CONTEXT,
  parseRowNumber,
  rowAt,
  virtualWindow,
} from "./tableGrid";
import { presetColumns, tablePreset, tableQuery, viewKindFor } from "./files";

describe("virtualWindow", () => {
  it("renders the visible rows plus overscan, with spacers for the rest", () => {
    const w = virtualWindow(2_500, 500, 25, 10_000, 10);
    // Rows 100..120 are visible (500 / 25 = 20, +1 for a partial row).
    expect(w.start).toBe(90);
    expect(w.end).toBe(131);
    expect(w.padTop).toBe(90 * 25);
    expect(w.padBottom).toBe((10_000 - 131) * 25);
    // Spacers plus rendered rows always add up to the full height.
    expect(w.padTop + (w.end - w.start) * 25 + w.padBottom).toBe(10_000 * 25);
  });

  it("clamps at both ends and for tiny or empty grids", () => {
    expect(virtualWindow(0, 500, 25, 10_000, 10)).toMatchObject({ start: 0, padTop: 0, end: 31 });
    const bottom = virtualWindow(250_000, 500, 25, 10_000, 10);
    expect(bottom.end).toBe(10_000);
    expect(bottom.padBottom).toBe(0);
    expect(virtualWindow(0, 500, 25, 3, 10)).toEqual({ start: 0, end: 3, padTop: 0, padBottom: 0 });
    expect(virtualWindow(100, 500, 25, 0, 10)).toEqual({ start: 0, end: 0, padTop: 0, padBottom: 0 });
    expect(virtualWindow(-40, 500, 0, 10, 10)).toEqual({ start: 0, end: 0, padTop: 0, padBottom: 0 });
  });

  it("keeps a scroll past the end (a shrinking window) inside bounds", () => {
    const w = virtualWindow(1e9, 500, 25, 50, 10);
    expect(w.start).toBeLessThanOrEqual(w.end);
    expect(w.end).toBe(50);
    expect(w.padBottom).toBe(0);
  });

  it("names the top row", () => {
    expect(rowAt(0, 25, 100)).toBe(0);
    expect(rowAt(26, 25, 100)).toBe(1);
    expect(rowAt(1e9, 25, 100)).toBe(99);
    expect(rowAt(10, 25, 0)).toBe(0);
  });
});

describe("autoColumnWidths", () => {
  const opts = { pad: 24, min: 48, maxChars: 40 };
  it("sizes each column by its longest header or sampled cell", () => {
    const widths = autoColumnWidths(["id", "description"], [["1", "short"], ["22", "x"]], 8, opts);
    expect(widths).toEqual([48, 11 * 8 + 24]);
  });
  it("caps a long cell and covers rows wider than the header", () => {
    const widths = autoColumnWidths(["a"], [["y".repeat(500), "extra"]], 7.5, opts);
    // 40 chars × 7.5 + 24, and "extra" (5 chars → 38 + 24) past the header.
    expect(widths).toEqual([324, 62]);
  });
});

describe("formatRowCount", () => {
  it("prints exact counts in full and estimates compactly", () => {
    expect(formatRowCount(1_234_567, true)).toBe("1,234,567");
    expect(formatRowCount(3_000, false)).toBe("~3,000");
    expect(formatRowCount(12_345, false)).toBe("~12.3k");
    expect(formatRowCount(1_200_000, false)).toBe("~1.2M");
    expect(formatRowCount(250_000_000, false)).toBe("~250M");
    expect(formatRowCount(3_400_000_000, false)).toBe("~3.4B");
  });
});

describe("jump-to-row helpers", () => {
  it("fetches with context above the target", () => {
    expect(jumpOffset(1)).toBe(0);
    expect(jumpOffset(1_500_000)).toBe(1_500_000 - 1 - JUMP_CONTEXT);
  });
  it("parses typed row numbers", () => {
    expect(parseRowNumber("1,500,000")).toBe(1_500_000);
    expect(parseRowNumber(" 42 ")).toBe(42);
    expect(parseRowNumber("0")).toBeNull();
    expect(parseRowNumber("-3")).toBeNull();
    expect(parseRowNumber("1e5")).toBeNull();
    expect(parseRowNumber("")).toBeNull();
  });
});

describe("bioinformatics table presets", () => {
  it("routes the formats (and their gzip wrappers) to the table view", () => {
    for (const p of [
      "/d/calls.vcf",
      "/d/calls.vcf.gz",
      "/d/peaks.narrowPeak",
      "/d/x.broadPeak",
      "/d/cov.bedGraph",
      "/d/genes.gff3",
      "/d/genes.gtf.gz",
      "/d/reads.sam",
      "/d/a.bed",
    ]) {
      expect(viewKindFor(p), p).toBe("table");
    }
    expect(viewKindFor("/d/reads.bam")).toBe("binary");
    expect(tablePreset("/d/plain.tsv")).toBeNull();
  });

  it("VCF skips ## meta lines and keeps its #CHROM header", () => {
    const q = tableQuery("/d/calls.vcf.gz", 0, 200);
    expect(q.get("comment")).toBe("##");
    expect(q.get("header")).toBeNull();
    expect(q.get("delim")).toBe("tab");
    expect(q.get("quote")).toBe("false");
    expect(presetColumns("/d/calls.vcf.gz", ["#CHROM", "POS"])).toEqual(["CHROM", "POS"]);
    // Only VCF strips the hash; plain tables keep their names verbatim.
    expect(presetColumns("/d/t.tsv", ["#id", "x"])).toEqual(["#id", "x"]);
  });

  it("BED, GFF and SAM are header-less with their standard names", () => {
    const bed = tableQuery("/d/a.bed", 1_000, 200);
    expect(bed.get("offset_rows")).toBe("1000");
    expect(bed.get("comment")).toBe("#,track,browser");
    expect(bed.get("header")).toBe("false");
    expect(bed.get("names")?.split(",").slice(0, 3)).toEqual(["chrom", "chromStart", "chromEnd"]);
    expect(bed.get("names")?.split(",")).toHaveLength(12);

    const gff = tableQuery("/d/g.gff", 0, 10);
    expect(gff.get("comment")).toBe("#");
    expect(gff.get("names")?.split(",")).toEqual([
      "seqid", "source", "type", "start", "end", "score", "strand", "phase", "attributes",
    ]);
    expect(tableQuery("/d/g.gtf", 0, 10).get("names")?.split(",")[2]).toBe("feature");

    const sam = tableQuery("/d/r.sam", 0, 10);
    expect(sam.get("comment")).toBe("@");
    expect(sam.get("names")?.split(",")).toHaveLength(11);
    expect(sam.get("names")?.split(",")[10]).toBe("QUAL");

    expect(tableQuery("/d/p.narrowPeak", 0, 1).get("names")?.split(",").at(-1)).toBe("peak");
    expect(tableQuery("/d/c.bedGraph", 0, 1).get("names")).toBe("chrom,chromStart,chromEnd,dataValue");
  });

  it("plain CSV/TSV queries carry only paging", () => {
    const q = tableQuery("/d/t.csv", 5, 7);
    expect([...q.keys()]).toEqual(["path", "offset_rows", "limit_rows"]);
  });
});
