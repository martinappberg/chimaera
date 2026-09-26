import JSZip from "jszip";
import { describe, expect, it } from "vitest";
import { NOTES_PART_MAX, readNotesParts, type NotesSlideRef } from "./pptxDeck";

const NOTES_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide";

function slide(n: number): NotesSlideRef {
  return {
    slidePath: `ppt/slides/slide${n}.xml`,
    rels: new Map([["rId2", { type: NOTES_REL, target: `../notesSlides/notesSlide${n}.xml` }]]),
  };
}

const small = (text: string) => `<p:notes><p:txBody><a:p><a:r><a:t>${text}</a:t></a:r></a:p></p:txBody></p:notes>`;

/** A package whose notes parts are `parts` (slide n → notesSlide n). */
async function deck(parts: string[]): Promise<ArrayBuffer> {
  const zip = new JSZip();
  zip.file("[Content_Types].xml", "<Types/>");
  parts.forEach((xml, i) => zip.file(`ppt/notesSlides/notesSlide${i + 1}.xml`, xml));
  return zip.generateAsync({ type: "arraybuffer", compression: "DEFLATE" });
}

/** Rewrite every size field of `name` to `size`: a central directory that
 *  lies about how big the part inflates. */
function lieAboutSize(buf: ArrayBuffer, name: string, size: number): ArrayBuffer {
  const bytes = new Uint8Array(buf.slice(0));
  const view = new DataView(bytes.buffer);
  const want = new TextEncoder().encode(name);
  const nameAt = (at: number, len: number) => len === want.length && want.every((b, k) => bytes[at + k] === b);
  for (let at = 0; at + 46 < bytes.length; at++) {
    const sig = view.getUint32(at, true);
    if (sig === 0x04034b50 && nameAt(at + 30, view.getUint16(at + 26, true))) view.setUint32(at + 22, size, true);
    if (sig === 0x02014b50 && nameAt(at + 46, view.getUint16(at + 28, true))) view.setUint32(at + 24, size, true);
  }
  return bytes.buffer;
}

describe("speaker notes stay bounded", () => {
  it("reads ordinary notes", async () => {
    const bytes = await deck([small("first"), small("second")]);
    const { xml, skipped } = await readNotesParts(bytes, [slide(1), slide(2), slide(3)]);
    expect(xml[0]).toContain("first");
    expect(xml[1]).toContain("second");
    expect(xml[2]).toBeNull();
    expect(skipped.size).toBe(0);
  });

  it("skips a notes part that declares more than the cap, without inflating it", async () => {
    // ~8 MB of XML that deflates to a few KB.
    const bomb = `<p:notes>${"A".repeat(8 * 1024 * 1024)}</p:notes>`;
    const bytes = await deck([bomb, small("after")]);
    expect(bytes.byteLength).toBeLessThan(64 * 1024);
    const { xml, skipped } = await readNotesParts(bytes, [slide(1), slide(2)]);
    expect(xml[0]).toBeNull();
    expect([...skipped]).toEqual([0]);
    expect(xml[1]).toContain("after");
  });

  it("stops inflating at the cap when the declared size lies", async () => {
    const bomb = `<p:notes>${"B".repeat(8 * 1024 * 1024)}</p:notes>`;
    const bytes = lieAboutSize(await deck([small("before"), bomb]), "ppt/notesSlides/notesSlide2.xml", 200);
    const { xml, skipped } = await readNotesParts(bytes, [slide(1), slide(2)]);
    expect(xml[0]).toContain("before");
    expect(xml[1]).toBeNull();
    expect([...skipped]).toEqual([1]);
  });

  it("reads a part right at the cap and skips one byte over", async () => {
    const at = `<x>${"c".repeat(NOTES_PART_MAX - 7)}</x>`;
    expect(new TextEncoder().encode(at).byteLength).toBe(NOTES_PART_MAX);
    const over = `${at} `;
    const { xml, skipped } = await readNotesParts(await deck([at, over]), [slide(1), slide(2)]);
    expect(xml[0]?.length).toBe(NOTES_PART_MAX);
    expect(xml[1]).toBeNull();
    expect([...skipped]).toEqual([1]);
  });
});
