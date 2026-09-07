import { describe, expect, it } from "vitest";

import { markScrollRegion, markScrollRegions, type Region } from "./scrollRegion";

const TABLE: Region = { role: "group", label: "scrollable table" };

/** The slice of HTMLElement the marker touches, on a plain object (Vitest
 *  runs in node — no DOM). */
function fake(scrollWidth: number, clientWidth: number, attrs: Record<string, string> = {}) {
  const a = { ...attrs };
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
});

describe("markScrollRegions", () => {
  it("marks every match under a root by its own overflow", () => {
    const wide = fake(900, 500);
    const narrow = fake(500, 500);
    const root = { querySelectorAll: () => [wide, narrow] } as unknown as ParentNode;
    markScrollRegions(root, "table", null);
    expect(wide.attrs).toEqual({ tabindex: "0" });
    expect(narrow.attrs).toEqual({});
  });
});
