import { describe, expect, it } from "vitest";
import { hoverTarget, isLineFragment, placePopover, sectionBounds } from "./hover";
import { bodyText, documentContext } from "./model";

describe("what a link previews", () => {
  it("previews files and places in this document, nothing on the web", () => {
    expect(hoverTarget("notes.md#Results")).toEqual({ kind: "file", target: "notes.md#Results", fragment: "Results", byName: false });
    expect(hoverTarget("paper.pdf#page=3")).toMatchObject({ kind: "file", target: "paper.pdf#page=3", fragment: "page=3" });
    expect(hoverTarget("note.md", true)).toMatchObject({ kind: "file", byName: true });
    expect(hoverTarget("#results")).toEqual({ kind: "self", anchor: "results" });
    expect(hoverTarget("#fn-1")).toEqual({ kind: "self", anchor: "fn-1" });
    expect(hoverTarget("#fnref-1")).toBeNull();
    expect(hoverTarget("#L12")).toBeNull();
    expect(hoverTarget("https://example.com/x.md")).toBeNull();
    expect(hoverTarget("mailto:a@b.c")).toBeNull();
    expect(hoverTarget("")).toBeNull();
    expect(hoverTarget("javascript:alert(1)")).toBeNull();
  });

  it("reads a file URL as the path it names", () => {
    expect(hoverTarget("file:///data/run%232/x.png#xywh=0,0,1,1")).toMatchObject({
      target: "/data/run%232/x.png#xywh=0,0,1,1",
      fragment: "xywh=0,0,1,1",
    });
  });

  it("tells a line fragment from a heading", () => {
    expect(isLineFragment("L10-L20")).toBe(true);
    expect(isLineFragment("results")).toBe(false);
    expect(isLineFragment(null)).toBe(false);
  });
});

describe("which part of a document", () => {
  const doc = "---\ntitle: t\n---\n\nIntro.\n\n# Top\n\n## Methods\n\nm\n\n### Detail\n\nd\n\n## Results\n\nr\n";
  const cx = documentContext(bodyText(doc).text);

  it("shows a heading's section, down to the next heading of its rank", () => {
    const b = sectionBounds(cx.outline, "methods", doc.length);
    expect(b.found).toBe(true);
    expect(doc.slice(b.from, b.to)).toBe("## Methods\n\nm\n\n### Detail\n\nd\n\n");
    const r = sectionBounds(cx.outline, "user-content-results", doc.length);
    expect(doc.slice(r.from, r.to)).toBe("## Results\n\nr\n");
    // Heading links are case-insensitive, as the reading view follows them.
    expect(sectionBounds(cx.outline, "Detail", doc.length).found).toBe(true);
  });

  it("falls back to the opening", () => {
    expect(sectionBounds(cx.outline, null, doc.length)).toEqual({ from: 0, to: doc.length, found: true });
    expect(sectionBounds(cx.outline, "nope", doc.length)).toEqual({ from: 0, to: doc.length, found: false });
  });
});

describe("where the popover sits", () => {
  const box = { width: 800, height: 600 };
  const want = { width: 440, height: 340 };

  it("sits below the link, clamped inside the box", () => {
    const p = placePopover({ left: 100, top: 100, right: 160, bottom: 118 }, box, want);
    expect(p).toEqual({ left: 100, width: 440, top: 124, bottom: null, maxHeight: 340 });
    const right = placePopover({ left: 700, top: 100, right: 760, bottom: 118 }, box, want);
    expect(right.left).toBe(800 - 440 - 8);
  });

  it("goes above when below is short, growing away from the link", () => {
    const p = placePopover({ left: 100, top: 500, right: 160, bottom: 518 }, box, want);
    expect(p.top).toBeNull();
    expect(p.bottom).toBe(600 - 500 + 6);
    expect(p.maxHeight).toBe(340);
  });

  it("fits a narrow or short box", () => {
    const p = placePopover({ left: 10, top: 40, right: 60, bottom: 58 }, { width: 300, height: 200 }, want);
    expect(p.width).toBe(284);
    expect(p.top).toBe(64);
    expect(p.maxHeight).toBe(200 - 58 - 6 - 8);
  });
});
