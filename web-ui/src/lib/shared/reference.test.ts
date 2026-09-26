import { get } from "svelte/store";
import { afterEach, describe, expect, it } from "vitest";
import {
  activeSelection,
  composeProvenanceSuffix,
  composeSelectionReference,
  needsCropUpload,
  referenceNow,
  setReferenceHandler,
  type FileSelection,
} from "./reference";

const crop = new Blob([new Uint8Array([137, 80, 78, 71])], { type: "image/png" });

function file(over: Partial<FileSelection>): FileSelection {
  return { kind: "file", path: "/w/x", startLine: null, endLine: null, text: "", ...over };
}

describe("composeSelectionReference", () => {
  it("keeps the line format for text views", () => {
    const sel = file({ startLine: 3, endLine: 7, text: "let x = 1;\nlet y = 2;" });
    expect(composeSelectionReference("src/a.ts", sel, "terminal")).toBe('@src/a.ts#L3-L7 "let x = 1; let y = 2;" ');
    expect(composeSelectionReference("src/a.ts", sel, "chat")).toBe('@src/a.ts#L3-L7 "let x = 1; let y = 2;" ');
  });
  it("a PDF text selection: page + quote", () => {
    const sel = file({ fragment: "page=3", text: "the effect held\nacross cohorts", label: "p. 3" });
    expect(composeSelectionReference("paper.pdf", sel, "terminal")).toBe(
      '@paper.pdf#page=3 "the effect held across cohorts" ',
    );
  });
  it("a markdown selection names its heading", () => {
    const sel = file({ startLine: 40, endLine: 42, text: "p < 0.01", context: "§ Results" });
    expect(composeSelectionReference("report.md", sel, "chat")).toBe('@report.md#L40-L42 (§ Results) "p < 0.01" ');
  });
  describe("a region with a crop", () => {
    const sel = file({ fragment: "page=3&xywh=72,90,200,120", text: "Figure 2", crop, label: "p. 3 region" });
    it("terminal target: the uploaded crop's path is typed", () => {
      expect(composeSelectionReference("paper.pdf", sel, "terminal", "/h/.chimaera/uploads/s1/ref-1.png")).toBe(
        '@paper.pdf#page=3&xywh=72,90,200,120 "Figure 2" (region image: /h/.chimaera/uploads/s1/ref-1.png) ',
      );
    });
    it("terminal target, upload failed: the locator still goes", () => {
      expect(composeSelectionReference("paper.pdf", sel, "terminal", null)).toBe(
        '@paper.pdf#page=3&xywh=72,90,200,120 "Figure 2" ',
      );
    });
    it("chat target: no path (the crop rides as an attachment)", () => {
      expect(composeSelectionReference("paper.pdf", sel, "chat", "/ignored.png")).toBe(
        '@paper.pdf#page=3&xywh=72,90,200,120 "Figure 2" ',
      );
    });
    it("only a terminal target uploads", () => {
      expect(needsCropUpload(sel, "terminal")).toBe(true);
      expect(needsCropUpload(sel, "chat")).toBe(false);
      expect(needsCropUpload(file({ text: "x" }), "terminal")).toBe(false);
    });
    it("an image region with no text sends no quote", () => {
      const img = file({ fragment: "xywh=160,120,320,240", crop });
      expect(composeSelectionReference("figs/umap.png", img, "terminal", "/u/ref-2.png")).toBe(
        "@figs/umap.png#xywh=160,120,320,240 (region image: /u/ref-2.png) ",
      );
    });
  });
  it("a table block quotes its TSV as built (spacing is data)", () => {
    const sel = file({ fragment: "cell=5,2-6,3", text: "", quote: "gene\\tlog2FC\\nTP53  x\\t2.1" });
    expect(composeSelectionReference("de.tsv", sel, "terminal")).toBe(
      '@de.tsv#cell=5,2-6,3 "gene\\tlog2FC\\nTP53  x\\t2.1" ',
    );
  });
  it("a moment, a notebook cell and a slide", () => {
    expect(composeSelectionReference("demo.mp4", file({ fragment: "t=12.5,20" }), "terminal")).toBe(
      "@demo.mp4#t=12.5,20 ",
    );
    expect(composeSelectionReference("nb.ipynb", file({ fragment: "cell=7", text: "df.head()" }), "chat")).toBe(
      '@nb.ipynb#cell=7 "df.head()" ',
    );
    expect(composeSelectionReference("deck.md", file({ fragment: "slide=3", text: "Results" }), "chat")).toBe(
      '@deck.md#slide=3 "Results" ',
    );
  });
  it("never contains a newline or tab, whatever the producer handed it", () => {
    const nasty = file({
      fragment: "row=1",
      text: "a\nb",
      quote: "x\ny\tz\r",
      context: "head\ning",
      crop,
    });
    for (const target of ["chat", "terminal"] as const) {
      const out = composeSelectionReference("f.csv", nasty, target, "/p\n.png");
      expect(out).not.toMatch(/[\r\n\t]/);
      expect(out.endsWith(" ")).toBe(true);
    }
  });
});

describe("provenance and one-click references", () => {
  afterEach(() => setReferenceHandler(null));
  it("the provenance suffix carries a locator", () => {
    expect(composeProvenanceSuffix(file({ fragment: "page=2" }), "paper.pdf", null)).toBe(" [from @paper.pdf#page=2] ");
  });
  it("referenceNow publishes, references through the handler, then clears", () => {
    const seen: unknown[] = [];
    setReferenceHandler(() => seen.push(get(activeSelection)));
    const sel = file({ fragment: "cell=7" });
    referenceNow("nb", sel);
    expect(seen).toEqual([sel]);
    expect(get(activeSelection)).toBeNull();
  });
});
