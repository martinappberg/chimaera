import { describe, expect, it } from "vitest";
import {
  insertIntoComposer,
  registerComposer,
  type InsertPlacement,
} from "./composerBus";
import type { ImageAttachment } from "./images";

type Seen = [string, InsertPlacement][];

function composer(): {
  seen: Seen;
  images: string[];
  insert: (text: string, placement: InsertPlacement, image?: ImageAttachment) => void;
} {
  const seen: Seen = [];
  const images: string[] = [];
  return {
    seen,
    images,
    insert: (text, placement, image) => {
      if (text !== "") seen.push([text, placement]);
      if (image !== undefined) images.push(image.label);
    },
  };
}

const crop: ImageAttachment = { media_type: "image/png", data: "AAAA", label: "region 40×30" };

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

  it("delivers a region's pixels with its locator, to one composer", () => {
    const pane = composer();
    const dock = composer();
    const offPane = registerComposer("s-region", pane.insert, {});
    const offDock = registerComposer("s-region", dock.insert, {});
    insertIntoComposer("s-region", "@fig.png#xywh=1,2,3,4 ", "inline", undefined, crop);
    expect(dock.seen).toEqual([["@fig.png#xywh=1,2,3,4 ", "inline"]]);
    expect(dock.images).toEqual([crop.label]);
    expect(pane.images).toEqual([]);
    offDock();
    offPane();
  });

  it("buffers a region's pixels and locator together until a composer mounts", () => {
    insertIntoComposer("s-late", "@doc.pdf#page=2 ", "inline", undefined, crop);
    const c = composer();
    const off = registerComposer("s-late", c.insert);
    expect(c.seen).toEqual([["@doc.pdf#page=2 ", "inline"]]);
    expect(c.images).toEqual([crop.label]);
    off();
  });
});
