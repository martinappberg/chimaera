import { describe, expect, it } from "vitest";
import { buildLayerTree, selectionBranch } from "./boardLayers";
import type { ChildFrame, ObjInfo } from "./boardInteract";

/** A parsed top-level object; `raw` carries whatever the layer tree reads
 *  (a group's nested `objects`). */
function obj(id: string, kind: string, raw: Record<string, unknown> = {}): ObjInfo {
  return {
    id,
    kind,
    at: null,
    size: null,
    text: null,
    raw: { id, type: kind, ...raw },
    sig: "",
  };
}

const frame = (id: string): ChildFrame => ({ id, frame: [0, 0, 10, 10] });

describe("buildLayerTree", () => {
  it("nests a group's own child objects, all selecting the group", () => {
    const objects = [
      obj("g", "group", {
        objects: [
          { id: "g/a", type: "text" },
          { id: "g/b", type: "shape" },
          { id: "g/c", type: "chart" },
        ],
      }),
    ];
    const [g] = buildLayerTree(objects, {});
    expect(g.select).toEqual({ via: "object", id: "g" });
    expect(g.children.map((c) => c.id)).toEqual(["g/a", "g/b", "g/c"]);
    expect(g.children.map((c) => c.kind)).toEqual(["text", "shape", "chart"]);
    // Every descendant selects the enclosing top-level group (the moved unit).
    for (const c of g.children) expect(c.select).toEqual({ via: "object", id: "g" });
  });

  it("nests a composite's derived children via onselectchild, label stripped", () => {
    const objects = [obj("flow", "diagram")];
    const childFrames = { flow: [frame("flow/hot"), frame("flow/cold")] };
    const [d] = buildLayerTree(objects, childFrames);
    expect(d.children.map((c) => c.label)).toEqual(["hot", "cold"]);
    expect(d.children[0].select).toEqual({ via: "child", parent: "flow", id: "flow/hot" });
    expect(d.children[0].kind).toBe("");
  });

  it("recurses nested groups but still points every level at the outer group", () => {
    const objects = [
      obj("g", "group", {
        objects: [
          { id: "g/inner", type: "group", objects: [{ id: "g/inner/x", type: "text" }] },
        ],
      }),
    ];
    const [g] = buildLayerTree(objects, {});
    const inner = g.children[0];
    expect(inner.children[0].id).toBe("g/inner/x");
    expect(inner.children[0].select).toEqual({ via: "object", id: "g" });
  });

  it("a plain object with no children is a flat leaf", () => {
    const [n] = buildLayerTree([obj("t1", "text")], {});
    expect(n.children).toEqual([]);
    expect(n.select).toEqual({ via: "object", id: "t1" });
  });

  it("bounds a pathological node count", () => {
    const kids = Array.from({ length: 2000 }, (_, i) => ({ id: `g/${i}`, type: "text" }));
    const [g] = buildLayerTree([obj("g", "group", { objects: kids })], {});
    expect(g.children.length).toBeLessThan(2000);
  });
});

describe("selectionBranch", () => {
  const roots = buildLayerTree(
    [
      obj("g", "group", { objects: [{ id: "g/a", type: "text" }] }),
      obj("flow", "diagram"),
    ],
    { flow: [frame("flow/hot")] },
  );

  it("opens a selected group so its contents show", () => {
    expect(selectionBranch(roots, "g", null).has("g")).toBe(true);
  });

  it("opens a composite whose derived child is drilled into", () => {
    const open = selectionBranch(roots, "flow", "flow/hot");
    expect(open.has("flow")).toBe(true);
    expect(open.has("flow/hot")).toBe(true);
  });

  it("opens nothing when nothing is selected", () => {
    expect(selectionBranch(roots, null, null).size).toBe(0);
  });
});
