import { describe, expect, it } from "vitest";
import {
  composeBoardContext,
  editableText,
  editorFontPx,
  editorTextToParagraphs,
  nextPinId,
  paragraphsToEditorText,
  pinAnchor,
  sameParagraphs,
  snapshotRegion,
  unresolvedPins,
  SNAPSHOT_PAD_PT,
  type ObjInfo,
  type PinInfo,
} from "./boardInteract";
import type { BoardJournalEvent } from "./files";

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

describe("comment pins (journal → unresolved set)", () => {
  const comment = (
    seq: number,
    pin: string,
    extra: Partial<BoardJournalEvent> = {},
  ): BoardJournalEvent => ({
    seq,
    actor: "human",
    event: "comment",
    page: "bench",
    pin,
    text: `note ${pin}`,
    ...extra,
  });
  const resolved = (seq: number, pin: string): BoardJournalEvent => ({
    seq,
    actor: "human",
    event: "comment-resolved",
    pin,
  });
  const move = (seq: number): BoardJournalEvent => ({
    seq,
    actor: "agent",
    event: "move",
    object: "callout",
  });

  it("keeps comments without a matching resolve, in seq order", () => {
    const pins = unresolvedPins([
      comment(1, "c1", { object: "callout" }),
      move(2),
      comment(3, "c2", { at: [320, 96] }),
      resolved(4, "c1"),
    ]);
    expect(pins.map((p) => p.pin)).toEqual(["c2"]);
    expect(pins[0]).toMatchObject({
      pin: "c2",
      seq: 3,
      actor: "human",
      page: "bench",
      object: null,
      at: [320, 96],
      text: "note c2",
    });
  });

  it("is order-aware: a re-used pin id after its resolve is a fresh pin", () => {
    const pins = unresolvedPins([comment(1, "c1"), resolved(2, "c1"), comment(3, "c1")]);
    expect(pins).toHaveLength(1);
    expect(pins[0].seq).toBe(3);
  });

  it("ignores a resolve with no matching comment (its pair compacted away)", () => {
    expect(unresolvedPins([resolved(1, "c9"), comment(2, "c10")])).toHaveLength(1);
  });

  it("mints the next id past every c<n> seen, resolved included", () => {
    expect(nextPinId([])).toBe("c1");
    expect(nextPinId([comment(1, "c1"), resolved(2, "c1"), comment(3, "c4")])).toBe("c5");
    // Foreign id shapes never confuse the counter.
    expect(nextPinId([comment(1, "note-a"), move(2)])).toBe("c1");
  });
});

describe("pinAnchor", () => {
  const objects: ObjInfo[] = [
    { id: "callout", kind: "shape", at: [520, 150], size: [200, 80], text: ["hi"] },
    { id: "ghost", kind: "image", at: null, size: null, text: null },
  ];
  const pin = (object: string | null, at: [number, number] | null): PinInfo => ({
    pin: "c1",
    seq: 1,
    actor: "human",
    page: "bench",
    object,
    at,
    text: "t",
  });

  it("anchors an object-bound pin to the frame's top-right corner (tracks moves)", () => {
    expect(pinAnchor(pin("callout", [1, 1]), objects)).toEqual([720, 150]);
    const moved = objects.map((o) =>
      o.id === "callout" ? { ...o, at: [100, 40] as [number, number] } : o,
    );
    expect(pinAnchor(pin("callout", [1, 1]), moved)).toEqual([300, 40]);
  });

  it("falls back to the stored point when the object is gone or frameless", () => {
    expect(pinAnchor(pin("vanished", [64, 32]), objects)).toEqual([64, 32]);
    expect(pinAnchor(pin("ghost", [8, 8]), objects)).toEqual([8, 8]);
  });

  it("sits a point pin at its stored point, and hides an unanchorable pin", () => {
    expect(pinAnchor(pin(null, [320, 96]), objects)).toEqual([320, 96]);
    expect(pinAnchor(pin("vanished", null), objects)).toBeNull();
    expect(pinAnchor(pin(null, null), objects)).toBeNull();
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
