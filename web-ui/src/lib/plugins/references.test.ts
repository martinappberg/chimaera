import { describe, expect, it } from "vitest";

import { collectReferences, pluginSource } from "./references";
import type { SurfaceItem } from "./platform";

const items: SurfaceItem[] = [
  {
    plugin: "latex",
    key: "main.tex",
    data: {
      shapes: [{ kind: "label", pattern: "sec:[a-z0-9-]+" }],
      ids: [
        { id: "sec:intro", key: "main.tex#sec:intro", kind: "label", title: "Introduction", span: { path: "main.tex", line: 5, end_line: 9 } },
        { id: "sec:nowhere", key: "k", kind: "label", title: "No place to open" },
        { id: "", key: "k2", kind: "label", title: "no id", view: "document" },
      ],
    },
  },
  {
    plugin: "latex",
    key: "ch/one.tex",
    data: {
      shapes: [{ kind: "label", pattern: "sec:[a-z0-9-]+" }],
      ids: [{ id: "sec:intro", key: "ch/one.tex#sec:intro", kind: "label", title: "Intro again", view: "document" }],
    },
  },
  { plugin: "empty", key: "k", data: { shapes: [{ kind: "x", pattern: "X-\\d+" }], ids: [] } },
  { plugin: "broken", key: "k", data: null },
];

describe("plugins' published ids", () => {
  it("merge per plugin, drop ids with nowhere to go and plugins with nothing", () => {
    const refs = collectReferences(items);
    expect([...refs.keys()]).toEqual(["latex"]);
    const latex = refs.get("latex")!;
    expect(latex.shapes).toEqual([{ kind: "label", pattern: "sec:[a-z0-9-]+" }]);
    expect(latex.byId.get("sec:intro")?.map((r) => r.key)).toEqual(["main.tex#sec:intro", "ch/one.tex#sec:intro"]);
    expect(latex.byId.has("sec:nowhere")).toBe(false);
  });

  it("resolve to targets that preview their span under the workspace root", () => {
    const src = pluginSource(collectReferences(items).get("latex")!, "LaTeX", "/ws");
    expect(src.id).toBe("plugin:latex");
    const [first, second] = src.lookup("sec:intro", "label");
    expect(first.span).toEqual({ path: "main.tex", line: 5, end_line: 9 });
    expect(first.base).toBe("/ws");
    expect(first.note).toBe("sec:intro · label · LaTeX");
    expect(second.span).toBeUndefined();
    expect(src.lookup("sec:other", "label")).toEqual([]);
  });
});
