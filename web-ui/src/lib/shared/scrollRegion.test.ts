import { afterEach, describe, expect, it, vi } from "vitest";

import { markScrollRegion, markScrollRegions, watchWidth, type Region } from "./scrollRegion";

const TABLE: Region = { role: "group", label: "scrollable table" };

/** The slice of HTMLElement the marker touches, on a plain object (Vitest
 *  runs in node — no DOM). */
function fake(scrollWidth: number, clientWidth: number, attrs: Record<string, string> = {}) {
  const a = { ...attrs };
  const listeners: Record<string, Array<() => void>> = {};
  return {
    scrollWidth,
    clientWidth,
    hasAttribute: (k: string) => k in a,
    getAttribute: (k: string) => a[k] ?? null,
    setAttribute: (k: string, v: string) => {
      a[k] = v;
    },
    removeAttribute: (k: string) => {
      delete a[k];
    },
    addEventListener: (type: string, fn: () => void) => {
      (listeners[type] ??= []).push(fn);
    },
    fire: (type: string) => {
      for (const fn of listeners[type] ?? []) fn();
      listeners[type] = [];
    },
    attrs: a,
  };
}
const el = (f: ReturnType<typeof fake>) => f as unknown as HTMLElement;

describe("markScrollRegion", () => {
  it("marks an overflowing host as a focusable, named group", () => {
    const host = fake(900, 500);
    markScrollRegion(el(host), TABLE);
    expect(host.attrs).toEqual({ tabindex: "0", role: "group", "aria-label": "scrollable table" });
  });

  it("gives a self-scrolling table (or fence) tabindex only — its own role stays", () => {
    const table = fake(900, 500);
    markScrollRegion(el(table), null);
    expect(table.attrs).toEqual({ tabindex: "0" });
  });

  it("is strict about overflow: a box that fits is no tab stop", () => {
    const fits = fake(500, 500);
    markScrollRegion(el(fits), TABLE);
    expect(fits.attrs).toEqual({});
    const byOne = fake(501, 500);
    markScrollRegion(el(byOne), TABLE);
    expect(byOne.attrs.tabindex).toBe("0");
  });

  it("clears only what it set, and never overwrites the content's own attributes", () => {
    const was = fake(900, 500);
    markScrollRegion(el(was), TABLE);
    was.scrollWidth = 500; // the pane grew: no overflow any more
    markScrollRegion(el(was), TABLE);
    expect(was.attrs).toEqual({});

    // Sanitized agent HTML brought a role and a name: keep them, add reach only.
    const foreign = fake(900, 500, { role: "note", "aria-label": "caveat" });
    markScrollRegion(el(foreign), TABLE);
    expect(foreign.attrs).toEqual({ role: "note", "aria-label": "caveat", tabindex: "0" });
    foreign.scrollWidth = 500;
    markScrollRegion(el(foreign), TABLE);
    expect(foreign.attrs).toEqual({ role: "note", "aria-label": "caveat" });

    // A content tabindex of its own is not ours to touch either way.
    const inert = fake(900, 500, { tabindex: "-1" });
    markScrollRegion(el(inert), null);
    expect(inert.attrs).toEqual({ tabindex: "-1" });
    inert.scrollWidth = 500;
    markScrollRegion(el(inert), null);
    expect(inert.attrs).toEqual({ tabindex: "-1" });
  });

  it("keeps the mark while the element is focused and clears it on focusout", () => {
    const focused = fake(900, 500);
    markScrollRegion(el(focused), null);
    vi.stubGlobal("document", { activeElement: focused });
    focused.scrollWidth = 500;
    markScrollRegion(el(focused), null);
    expect(focused.attrs).toEqual({ tabindex: "0" }); // focus would fall to <body>
    vi.stubGlobal("document", { activeElement: null });
    focused.fire("focusout");
    expect(focused.attrs).toEqual({});
  });
});

describe("markScrollRegions", () => {
  it("marks every target's matches under a root, each by its own overflow", () => {
    const host = fake(900, 500);
    const narrowHost = fake(500, 500);
    const fence = fake(700, 500);
    const bySelector: Record<string, unknown[]> = {
      ".md-table": [host, narrowHost],
      "pre > code, pre": [fence],
    };
    const root = { querySelectorAll: (s: string) => bySelector[s] ?? [] } as unknown as ParentNode;
    markScrollRegions(root, [
      [".md-table", TABLE],
      ["pre > code, pre", null],
    ]);
    expect(host.attrs).toEqual({ tabindex: "0", role: "group", "aria-label": "scrollable table" });
    expect(narrowHost.attrs).toEqual({});
    expect(fence.attrs).toEqual({ tabindex: "0" });
  });
});

/** A ResizeObserver stand-in: records observe/unobserve and lets a test
 *  deliver entries by hand. */
class FakeResizeObserver {
  static last: FakeResizeObserver | null = null;
  observed = new Set<Element>();
  constructor(readonly cb: (entries: ResizeObserverEntry[]) => void) {
    FakeResizeObserver.last = this;
  }
  observe(el: Element) {
    this.observed.add(el);
  }
  unobserve(el: Element) {
    this.observed.delete(el);
  }
  deliver(target: Element, width: number) {
    this.cb([{ target, contentRect: { width } } as ResizeObserverEntry]);
  }
}

describe("watchWidth", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("rechecks on width changes only, once per frame, and stops with the last watcher", () => {
    vi.stubGlobal("ResizeObserver", FakeResizeObserver);
    const host = {} as Element;
    const table = {} as Element;
    const seen: string[] = [];
    const recheck = () => seen.push("host"); // one closure shared by host and content, as callers do
    const stopHost = watchWidth(host, recheck);
    const stopTable = watchWidth(table, recheck);
    const ro = FakeResizeObserver.last!;
    expect(ro.observed.has(host) && ro.observed.has(table)).toBe(true);

    ro.deliver(host, 400); // the first delivery is the initial check
    ro.deliver(host, 400); // unchanged width: nothing
    expect(seen).toEqual(["host"]);
    seen.length = 0;
    ro.cb([
      { target: host, contentRect: { width: 300 } } as ResizeObserverEntry,
      { target: table, contentRect: { width: 800 } } as ResizeObserverEntry,
    ]);
    expect(seen).toEqual(["host"]); // both changed, one callback due, run once

    // A second watcher on an element already measured gets its first check now.
    seen.length = 0;
    const stopLate = watchWidth(host, () => seen.push("late"));
    expect(seen).toEqual(["late"]);

    stopHost();
    expect(ro.observed.has(host)).toBe(true); // "late" still watches it
    stopLate();
    expect(ro.observed.has(host)).toBe(false);
    stopTable();
    expect(ro.observed.size).toBe(0);
  });
});
