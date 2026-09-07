import { describe, expect, it } from "vitest";

import { markScrollRegion } from "./scrollRegion";

/** The slice of HTMLElement the marker touches, on a plain object (Vitest
 *  runs in node — no DOM). */
function fake(scrollWidth: number, clientWidth: number, attrs: Record<string, string> = {}) {
  const a = { ...attrs };
  return {
    scrollWidth,
    clientWidth,
    scrollHeight: 0,
    clientHeight: 0,
    dataset: {} as Record<string, string | undefined>,
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

describe("markScrollRegion", () => {
  it("marks an overflowing scroller as a labelled focusable region", () => {
    const el = fake(900, 500);
    markScrollRegion(el as unknown as HTMLElement, "x", "scrollable table");
    expect(el.attrs).toEqual({ tabindex: "0", role: "region", "aria-label": "scrollable table" });
    expect(el.dataset.scrollRegion).toBe("1");
  });

  it("leaves a scroller that fits alone, and clears only its own mark", () => {
    const fits = fake(500, 500);
    markScrollRegion(fits as unknown as HTMLElement, "x", "scrollable table");
    expect(fits.attrs).toEqual({});

    const was = fake(900, 500);
    markScrollRegion(was as unknown as HTMLElement, "x", "scrollable table");
    was.scrollWidth = 500; // the pane grew: no overflow any more
    markScrollRegion(was as unknown as HTMLElement, "x", "scrollable table");
    expect(was.attrs).toEqual({});
    expect(was.dataset.scrollRegion).toBeUndefined();

    // A role the content brought along (sanitized agent HTML) is not ours to remove.
    const foreign = fake(500, 500, { role: "note", tabindex: "-1" });
    markScrollRegion(foreign as unknown as HTMLElement, "x", "scrollable table");
    expect(foreign.attrs).toEqual({ role: "note", tabindex: "-1" });
  });
});
