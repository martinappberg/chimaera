import { describe, expect, it } from "vitest";
import { clockText, readSync, syncAction, syncName } from "./showSync";
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
  it("reads who leads, and the plugin deck viewer's messages as a follower's moves", () => {
    expect(readSync({ kind: "at", from: "a", title: "k", slide: 0, build: 0, lead: true, reply: true })).toMatchObject({ lead: true, reply: true });
    expect(readSync({ kind: "at", from: "a", title: "k", slide: 0, build: 0, lead: "yes" })).not.toHaveProperty("lead");
  });
});

describe("syncAction", () => {
  const lead = { lead: true };
  const follower = {};
  it("a follower takes the leader's place, never another follower's", () => {
    expect(syncAction(false, lead)).toBe("take");
    expect(syncAction(false, { lead: true, reply: true })).toBe("take");
    expect(syncAction(false, follower)).toBe("ignore");
    expect(syncAction(false, { reply: true })).toBe("ignore");
  });
  it("a leader never takes an answer, takes a follower's move and tells it again, takes another leader's quietly", () => {
    expect(syncAction(true, { reply: true })).toBe("ignore");
    expect(syncAction(true, { lead: true, reply: true })).toBe("ignore");
    expect(syncAction(true, follower)).toBe("take-and-tell");
    expect(syncAction(true, lead)).toBe("take");
  });
  it("two moves at once end on the leader's resulting place in both windows", () => {
    // The presenter goes back to A while the show window goes on to B.
    let presenter = "A";
    let show = "B";
    // Each hears the other's move.
    if (syncAction(false, lead) === "take") show = "A";
    const act = syncAction(true, follower);
    if (act !== "ignore") presenter = "B";
    // The leader tells again what it took; the follower takes it.
    if (act === "take-and-tell" && syncAction(false, lead) === "take") show = presenter;
    expect([presenter, show]).toEqual(["B", "B"]);
  });
});
