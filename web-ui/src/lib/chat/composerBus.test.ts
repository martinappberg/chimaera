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

  it("a chat mounted twice keeps a return target when one view unmounts", () => {
    const target = (taken: ReturnedSends[]) => ({ room: () => 4, take: (sends: ReturnedSends) => void taken.push(sends) });
    const pane: ReturnedSends[] = [];
    const dock: ReturnedSends[] = [];
    const paneView = {};
    const dockView = {};
    const offPane = registerComposerReturn("s-two", target(pane), paneView);
    const offDock = registerComposerReturn("s-two", target(dock), dockView);
    // Each view's returned message goes to its own composer.
    expect(returnToComposer("s-two", { text: "to the pane", images: [] }, paneView)).toBe(true);
    expect(returnToComposer("s-two", { text: "to the dock", images: [] }, dockView)).toBe(true);
    expect([pane.map((s) => s.text), dock.map((s) => s.text)]).toEqual([["to the pane"], ["to the dock"]]);
    // The dock (mounted last) goes away: the pane is still a target, for its
    // own view and for the session.
    offDock();
    expect(returnableCount("s-two", [{ images: [] }], paneView)).toBe(1);
    expect(returnToComposer("s-two", { text: "still here", images: [] })).toBe(true);
    expect(pane.map((s) => s.text)).toEqual(["to the pane", "still here"]);
    offPane();
    expect(returnToComposer("s-two", { text: "nobody", images: [] }, paneView)).toBe(false);
  });
});
