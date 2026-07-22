import { describe, expect, it } from "vitest";
import {
  composeBoardContext,
  editableText,
  editorFontPx,
  editorTextToParagraphs,
  paragraphsToEditorText,
  sameParagraphs,
  snapshotRegion,
  SNAPSHOT_PAD_PT,
} from "./boardInteract";

describe("editableText", () => {
  it("projects plain paragraphs for the text-op kinds only", () => {
    expect(editableText("text", ["a", "b"])).toEqual(["a", "b"]);
    expect(editableText("shape", ["label"])).toEqual(["label"]);
    // The daemon's text op bails for every other kind — no editor there.
    expect(editableText("callout", ["hi"])).toBeNull();
    expect(editableText("image", undefined)).toBeNull();
    expect(editableText(undefined, ["x"])).toBeNull();
  });

  it("flattens rich runs the same way the edit op would", () => {
    expect(
      editableText("text", [{ runs: [{ t: "bold" }, { t: " tail" }] }, "plain"]),
    ).toEqual(["bold tail", "plain"]);
  });

  it("treats a missing/malformed text field as an empty editor, not ineligible", () => {
    expect(editableText("shape", undefined)).toEqual([]);
    expect(editableText("text", "not-an-array")).toEqual([]);
    expect(editableText("text", [42, { runs: "nope" }, null])).toEqual(["", "", ""]);
  });
});

describe("editor text round trip (paragraphs = newlines)", () => {
  it("round-trips plain paragraphs exactly", () => {
    const paras = ["Results", "", "p < 0.01 across both cohorts"];
    expect(editorTextToParagraphs(paragraphsToEditorText(paras))).toEqual(paras);
  });

  it("normalizes pasted CR/CRLF line endings", () => {
    expect(editorTextToParagraphs("a\r\nb\rc")).toEqual(["a", "b", "c"]);
  });

  it("maps a fully empty editor to no paragraphs (clearing writes [])", () => {
    expect(editorTextToParagraphs("")).toEqual([]);
    expect(paragraphsToEditorText([])).toBe("");
  });

  it("gates the no-change no-op commit", () => {
    expect(sameParagraphs(["a", "b"], ["a", "b"])).toBe(true);
    expect(sameParagraphs(["a"], ["a", ""])).toBe(false);
    expect(sameParagraphs(["a"], ["b"])).toBe(false);
    expect(sameParagraphs([], [])).toBe(true);
  });
});

describe("editorFontPx", () => {
  it("scales with the box height per seeded line and the stage scale", () => {
    const one = editorFontPx([200, 40], 1, 1);
    const two = editorFontPx([200, 40], 1, 2);
    expect(one).toBeGreaterThan(two);
    expect(editorFontPx([200, 40], 2, 1)).toBeGreaterThan(one);
  });

  it("clamps to a sane band", () => {
    expect(editorFontPx([100, 800], 1, 1)).toBe(44); // pt ceiling
    expect(editorFontPx([100, 8], 1, 4)).toBe(11); // px floor
  });
});

describe("composeBoardContext", () => {
  it("matches the §6.4 shape with a trailing space and no newline", () => {
    const line = composeBoardContext("figures/fig2.board.json", "results", [
      "callout",
      "arrow-1",
    ]);
    expect(line).toBe("[board: figures/fig2.board.json › results › callout, arrow-1] ");
    expect(/[\r\n]/.test(line)).toBe(false);
  });

  it("keeps a single id unadorned", () => {
    expect(composeBoardContext("deck.board.json", "page-1", ["title"])).toBe(
      "[board: deck.board.json › page-1 › title] ",
    );
  });
});

describe("snapshotRegion", () => {
  const canvas: [number, number] = [960, 540];

  it("pads a single frame by the snapshot padding", () => {
    const r = snapshotRegion([{ at: [100, 80], size: [200, 100] }], canvas);
    expect(r).toEqual({
      at: [100 - SNAPSHOT_PAD_PT, 80 - SNAPSHOT_PAD_PT],
      size: [200 + 2 * SNAPSHOT_PAD_PT, 100 + 2 * SNAPSHOT_PAD_PT],
    });
  });

  it("unions multiple frames", () => {
    const r = snapshotRegion(
      [
        { at: [100, 100], size: [50, 50] },
        { at: [300, 200], size: [40, 40] },
      ],
      canvas,
      0,
    );
    expect(r).toEqual({ at: [100, 100], size: [240, 140] });
  });

  it("clamps to the canvas at the edges", () => {
    const r = snapshotRegion([{ at: [0, 0], size: [960, 540] }], canvas);
    expect(r).toEqual({ at: [0, 0], size: [960, 540] });
  });

  it("is null with no frames or a degenerate region", () => {
    expect(snapshotRegion([], canvas)).toBeNull();
    expect(snapshotRegion([{ at: [2000, 2000], size: [10, 10] }], canvas)).toBeNull();
  });
});
