import { describe, expect, it } from "vitest";
import {
  LINE_BREAK,
  PptxXmlError,
  deleteShape,
  readText,
  readXfrm,
  reorderSlides,
  scan,
  setText,
  setXfrm,
  slideOrder,
} from "./pptxXml";

const NS =
  'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" ' +
  'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" ' +
  'xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"';

/** A slide the way PowerPoint writes one: a title placeholder that inherits
 *  its place, a positioned text box with two formatted runs, a picture. */
const SLIDE = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld ${NS}><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>
<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang="en-US" dirty="0"/><a:t>Results &amp; methods</a:t></a:r><a:endParaRPr lang="en-US" dirty="0"/></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:cNvPr id="4" name="TextBox 3"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="838200" y="1825625"/><a:ext cx="5181600" cy="1200329"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr><p:txBody><a:bodyPr wrap="square"/><a:lstStyle/><a:p><a:pPr marL="0"/><a:r><a:rPr lang="en-US" b="1"/><a:t>Cells</a:t></a:r><a:r><a:rPr lang="en-US"/><a:t> divide fast</a:t></a:r></a:p><a:p><a:r><a:rPr lang="en-US" i="1"/><a:t>second line</a:t></a:r><a:endParaRPr lang="en-US" i="1"/></a:p></p:txBody></p:sp>
<p:pic><p:nvPicPr><p:cNvPr id="5" name="Picture 4"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"/></p:blipFill><p:spPr><a:xfrm rot="5400000"><a:off x="100" y="200"/><a:ext cx="300" cy="400"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>
</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>`;

/** The one differing span between two strings: [start, endA, endB). */
function diffSpan(a: string, b: string): { start: number; a: string; b: string } {
  let s = 0;
  while (s < a.length && s < b.length && a[s] === b[s]) s++;
  let e = 0;
  while (e < a.length - s && e < b.length - s && a[a.length - 1 - e] === b[b.length - 1 - e]) e++;
  return { start: s, a: a.slice(s, a.length - e), b: b.slice(s, b.length - e) };
}

describe("scan", () => {
  it("parses a PowerPoint slide with offsets and namespaces", () => {
    const root = scan(SLIDE);
    expect(root.local).toBe("sld");
    expect(SLIDE.slice(root.start, root.end).startsWith("<p:sld")).toBe(true);
    expect(root.end).toBe(SLIDE.length);
  });

  it("refuses a DOCTYPE, mismatched tags and unclosed elements", () => {
    expect(() => scan('<!DOCTYPE x [<!ENTITY e "x">]><a/>')).toThrow(PptxXmlError);
    expect(() => scan("<a><b></a></b>")).toThrow(PptxXmlError);
    expect(() => scan("<a><b>")).toThrow(PptxXmlError);
    expect(() => scan("<a x=1/>")).toThrow(PptxXmlError);
  });

  it("skips comments, CDATA and processing instructions", () => {
    const root = scan('<?xml version="1.0"?><!-- <x> --><r><![CDATA[<y>]]><z/></r>');
    expect(root.children.map((c) => c.local)).toEqual(["z"]);
  });

  it("resolves a prefix other than a: to DrawingML", () => {
    const xml = `<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:d="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="3" name="x"/></p:nvSpPr><p:spPr><d:xfrm><d:off x="1" y="2"/><d:ext cx="3" cy="4"/></d:xfrm></p:spPr></p:sp></p:spTree></p:cSld></p:sld>`;
    expect(readXfrm(xml, "3")).toEqual({ x: 1, y: 2, cx: 3, cy: 4 });
    const moved = setXfrm(xml, "3", { x: 10, y: 2, cx: 3, cy: 4 });
    expect(diffSpan(xml, moved)).toMatchObject({ a: "", b: "0" });
  });
});

describe("geometry", () => {
  it("reads a shape's own transform, and null for an inherited one", () => {
    expect(readXfrm(SLIDE, "4")).toEqual({ x: 838200, y: 1825625, cx: 5181600, cy: 1200329 });
    expect(readXfrm(SLIDE, "5")).toEqual({ x: 100, y: 200, cx: 300, cy: 400 });
    expect(readXfrm(SLIDE, "2")).toBeNull();
    expect(readXfrm(SLIDE, "99")).toBeNull();
  });

  it("a move rewrites only the offset values", () => {
    const out = setXfrm(SLIDE, "4", { x: 900000, y: 1825625, cx: 5181600, cy: 1200329 });
    expect(diffSpan(SLIDE, out)).toEqual({ start: SLIDE.indexOf('838200"'), a: "8382", b: "9000" });
    expect(readXfrm(out, "4")).toEqual({ x: 900000, y: 1825625, cx: 5181600, cy: 1200329 });
  });

  it("a resize rewrites only the extent values and keeps the rotation", () => {
    const out = setXfrm(SLIDE, "5", { x: 100, y: 200, cx: 600, cy: 800 });
    expect(out).toContain('<a:xfrm rot="5400000"><a:off x="100" y="200"/><a:ext cx="600" cy="800"/>');
    expect(out.length).toBe(SLIDE.length);
  });

  it("an unchanged transform leaves the part identical", () => {
    expect(setXfrm(SLIDE, "4", { x: 838200, y: 1825625, cx: 5181600, cy: 1200329 })).toBe(SLIDE);
  });

  it("moving an inherited placeholder writes the a:xfrm PowerPoint writes", () => {
    const out = setXfrm(SLIDE, "2", { x: 1, y: 2, cx: 3, cy: 4 });
    const d = diffSpan(SLIDE, out);
    expect(d.a).toBe("/");
    expect(d.b).toBe('><a:xfrm><a:off x="1" y="2"/><a:ext cx="3" cy="4"/></a:xfrm></p:spPr');
    expect(readXfrm(out, "2")).toEqual({ x: 1, y: 2, cx: 3, cy: 4 });
  });

  it("an open p:spPr gets the transform as its first child", () => {
    const xml = SLIDE.replace("<p:spPr/>", '<p:spPr><a:noFill/></p:spPr>');
    const out = setXfrm(xml, "2", { x: 1, y: 2, cx: 3, cy: 4 });
    expect(out).toContain('<p:spPr><a:xfrm><a:off x="1" y="2"/><a:ext cx="3" cy="4"/></a:xfrm><a:noFill/></p:spPr>');
  });

  it("edits every branch of an mc:AlternateContent shape", () => {
    const shape = (pfx: string) =>
      `<p:sp><p:nvSpPr><p:cNvPr id="7" name="${pfx}"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="5" y="5"/><a:ext cx="5" cy="5"/></a:xfrm></p:spPr></p:sp>`;
    const xml = `<p:sld ${NS} xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><p:cSld><p:spTree><mc:AlternateContent><mc:Choice Requires="x">${shape("c")}</mc:Choice><mc:Fallback>${shape("f")}</mc:Fallback></mc:AlternateContent></p:spTree></p:cSld></p:sld>`;
    const out = setXfrm(xml, "7", { x: 6, y: 5, cx: 5, cy: 5 });
    expect(out.match(/<a:off x="6"/g)?.length).toBe(2);
    expect(deleteShape(xml, "7")).not.toContain("AlternateContent");
  });

  it("moves a table's graphic frame through p:xfrm", () => {
    const xml = `<p:sld ${NS}><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="9" name="Table"/></p:nvGraphicFramePr><p:xfrm><a:off x="10" y="20"/><a:ext cx="30" cy="40"/></p:xfrm></p:graphicFrame></p:spTree></p:cSld></p:sld>`;
    expect(readXfrm(xml, "9")).toEqual({ x: 10, y: 20, cx: 30, cy: 40 });
    expect(setXfrm(xml, "9", { x: 11, y: 20, cx: 30, cy: 40 })).toContain('<a:off x="11" y="20"/>');
  });

  it("refuses fractional EMU and unknown shapes", () => {
    expect(() => setXfrm(SLIDE, "4", { x: 1.5, y: 0, cx: 1, cy: 1 })).toThrow(PptxXmlError);
    expect(() => setXfrm(SLIDE, "99", { x: 1, y: 0, cx: 1, cy: 1 })).toThrow(PptxXmlError);
  });
});

describe("text", () => {
  it("reads paragraphs with entities decoded", () => {
    expect(readText(SLIDE, "2")).toEqual(["Results & methods"]);
    expect(readText(SLIDE, "4")).toEqual(["Cells divide fast", "second line"]);
    expect(readText(SLIDE, "5")).toBeNull();
  });

  it("changing a word touches only that run's text", () => {
    const out = setText(SLIDE, "4", ["Cells divide slowly", "second line"]);
    expect(diffSpan(SLIDE, out)).toMatchObject({ a: "fast", b: "slowly" });
    expect(readText(out, "4")).toEqual(["Cells divide slowly", "second line"]);
  });

  it("typing at a run boundary continues the run before it", () => {
    const out = setText(SLIDE, "4", ["Cells!! divide fast", "second line"]);
    expect(out).toContain('<a:rPr lang="en-US" b="1"/><a:t>Cells!!</a:t>');
  });

  it("keeps existing entities outside the edit byte-for-byte", () => {
    const out = setText(SLIDE, "2", ["Results & new methods"]);
    expect(diffSpan(SLIDE, out)).toMatchObject({ a: "", b: "new " });
    expect(out).toContain("Results &amp; new methods");
  });

  it("escapes markup typed into the text", () => {
    const out = setText(SLIDE, "2", ["a <b> & c"]);
    expect(out).toContain("<a:t>a &lt;b&gt; &amp; c</a:t>");
    expect(readText(out, "2")).toEqual(["a <b> & c"]);
    expect(() => scan(out)).not.toThrow();
  });

  it("a deletion across runs trims the first and drops the covered ones", () => {
    const out = setText(SLIDE, "4", ["Cefast", "second line"]);
    expect(readText(out, "4")).toEqual(["Cefast", "second line"]);
    expect(out).toContain('<a:rPr lang="en-US" b="1"/><a:t>Ce</a:t>');
    expect(out).toContain('<a:rPr lang="en-US"/><a:t>fast</a:t>');
  });

  it("a new paragraph copies its neighbour's formatting", () => {
    const out = setText(SLIDE, "4", ["Cells divide fast", "second line", "third"]);
    expect(readText(out, "4")).toEqual(["Cells divide fast", "second line", "third"]);
    expect(out).toContain(
      '<a:p><a:r><a:rPr lang="en-US" i="1"/><a:t>third</a:t></a:r><a:endParaRPr lang="en-US" i="1"/></a:p>',
    );
  });

  it("removing a paragraph cuts it whole", () => {
    const out = setText(SLIDE, "4", ["Cells divide fast"]);
    expect(readText(out, "4")).toEqual(["Cells divide fast"]);
    expect(out).not.toContain("second line");
  });

  it("a line break becomes an a:br with the run's formatting", () => {
    const out = setText(SLIDE, "2", [`Results${LINE_BREAK}& methods`]);
    expect(readText(out, "2")).toEqual([`Results${LINE_BREAK}& methods`]);
    expect(out).toContain('<a:br><a:rPr lang="en-US" dirty="0"/></a:br>');
  });

  it("typing into an empty paragraph uses its end-of-paragraph properties", () => {
    const xml = SLIDE.replace(
      "<a:p><a:r><a:rPr lang=\"en-US\" i=\"1\"/><a:t>second line</a:t></a:r>",
      "<a:p>",
    );
    expect(readText(xml, "4")).toEqual(["Cells divide fast", ""]);
    const out = setText(xml, "4", ["Cells divide fast", "hi"]);
    expect(out).toContain('<a:p><a:r><a:rPr lang="en-US" i="1"/><a:t>hi</a:t></a:r><a:endParaRPr');
  });

  it("an unchanged text leaves the part identical", () => {
    expect(setText(SLIDE, "4", ["Cells divide fast", "second line"])).toBe(SLIDE);
  });

  it("never splits a surrogate pair", () => {
    const xml = SLIDE.replace("Results &amp; methods", "a😀b");
    const out = setText(xml, "2", ["a😁b"]);
    expect(readText(out, "2")).toEqual(["a😁b"]);
  });
});

describe("delete", () => {
  it("removes the shape and the blank line before it", () => {
    const out = deleteShape(SLIDE, "5");
    expect(out).not.toContain("Picture 4");
    expect(out).toContain("</p:sp>\n</p:spTree>");
    expect(readText(out, "4")).toEqual(["Cells divide fast", "second line"]);
  });
});

describe("slide order", () => {
  const PRES = `<?xml version="1.0"?>\n<p:presentation ${NS}><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>\n  <p:sldId id="256" r:id="rId2"/>\n  <p:sldId id="257" r:id="rId3"/>\n  <p:sldId id="258" r:id="rId4"/>\n</p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/></p:presentation>`;

  it("reads the order and moves whole sldId elements", () => {
    expect(slideOrder(PRES)).toEqual(["rId2", "rId3", "rId4"]);
    const out = reorderSlides(PRES, ["rId4", "rId2", "rId3"]);
    expect(slideOrder(out)).toEqual(["rId4", "rId2", "rId3"]);
    expect(out.length).toBe(PRES.length);
    expect(out).toContain('<p:sldIdLst>\n  <p:sldId id="258" r:id="rId4"/>\n  <p:sldId id="256" r:id="rId2"/>');
  });

  it("refuses anything but a permutation", () => {
    expect(() => reorderSlides(PRES, ["rId2", "rId3"])).toThrow(PptxXmlError);
    expect(() => reorderSlides(PRES, ["rId2", "rId2", "rId3"])).toThrow(PptxXmlError);
    expect(() => reorderSlides(PRES, ["rId2", "rId3", "rId9"])).toThrow(PptxXmlError);
  });
});
