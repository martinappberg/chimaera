import { afterEach, describe, expect, it, vi } from "vitest";
import { clampPlace, INVITE_KEY, INVITE_MS, inviteFor, inviteText, nextClick, SlideShow, type ShowOptions } from "./showController.svelte";

describe("inviteFor", () => {
  const sync = "chimaera-deck:w:file:/a/talk.pptx";
  it("is taken by a document created after it, while fresh, for its deck only", () => {
    const raw = inviteText(sync, 1_000);
    expect(inviteFor(raw, sync, 2_000, 1_500)).toBe(true);
    // A window that was already open (the deck in another pane) never takes it.
    expect(inviteFor(raw, sync, 2_000, 900)).toBe(false);
    expect(inviteFor(raw, "chimaera-deck:w:file:/a/other.pptx", 2_000, 1_500)).toBe(false);
    expect(inviteFor(raw, sync, 1_000 + INVITE_MS + 1, 1_500)).toBe(false);
  });
  it("ignores what isn't an invite", () => {
    for (const bad of [null, "", "{", "[]", JSON.stringify({ sync, at: "1" }), JSON.stringify({ at: 1 })]) {
      expect(inviteFor(bad, sync, 2_000, 1_500)).toBe(false);
    }
  });
});

describe("clampPlace and nextClick", () => {
  it("holds a place to the deck", () => {
    expect(clampPlace([3, 1], { slide: 5, build: 9 })).toEqual({ slide: 1, build: 0 });
    expect(clampPlace([3, 1], { slide: 0, build: 9 })).toEqual({ slide: 0, build: 2 });
    expect(clampPlace([], { slide: 2, build: 2 })).toEqual({ slide: 0, build: 0 });
  });
  it("is the next build, then the next shown slide's start, then nothing", () => {
    const builds = [2, 3, 1];
    const hidden = [false, true, false];
    expect(nextClick(builds, hidden, { slide: 0, build: 0 })).toEqual({ slide: 0, build: 1 });
    expect(nextClick(builds, hidden, { slide: 0, build: 1 })).toEqual({ slide: 2, build: 0 });
    expect(nextClick(builds, hidden, { slide: 2, build: 0 })).toBeNull();
  });
});

interface Deck {
  keys: string[];
  builds: number[];
  hidden: boolean[];
}

function make(deck: Deck, over: Partial<ShowOptions> = {}) {
  const arrivals: string[] = [];
  // The tests run on Svelte's server build, where effects never run: the
  // channel, the timer and the invite are verified live.
  const show = new SlideShow({
      deck: {
        count: () => deck.keys.length,
        hidden: () => deck.hidden,
        builds: () => deck.builds,
        key: (i) => deck.keys[i] ?? String(i),
        find: (key) => {
          const i = deck.keys.indexOf(key);
          return i === -1 ? null : i;
        },
      },
      sync: () => "test-sync",
      visible: () => true,
      showEl: () => null,
      openWindow: async () => true,
      browseBySlide: true,
      endOnLastBuild: false,
      arrive: (from, to, how) => arrivals.push(`${from.slide}.${from.build}>${to.slide}.${to.build}${how.click ? " click" : ""}${how.heard ? " heard" : ""}`),
      ...over,
  });
  return { show, arrivals };
}

const deck = (): Deck => ({ keys: ["s1", "s2", "s3", "s4"], builds: [1, 3, 2, 1], hidden: [false, false, true, false] });

describe("SlideShow", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("pages browsing slide by slide and the show build by build, hidden slides skipped", async () => {
    const { show, arrivals } = make(deck());
    show.step(1);
    expect(show.at).toEqual({ slide: 1, build: 0 });
    show.step(1);
    expect(show.at).toEqual({ slide: 2, build: 0 });
    show.go({ slide: 1, build: 0 });
    await show.present();
    expect(show.mode).toBe("present");
    show.step(1);
    show.step(1);
    expect(show.at).toEqual({ slide: 1, build: 2 });
    show.step(1);
    // The hidden third slide is skipped.
    expect(show.at).toEqual({ slide: 3, build: 0 });
    show.step(-1);
    expect(show.at).toEqual({ slide: 1, build: 2 });
    expect(arrivals.at(-2)).toBe("1.2>3.0 click");
    show.edge("last");
    expect(show.at).toEqual({ slide: 3, build: 0 });
    await show.leave();
    expect(show.mode).toBe("browse");
  });

  it("lands an End on the last build when asked to", () => {
    const { show } = make(deck(), { endOnLastBuild: true, browseBySlide: false });
    show.go({ slide: 1, build: 0 });
    show.edge("last");
    expect(show.at).toEqual({ slide: 3, build: 0 });
    show.step(-1);
    // Browsing build by build walks the hidden slide too.
    expect(show.at).toEqual({ slide: 2, build: 1 });
  });

  it("stays on the same slide by key when the deck is re-read, the build clamped", () => {
    const d = deck();
    const { show } = make(d);
    show.go({ slide: 1, build: 2 });
    d.keys = ["s2", "s1", "s3", "s4"];
    d.builds = [2, 1, 2, 1];
    show.relocate();
    expect(show.at).toEqual({ slide: 0, build: 1 });
    // A slide that went: the place stays where it was, held to the deck.
    d.keys = ["s1"];
    d.builds = [1];
    d.hidden = [false];
    show.relocate();
    expect(show.at).toEqual({ slide: 0, build: 0 });
  });

  it("opens the show's window: the presenter's view once it opened, nothing when it didn't", async () => {
    const store = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    });
    const ok = make(deck());
    await ok.show.showInWindow();
    expect(ok.show.mode).toBe("presenter");
    expect(store.has(INVITE_KEY)).toBe(true);
    store.clear();
    const blocked = make(deck(), { openWindow: async () => false });
    await blocked.show.showInWindow();
    expect(blocked.show.mode).toBe("browse");
    expect(blocked.show.windowError).not.toBeNull();
    expect(store.has(INVITE_KEY)).toBe(false);
  });
});
