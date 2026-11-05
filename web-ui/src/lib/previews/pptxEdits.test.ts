import JSZip from "jszip";
import { describe, expect, it } from "vitest";
import { DeckEditsCore, type DeckOp } from "./pptxEdits";
import { PART_MAX, faithfulText, packageReader, sha256Hex } from "./pptxPackage";
import { readText, readXfrm } from "./pptxXml";

const NS =
  'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" ' +
  'xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"';

const box = (id: string, x: number, text: string) =>
  `<p:sp><p:nvSpPr><p:cNvPr id="${id}" name="b${id}"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="${x}" y="0"/><a:ext cx="10" cy="10"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:p><a:r><a:rPr lang="en-US"/><a:t>${text}</a:t></a:r></a:p></p:txBody></p:sp>`;
const slide = (...shapes: string[]) => `<p:sld ${NS}><p:cSld><p:spTree>${shapes.join("")}</p:spTree></p:cSld></p:sld>`;

const S1 = "ppt/slides/slide1.xml";

function reader(parts: Record<string, string>) {
  return async (name: string) => {
    const t = parts[name];
    if (t === undefined) throw new Error(`no ${name}`);
    return t;
  };
}

const move = (shape: string, fromX: number, toX: number): DeckOp => ({
  kind: "xfrm",
  part: S1,
  shape,
  from: { x: fromX, y: 0, cx: 10, cy: 10 },
  to: { x: toX, y: 0, cx: 10, cy: 10 },
});

describe("DeckEditsCore", () => {
  it("tracks dirty exactly: undo back to the disk state is clean", async () => {
    const core = new DeckEditsCore(reader({ [S1]: slide(box("2", 0, "a")) }), "h0");
    await core.ensure(S1);
    expect(core.dirty).toBe(false);
    core.push(move("2", 0, 5));
    expect(core.dirty).toBe(true);
    expect(readXfrm(core.text(S1), "2")?.x).toBe(5);
    core.undo();
    expect(core.dirty).toBe(false);
    expect(core.canRedo).toBe(true);
    core.redo();
    expect(readXfrm(core.text(S1), "2")?.x).toBe(5);
  });

  it("folds one gesture into one undo step, keeping the first from", async () => {
    const core = new DeckEditsCore(reader({ [S1]: slide(box("2", 0, "a")) }), "h0");
    await core.ensure(S1);
    core.push(move("2", 0, 1), "drag-1");
    core.push(move("2", 1, 2), "drag-1");
    core.push(move("2", 2, 3), "drag-1");
    expect(core.history).toHaveLength(1);
    expect(core.history[0]).toMatchObject({ from: { x: 0 }, to: { x: 3 } });
    core.push(move("2", 3, 4), "drag-2");
    expect(core.history).toHaveLength(2);
  });

  it("a save keeps the history; undo after a save makes it dirty again", async () => {
    const core = new DeckEditsCore(reader({ [S1]: slide(box("2", 0, "a")) }), "h0");
    await core.ensure(S1);
    core.push(move("2", 0, 5));
    const parts = core.pendingSave();
    expect(parts.map((p) => p.name)).toEqual([S1]);
    core.markSaved(parts, "h1");
    expect(core.dirty).toBe(false);
    expect(core.diskHash).toBe("h1");
    core.undo();
    expect(core.dirty).toBe(true);
    expect(readXfrm(core.text(S1), "2")?.x).toBe(0);
  });

  it("our own save coming back from disk changes nothing", async () => {
    const core = new DeckEditsCore(reader({ [S1]: slide(box("2", 0, "a")) }), "h0");
    await core.ensure(S1);
    core.push(move("2", 0, 5));
    const parts = core.pendingSave();
    core.markSaved(parts, "h1");
    const r = await core.rebase(reader({ [S1]: parts[0]!.text }), "h1");
    expect(r.conflicts).toEqual([]);
    expect(core.canUndo).toBe(true);
  });

  it("an outside change elsewhere on the slide merges with the edits", async () => {
    const core = new DeckEditsCore(reader({ [S1]: slide(box("2", 0, "a"), box("3", 50, "b")) }), "h0");
    await core.ensure(S1);
    core.push(move("2", 0, 5));
    // An agent rewrote shape 3's text meanwhile.
    const r = await core.rebase(reader({ [S1]: slide(box("2", 0, "a"), box("3", 50, "agent")) }), "h2");
    expect(r.conflicts).toEqual([]);
    expect(core.dirty).toBe(true);
    expect(readXfrm(core.text(S1), "2")?.x).toBe(5);
    expect(readText(core.text(S1), "3")).toEqual(["agent"]);
  });

  it("an outside change to the same shape holds the edit for a decision", async () => {
    const core = new DeckEditsCore(reader({ [S1]: slide(box("2", 0, "a")) }), "h0");
    await core.ensure(S1);
    core.push({ kind: "text", part: S1, shape: "2", from: ["a"], to: ["mine"] });
    const r = await core.rebase(reader({ [S1]: slide(box("2", 0, "theirs")) }), "h2");
    expect(r.conflicts).toHaveLength(1);
    expect(core.dirty).toBe(false);
    expect(readText(core.text(S1), "2")).toEqual(["theirs"]);
    core.resolve("mine");
    expect(readText(core.text(S1), "2")).toEqual(["mine"]);
    expect(core.dirty).toBe(true);
    expect(core.conflicts).toHaveLength(0);
  });

  it("keeping theirs drops the held edit; a vanished shape can't be kept", async () => {
    const core = new DeckEditsCore(reader({ [S1]: slide(box("2", 0, "a")) }), "h0");
    await core.ensure(S1);
    core.push(move("2", 0, 5));
    await core.rebase(reader({ [S1]: slide() }), "h2");
    expect(core.conflicts).toHaveLength(1);
    expect(core.resolve("mine")).toBe(1);
    expect(core.dirty).toBe(false);
  });

  it("an edit the file already reflects is dropped as done", async () => {
    const core = new DeckEditsCore(reader({ [S1]: slide(box("2", 5, "a")) }), "h0");
    await core.restore([move("2", 0, 5)]);
    expect(core.conflicts).toHaveLength(0);
    expect(core.history).toHaveLength(0);
  });

  it("a restored journal replays onto the file it was made against", async () => {
    const core = new DeckEditsCore(reader({ [S1]: slide(box("2", 0, "a")) }), "h0");
    await core.restore([move("2", 0, 5), { kind: "text", part: S1, shape: "2", from: ["a"], to: ["b"] }]);
    expect(core.history).toHaveLength(2);
    expect(core.dirty).toBe(true);
    expect(readText(core.text(S1), "2")).toEqual(["b"]);
  });

  it("refuses an edit that can't apply and leaves the history alone", async () => {
    const core = new DeckEditsCore(reader({ [S1]: slide(box("2", 0, "a")) }), "h0");
    await core.ensure(S1);
    expect(() => core.push(move("9", 0, 5))).toThrow();
    expect(core.history).toHaveLength(0);
  });
});

describe("package bytes", () => {
  it("keeps a BOM and refuses text that wouldn't round-trip", () => {
    const enc = new TextEncoder();
    const withBom = new Uint8Array([0xef, 0xbb, 0xbf, ...enc.encode("<a/>")]);
    expect(faithfulText(withBom)).toBe("﻿<a/>");
    expect(enc.encode(faithfulText(withBom)!)).toEqual(withBom);
    expect(faithfulText(new Uint8Array([0xff, 0xfe, 0x3c, 0x00]))).toBeNull();
  });

  it("reads parts from a real package, capped", async () => {
    const zip = new JSZip();
    zip.file(S1, slide(box("2", 0, "a")));
    zip.file("ppt/big.xml", "x".repeat(PART_MAX + 1));
    const bytes = await zip.generateAsync({ type: "arraybuffer", compression: "DEFLATE" });
    const read = await packageReader(bytes);
    expect(await read(S1)).toBe(slide(box("2", 0, "a")));
    await expect(read("ppt/big.xml")).rejects.toThrow(/too large/);
    await expect(read("ppt/none.xml")).rejects.toThrow(/not in this deck/);
  });

  it("hashes like the daemon (SHA-256 hex)", async () => {
    const h = await sha256Hex(new TextEncoder().encode("abc").buffer as ArrayBuffer);
    expect(h).toBe("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  });
});
