import { describe, expect, it } from "vitest";
import { insertIntoComposer, registerComposer, type InsertPlacement } from "./composerBus";

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
});
