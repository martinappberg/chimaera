import { describe, expect, it } from "vitest";
import { overlap } from "../paneView";
import { LIVE_MAX, liveViews } from "./liveViews.svelte";

describe("live HTML cards", () => {
  it("keeps a view for the cards most recently on screen", () => {
    const ids = Array.from({ length: LIVE_MAX + 2 }, (_, i) => `t-live-${i}`);
    for (const id of ids) liveViews.seen(id);
    // Newest first: the first ones seen gave their views up.
    expect(ids.map((id) => liveViews.live(id))).toEqual(ids.map((_, i) => i >= ids.length - LIVE_MAX));
    // Scrolled back to, the oldest takes a view again; the next oldest live one gives its up.
    liveViews.seen(ids[0]!);
    expect(liveViews.live(ids[0]!)).toBe(true);
    expect(liveViews.live(ids[2]!)).toBe(false);
    // A card that unmounts frees its place.
    liveViews.drop(ids[0]!);
    expect(liveViews.live(ids[0]!)).toBe(false);
    expect(liveViews.live(ids[2]!)).toBe(true);
    for (const id of ids) liveViews.drop(id);
  });

  it("has one card the user works in, cleared when it goes", () => {
    liveViews.seen("t-a");
    liveViews.activate("t-a");
    expect(liveViews.active).toBe("t-a");
    liveViews.drop("t-a");
    expect(liveViews.active).toBeNull();
  });

  const shot = (src: string, width = 600) => ({ src, width, height: 360 });

  it("keeps a bounded number of stills, newest kept", () => {
    for (let i = 0; i < 20; i++) liveViews.keepStill(`/raw/t${i}/r.html`, shot(`data:${i}`));
    expect(liveViews.still("/raw/t0/r.html", 600)).toBeNull();
    expect(liveViews.still("/raw/t19/r.html", 600)?.src).toBe("data:19");
    // Renewing one moves it to the newest end.
    liveViews.keepStill("/raw/t8/r.html", shot("data:8b"));
    for (let i = 20; i < 31; i++) liveViews.keepStill(`/raw/t${i}/r.html`, shot(`data:${i}`));
    expect(liveViews.still("/raw/t8/r.html", 600)?.src).toBe("data:8b");
    expect(liveViews.still(null, 600)).toBeNull();
  });

  it("never lends one card's still to a card of another width", () => {
    liveViews.keepStill("/raw/tw/r.html", shot("data:narrow", 260));
    expect(liveViews.still("/raw/tw/r.html", 600)).toBeNull();
    expect(liveViews.still("/raw/tw/r.html", 260.4)?.src).toBe("data:narrow");
  });
});

describe("a card's visible part", () => {
  const card = { x: 10, y: 80, width: 400, height: 360 };
  it("is the card cut to its scroller", () => {
    expect(overlap(card, { x: 0, y: 200, width: 800, height: 600 })).toEqual({ x: 10, y: 200, width: 400, height: 240 });
    expect(overlap(card, { x: 0, y: 0, width: 800, height: 900 })).toEqual(card);
  });
  it("is nothing once scrolled out", () => {
    expect(overlap(card, { x: 0, y: 440, width: 800, height: 300 })).toBeNull();
    expect(overlap(card, { x: 0, y: 439.5, width: 800, height: 300 })).toBeNull();
  });
});
