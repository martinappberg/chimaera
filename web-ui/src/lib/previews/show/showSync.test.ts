import { describe, expect, it } from "vitest";
import { clockText, readSync, syncName } from "./showSync";
import * as deck from "../../plugins/ui/deck";

describe("showSync", () => {
  it("names a file's channel by workspace and path", () => {
    expect(syncName("w-1", "file", "/home/a/talk.pptx")).toBe("chimaera-deck:w-1:file:/home/a/talk.pptx");
  });
  it("reads a slide part as the key", () => {
    expect(readSync({ kind: "at", from: "a", title: "ppt/slides/slide3.xml", slide: 2, build: 4 })).toEqual({
      kind: "at",
      from: "a",
      title: "ppt/slides/slide3.xml",
      slide: 2,
      build: 4,
    });
    expect(clockText(61_000)).toBe("1:01");
  });
  it("is what the plugin deck viewer reads through deck.ts", () => {
    expect(deck.syncName).toBe(syncName);
    expect(deck.readSync).toBe(readSync);
    expect(deck.clockText).toBe(clockText);
  });
});
