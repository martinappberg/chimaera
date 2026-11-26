import { describe, expect, it } from "vitest";
import src from "./Pane.svelte?raw";

// A pane view's page is drawn by the shell above the DOM: a drop wash that
// isn't `data-overlay` renders behind a browser or HTML page (layout map,
// "A drop wash is an overlay").
describe("Pane drop washes", () => {
  it("every drop wash is marked as an overlay", () => {
    const washes = [...src.matchAll(/<div\b[^>]*\bclass="drop[\s"-][^>]*>/g)].map((m) => m[0]);
    expect(washes.length).toBeGreaterThanOrEqual(6);
    for (const tag of washes) expect(tag, tag).toMatch(/\bdata-overlay\b/);
  });
});
