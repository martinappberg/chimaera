import { describe, expect, it } from "vitest";
import type { TextIndexEntry } from "@aiden0z/pptx-renderer";
import { areaReference, entriesText, itemAt, itemsUnder, namesContext, slideReference, snapBox, type SlideItem } from "./pptxRefs";
import { composeSelectionReference } from "../shared/reference";
import { parseLocator } from "../shared/locator";

const item = (name: string, x: number, y: number, w: number, h: number, text = "", kind = "shape", title = false): SlideItem => ({
  name,
  kind,
  title,
  text,
  box: { x, y, w, h },
});

// A 16:9 slide is 720 × 405 points.
const slide: SlideItem[] = [
  item("Background", 0, 0, 720, 405, "", "picture"),
  item("Panel", 20, 90, 330, 290),
  item("Body 2", 36, 100, 300, 260, "Effect held across cohorts\nn = 412"),
  item("Title 1", 36, 24, 648, 56, "Results", "shape", true),
  item("Chart 3", 380, 100, 300, 260, "", "chart"),
  item("Caption 4", 400, 330, 120, 20, "Figure 2"),
];

describe("slide references", () => {
  it("sends the title as context and the rest of the text, in reading order", () => {
    const sel = slideReference("/w/deck.pptx", 3, slide);
    expect(sel.fragment).toBe("slide=3");
    expect(sel.context).toBe("Results");
    expect(sel.quote).toBe("Effect held across cohorts n = 412 · Figure 2");
    expect(composeSelectionReference("deck.pptx", sel, "chat")).toBe(
      '@deck.pptx#slide=3 (Results) "Effect held across cohorts n = 412 · Figure 2" ',
    );
  });

  it("sends a slide with no text as the bare locator", () => {
    const sel = slideReference("/w/deck.pptx", 1, [item("Picture 1", 0, 0, 720, 405, "", "picture")]);
    expect(composeSelectionReference("deck.pptx", sel, "terminal")).toBe("@deck.pptx#slide=1 ");
  });
});

describe("pointing inside a slide", () => {
  it("names the shapes a box covers and quotes their text", () => {
    const sel = areaReference("/w/deck.pptx", 2, slide, { x: 370, y: 90, w: 320, h: 280 });
    expect(sel.fragment).toBe("slide=2&xywh=370,90,320,280");
    expect(sel.context).toBe("Chart 3, Caption 4");
    expect(sel.quote).toBe("Figure 2");
    // The locator reads back as the same slide and box.
    expect(parseLocator(sel.fragment ?? "", "deck.pptx")).toEqual({ slide: 2, region: { x: 370, y: 90, w: 320, h: 280 } });
  });

  it("takes a box drawn inside a text box to mean that text box, not the backdrop", () => {
    const under = itemsUnder(slide, { x: 60, y: 140, w: 80, h: 40 });
    expect(under.map((i) => i.name)).toEqual(["Body 2"]);
  });

  it("keeps a wordless backdrop only when nothing else is there", () => {
    const under = itemsUnder([item("Background", 0, 0, 720, 405, "", "picture")], { x: 10, y: 10, w: 50, h: 50 });
    expect(under.map((i) => i.name)).toEqual(["Background"]);
  });

  it("picks the frontmost shape that says or shows something under a click", () => {
    expect(itemAt(slide, { x: 450, y: 340 })?.name).toBe("Caption 4");
    expect(itemAt(slide, { x: 450, y: 200 })?.name).toBe("Chart 3");
    // An empty panel is skipped for the text box on it, then the backdrop.
    expect(itemAt(slide, { x: 340, y: 370 })?.name).toBe("Background");
  });

  it("snaps a shape's box outward to whole points on the slide", () => {
    expect(snapBox({ x: 10.4, y: -3, w: 20.2, h: 10 }, 720, 405)).toEqual({ x: 10, y: 0, w: 21, h: 7 });
    expect(snapBox({ x: 800, y: 0, w: 10, h: 10 }, 720, 405)).toBeNull();
  });

  it("caps the names in a context", () => {
    const many = [1, 2, 3, 4, 5].map((n) => item(`Box ${n}`, 0, 0, 1, 1, "x"));
    expect(namesContext(many)).toBe("Box 1, Box 2, Box 3 +2");
  });
});

describe("a node's text", () => {
  const entry = (over: Partial<TextIndexEntry>): TextIndexEntry => ({
    slideIndex: 0,
    nodeId: "4",
    nodePath: "slides/0/nodes/4",
    nodeType: "shape",
    textKind: "shape",
    text: "",
    bounds: { x: 0, y: 0, w: 1, h: 1 },
    ...over,
  });

  it("joins a table's cells across rows", () => {
    const cells = [
      entry({ nodeType: "table", textKind: "table-cell", text: "gene", rowIndex: 0, cellIndex: 0 }),
      entry({ nodeType: "table", textKind: "table-cell", text: "log2FC", rowIndex: 0, cellIndex: 1 }),
      entry({ nodeType: "table", textKind: "table-cell", text: "TP53", rowIndex: 1, cellIndex: 0 }),
      entry({ nodeType: "table", textKind: "table-cell", text: "2.1", rowIndex: 1, cellIndex: 1 }),
    ];
    expect(entriesText(cells)).toBe("gene | log2FC / TP53 | 2.1");
  });

  it("keeps a blank cell's column (the index leaves blank cells out)", () => {
    const cells = [
      entry({ nodeType: "table", textKind: "table-cell", text: "gene", rowIndex: 0, cellIndex: 0 }),
      entry({ nodeType: "table", textKind: "table-cell", text: "log2FC", rowIndex: 0, cellIndex: 1 }),
      entry({ nodeType: "table", textKind: "table-cell", text: "padj", rowIndex: 0, cellIndex: 2 }),
      entry({ nodeType: "table", textKind: "table-cell", text: "MYC", rowIndex: 1, cellIndex: 0 }),
      entry({ nodeType: "table", textKind: "table-cell", text: "0.04", rowIndex: 1, cellIndex: 2 }),
    ];
    expect(entriesText(cells)).toBe("gene | log2FC | padj / MYC |  | 0.04");
  });

  it("puts a group's children on their own lines", () => {
    expect(entriesText([entry({ text: " Step 1 " }), entry({ text: "Step 2" })])).toBe("Step 1\nStep 2");
  });
});
