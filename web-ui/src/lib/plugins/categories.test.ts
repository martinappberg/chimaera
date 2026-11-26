import { describe, expect, it } from "vitest";
import { CATEGORIES, groupByCategory } from "./categories";

const p = (id: string, category?: string | null) => ({ id, category });

describe("the Extensions page's groups", () => {
  it("lists plugins under their categories, in the page's order, Other last", () => {
    const groups = groupByCategory([p("chromium", "agents"), p("mine"), p("typst", "writing"), p("pptx", "presentations"), p("latex", "writing"), p("new", "games")]);
    expect(groups.map((g) => [g.label, g.plugins.map((x) => x.id)])).toEqual([
      ["Writing", ["typst", "latex"]],
      ["Presentations", ["pptx"]],
      ["Agent tools", ["chromium"]],
      ["Other", ["mine", "new"]],
    ]);
  });
  it("draws a plain list when nothing is categorised", () => {
    expect(groupByCategory([p("a"), p("b", null)])).toEqual([{ id: "other", label: "", plugins: [p("a"), p("b", null)] }]);
    expect(groupByCategory([])).toEqual([]);
  });
  it("never takes a category from an object key", () => {
    expect(groupByCategory([p("x", "constructor")])[0]?.id).toBe("other");
  });
  it("names every category once", () => {
    expect(new Set(CATEGORIES.map((c) => c.id)).size).toBe(CATEGORIES.length);
  });
});
