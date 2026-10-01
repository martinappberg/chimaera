import { describe, expect, it } from "vitest";
import {
  insertIntoComposer,
  registerComposer,
  registerComposerReturn,
  returnableCount,
  returnToComposer,
  type InsertPlacement,
  type ReturnedSends,
} from "./composerBus";

type Seen = [string, InsertPlacement][];

function composer(): { seen: Seen; insert: (text: string, placement: InsertPlacement) => void } {
  const seen: Seen = [];
  return { seen, insert: (text, placement) => seen.push([text, placement]) };
}

describe("composerBus", () => {
  it("drains a buffered insert with its placement once the composer mounts", () => {
    insertIntoComposer("s-buffer", "@a.ts ");
    insertIntoComposer("s-buffer", "> q\n\n", "block");
    const c = composer();
    const off = registerComposer("s-buffer", c.insert);
    expect(c.seen).toEqual([
      ["@a.ts ", "inline"],
      ["> q\n\n", "block"],
    ]);
    off();
  });

  it("routes an insert to the named view when the chat is mounted twice", () => {
    const pane = composer();
    const dock = composer();
    const paneView = {};
    const offPane = registerComposer("s-twice", pane.insert, paneView);
    const offDock = registerComposer("s-twice", dock.insert, {});
    insertIntoComposer("s-twice", "> from the pane\n\n", "block", paneView);
    insertIntoComposer("s-twice", "@term:x ");
    expect(pane.seen).toEqual([["> from the pane\n\n", "block"]]);
    expect(dock.seen).toEqual([["@term:x ", "inline"]]);
    offPane();
    // An unmounted view falls back to the session's composer.
    insertIntoComposer("s-twice", "> late\n\n", "block", paneView);
    expect(dock.seen.at(-1)).toEqual(["> late\n\n", "block"]);
    offDock();
  });

  it("returns a message whole or not yet: its pictures must fit the composer", () => {
    const picture = { media_type: "image/png", data: "AA==", label: "shot" };
    const drafts = [
      { text: "one", images: [picture, picture] },
      { text: "two", images: [picture, picture, picture] },
      { text: "three", images: [] },
    ];
    // No composer mounted: nothing can be taken, nothing is lost.
    expect(returnableCount("s-return", drafts)).toBe(0);
    expect(returnToComposer("s-return", { text: "one", images: drafts[0].images })).toBe(false);

    let attached = 2;
    const taken: ReturnedSends[] = [];
    const off = registerComposerReturn("s-return", {
      room: () => 4 - attached,
      take: (sends) => {
        taken.push(sends);
        attached += sends.images.length;
      },
    });
    // Room for two pictures: the first message fits, the second (three
    // more) does not, and the third waits behind it to keep the order.
    expect(returnableCount("s-return", drafts)).toBe(1);
    expect(returnToComposer("s-return", { text: "two", images: drafts[1].images })).toBe(false);
    expect(taken).toEqual([]);
    expect(returnToComposer("s-return", { text: "one", images: drafts[0].images })).toBe(true);
    expect(taken).toEqual([{ text: "one", images: [picture, picture] }]);
    // The user sends what is in the composer: there is room again.
    attached = 0;
    expect(returnableCount("s-return", drafts.slice(1))).toBe(2);
    off();
    expect(returnableCount("s-return", drafts)).toBe(0);
  });
});
