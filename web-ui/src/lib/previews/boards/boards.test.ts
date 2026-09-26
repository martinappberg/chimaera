import { describe, expect, it } from "vitest";
import { arrowPath, canvasColor, edgeGeometry, facingSides, linkLabel, parseCanvas } from "./canvas";
import { boardFormat, boardStem, fitView, safeColor, unionRects, zoomAround } from "./format";
import { decodeEntities, findAll, parseXml } from "./xml";
import {
  autoPorts,
  connectPorts,
  constraintPort,
  edgeLabelPoint,
  inflateDiagram,
  parseModel,
  perimeterPoint,
  pointAlong,
  readDrawio,
  resolveStyle,
  routeEdge,
  routePath,
  simplify,
  type Pt,
} from "./drawio";
import { arrowheadPoints, baselineOffset, cornerRadius, elementBounds, parseExcalidraw } from "./excalidraw";

/** draw.io's compression, the other way: URI-encode, raw deflate, base64. */
async function compress(xml: string): Promise<string> {
  const input = new Blob([new TextEncoder().encode(encodeURIComponent(xml))]).stream();
  const bytes = new Uint8Array(await new Response(input.pipeThrough(new CompressionStream("deflate-raw"))).arrayBuffer());
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
}

describe("board formats", () => {
  it("routes by extension, .excalidraw.json by name", () => {
    expect(boardFormat("/w/a.canvas")).toBe("canvas");
    expect(boardFormat("/w/Sketch.Excalidraw")).toBe("excalidraw");
    expect(boardFormat("/w/s.excalidraw.json")).toBe("excalidraw");
    expect(boardFormat("/w/f.drawio")).toBe("drawio");
    expect(boardFormat("/w/f.dio")).toBe("drawio");
    expect(boardFormat("/w/f.drawio.svg")).toBeNull();
    expect(boardFormat("/w/data.json")).toBeNull();
    expect(boardStem("/w/s.excalidraw.json")).toBe("s");
  });

  it("fits a board into the view, never enlarging past 1:1", () => {
    const small = fitView({ x: 100, y: 50, w: 200, h: 100 }, 1000, 800, 24, 1);
    expect(small.scale).toBe(1);
    // Centered: the board's center lands on the view's center.
    expect(small.tx + 200 * small.scale).toBeCloseTo(500);
    expect(small.ty + 100 * small.scale).toBeCloseTo(400);
    const big = fitView({ x: 0, y: 0, w: 4000, h: 1000 }, 1000, 800, 20, 1);
    expect(big.scale).toBeCloseTo(960 / 4000);
  });

  it("zooms around the pointer", () => {
    const v = zoomAround({ scale: 1, tx: 0, ty: 0 }, 2, 100, 50);
    // The world point under the pointer (100, 50) stays under it.
    expect(v.scale).toBe(2);
    expect((100 - v.tx) / v.scale).toBeCloseTo(100);
    expect((50 - v.ty) / v.scale).toBeCloseTo(50);
    expect(zoomAround({ scale: 7, tx: 0, ty: 0 }, 4, 0, 0).scale).toBe(8);
  });

  it("only passes colors that can't reference anything", () => {
    expect(safeColor("#1e1e1e")).toBe("#1e1e1e");
    expect(safeColor("rgb(1, 2, 3)")).toBe("rgb(1, 2, 3)");
    expect(safeColor("Transparent")).toBe("transparent");
    expect(safeColor("url(https://evil/x)")).toBeNull();
    expect(safeColor("red;background:url(x)")).toBeNull();
    expect(safeColor("var(--x)")).toBeNull();
  });

  it("unions rects with padding and skips non-finite ones", () => {
    expect(unionRects([{ x: 0, y: 0, w: 10, h: 10 }, { x: 20, y: -5, w: 5, h: 5 }], 2)).toEqual({ x: -2, y: -7, w: 29, h: 19 });
    expect(unionRects([{ x: NaN, y: 0, w: 1, h: 1 }])).toBeNull();
  });
});

describe("JSON Canvas", () => {
  const doc = parseCanvas(
    JSON.stringify({
      nodes: [
        { id: "a", type: "text", x: 0, y: 0, width: 100, height: 50, text: "# hi", color: "1" },
        { id: "b", type: "file", x: 300, y: 0, width: 100, height: 50, file: "notes/x.md", subpath: "#h" },
        { id: "c", type: "link", x: 0, y: 200, width: 100, height: 50, url: "https://example.org/a?b" },
        { id: "g", type: "group", x: -20, y: -20, width: 500, height: 300, label: "G", color: "#00ff00" },
        { id: "bad", type: "text", x: 0, y: 0, width: -1, height: 5 },
        { id: "a", type: "text", x: 0, y: 0, width: 5, height: 5 },
        { id: "weird", type: "video", x: 0, y: 0, width: 5, height: 5 },
      ],
      edges: [
        { id: "e1", fromNode: "a", toNode: "b" },
        { id: "e2", fromNode: "a", fromSide: "bottom", toNode: "c", toSide: "top", toEnd: "none", fromEnd: "arrow", color: "4" },
        { id: "e3", fromNode: "a", toNode: "missing" },
      ],
    }),
  );

  it("keeps valid nodes and edges, counting what it drops", () => {
    expect(doc.nodes.map((n) => n.id)).toEqual(["a", "b", "c", "g"]);
    expect(doc.edges.map((e) => e.id)).toEqual(["e1", "e2"]);
    expect(doc.dropped).toBe(4);
    expect(doc.edges[0]).toMatchObject({ fromEnd: "none", toEnd: "arrow", fromSide: null });
    expect(doc.edges[1]).toMatchObject({ fromEnd: "arrow", toEnd: "none", color: "var(--syn-string)" });
  });

  it("maps presets to theme tokens and keeps only hex otherwise", () => {
    expect(canvasColor("1")).toBe("var(--err)");
    expect(canvasColor("6")).toBe("var(--rate)");
    expect(canvasColor("#AbCdEf")).toBe("#AbCdEf");
    expect(canvasColor("red")).toBeNull();
    expect(canvasColor("url(x)")).toBeNull();
  });

  it("makes room above a top group's label", () => {
    expect(doc.bounds).toEqual({ x: -20, y: -48, w: 500, h: 328 });
  });

  it("rejects what isn't a canvas", () => {
    expect(() => parseCanvas("{")).toThrow(/JSON/);
    expect(() => parseCanvas("[]")).toThrow(/Canvas/);
    expect(parseCanvas("{}").nodes).toEqual([]);
  });

  it("picks facing sides by the gap between nodes", () => {
    const a = { x: 0, y: 0, w: 100, h: 50 };
    expect(facingSides(a, { x: 300, y: 10, w: 100, h: 50 })).toEqual(["right", "left"]);
    expect(facingSides(a, { x: -300, y: 10, w: 100, h: 50 })).toEqual(["left", "right"]);
    expect(facingSides(a, { x: 10, y: 200, w: 100, h: 50 })).toEqual(["bottom", "top"]);
    // Wide nodes side by side still connect left-right.
    expect(facingSides({ x: 0, y: 0, w: 400, h: 40 }, { x: 420, y: 60, w: 400, h: 40 })).toEqual(["right", "left"]);
  });

  it("draws a cubic leaving and entering along the sides' normals", () => {
    const g = edgeGeometry({ x: 0, y: 0, w: 100, h: 50 }, "right", { x: 300, y: 0, w: 100, h: 50 }, "left");
    expect(g.start).toEqual({ x: 100, y: 25 });
    expect(g.end).toEqual({ x: 300, y: 25 });
    expect(g.d).toBe("M100 25C180 25 220 25 300 25");
    expect(g.mid).toEqual({ x: 200, y: 25 });
    // The arrow at the end points right (into the target), the start's left.
    expect(g.endAngle).toBeCloseTo(0);
    expect(Math.abs(g.startAngle)).toBeCloseTo(Math.PI);
    const down = edgeGeometry({ x: 0, y: 0, w: 100, h: 50 }, "bottom", { x: 0, y: 300, w: 100, h: 50 }, "top");
    expect(down.endAngle).toBeCloseTo(Math.PI / 2);
  });

  it("puts an arrowhead's tip on the end point", () => {
    expect(arrowPath({ x: 10, y: 0 }, 0, 10, 8)).toBe("M10 0L0 4L0 -4Z");
  });

  it("labels links by host without parsing markup", () => {
    expect(linkLabel("https://example.org/a?b")).toEqual({ host: "example.org", rest: "/a?b", web: true });
    expect(linkLabel("javascript:alert(1)").web).toBe(false);
    expect(linkLabel("not a url").web).toBe(false);
  });
});

describe("the XML reader", () => {
  it("reads elements, attributes and entities, skipping the rest", () => {
    const doc = parseXml(
      `<?xml version="1.0"?><!-- c --><!DOCTYPE x><a k="1 &lt; 2" q='x"y'><b/><c>t&amp;u<![CDATA[<raw>]]></c></a>`,
    );
    const a = doc.children[0];
    expect(a.name).toBe("a");
    expect(a.attrs).toEqual({ k: "1 < 2", q: 'x"y' });
    expect(a.children.map((c) => c.name)).toEqual(["b", "c"]);
    expect(a.children[1].text).toBe("t&u<raw>");
    expect(decodeEntities("&#x41;&#66;&bogus;")).toBe("AB&bogus;");
  });

  it("tolerates a `>` inside a quoted attribute", () => {
    const doc = parseXml(`<m v="a > b"><n/></m>`);
    expect(doc.children[0].attrs.v).toBe("a > b");
    expect(findAll(doc, "n").length).toBe(1);
  });
});

describe("draw.io files", () => {
  const model = `<mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
    <mxCell id="a" value="A &amp; B" style="rounded=1;whiteSpace=wrap;html=1;" vertex="1" parent="1"><mxGeometry x="10" y="20" width="100" height="40" as="geometry"/></mxCell>
    <mxCell id="lane" value="Lane" style="swimlane;" vertex="1" parent="1"><mxGeometry x="200" y="0" width="200" height="200" as="geometry"/></mxCell>
    <mxCell id="in" value="in" style="ellipse;" vertex="1" parent="lane"><mxGeometry x="20" y="50" width="60" height="40" as="geometry"/></mxCell>
    <mxCell id="e" style="edgeStyle=orthogonalEdgeStyle;" edge="1" parent="1" source="a" target="in"><mxGeometry relative="1" as="geometry"><Array as="points"><mxPoint x="150" y="100"/></Array></mxGeometry></mxCell>
    <UserObject label="wrapped" link="https://x" id="u"><mxCell style="text;" vertex="1" parent="1"><mxGeometry x="0" y="300" width="80" height="20" as="geometry"/></mxCell></UserObject>
  </root></mxGraphModel>`;

  it("inflates draw.io's compressed page content", async () => {
    const xml = "<mxGraphModel><root><mxCell id=\"0\"/></root></mxGraphModel>";
    expect(await inflateDiagram(await compress(xml))).toBe(xml);
  });

  it("reads compressed and plain pages, and a bare model", async () => {
    const packed = await compress(model);
    const file = `<mxfile><diagram name="One" id="1">${packed}</diagram><diagram name="Two">${model}</diagram><diagram name="Bad">!!!!</diagram></mxfile>`;
    const pages = await readDrawio(file);
    expect(pages.map((p) => p.name)).toEqual(["One", "Two", "Bad"]);
    expect(pages[0].model?.name).toBe("mxGraphModel");
    expect(pages[1].model?.name).toBe("mxGraphModel");
    expect(pages[2].model).toBeNull();
    expect(pages[2].error).toMatch(/compressed/);
    expect((await readDrawio(model)).length).toBe(1);
    await expect(readDrawio("<svg/>")).rejects.toThrow(/draw\.io/);
  });

  it("parses cells into a tree with resolved styles", () => {
    const pageModel = parseXml(model).children[0];
    const m = parseModel(pageModel);
    const a = m.cells.get("a")!;
    expect(a.value).toBe("A & B");
    expect(a.style).toMatchObject({ rounded: "1", html: "1", shape: "rectangle", fillColor: "default" });
    expect(m.cells.get("in")!.style.shape).toBe("ellipse");
    expect(m.cells.get("lane")!.children.map((c) => c.id)).toEqual(["in"]);
    expect(m.cells.get("u")!.value).toBe("wrapped");
    expect(m.cells.get("u")!.style).toMatchObject({ fillColor: "none", strokeColor: "none" });
    const e = m.cells.get("e")!;
    expect(e.edge).toBe(true);
    expect(e.style.endArrow).toBe("classic");
    expect(e.geo?.points).toEqual([{ x: 150, y: 100 }]);
    expect(m.roots.map((c) => c.id)).toEqual(["0"]);
  });

  it("applies named styles before a cell's own keys", () => {
    expect(resolveStyle("ellipse;shape=cloud;", false).shape).toBe("cloud");
    expect(resolveStyle("text;align=center", false)).toMatchObject({ align: "center", verticalAlign: "top", fillColor: "none" });
    expect(resolveStyle("", true)).toMatchObject({ endArrow: "classic", shape: "connector" });
  });

  it("finds perimeter points", () => {
    const r = { x: 0, y: 0, w: 100, h: 50 };
    expect(perimeterPoint(r, "rectanglePerimeter", { x: 500, y: 25 })).toEqual({ x: 100, y: 25 });
    const e = perimeterPoint(r, "ellipsePerimeter", { x: 50, y: 500 });
    expect(e.x).toBeCloseTo(50);
    expect(e.y).toBeCloseTo(50);
    const d = perimeterPoint({ x: 0, y: 0, w: 100, h: 100 }, "rhombusPerimeter", { x: 100, y: 100 });
    expect(d.x).toBeCloseTo(75);
    expect(d.y).toBeCloseTo(75);
  });

  it("routes a straight orthogonal run where boxes overlap on an axis", () => {
    const s = { x: 0, y: 0, w: 100, h: 60 };
    const t = { x: 300, y: 20, w: 100, h: 60 };
    const [a, b] = autoPorts(s, t);
    expect(a).toEqual({ p: { x: 100, y: 40 }, dir: "E" });
    expect(b).toEqual({ p: { x: 300, y: 40 }, dir: "W" });
    expect(connectPorts(a, b)).toEqual([
      { x: 100, y: 40 },
      { x: 300, y: 40 },
    ]);
  });

  it("routes an L between diagonal boxes, every leg axis-aligned", () => {
    const route = routeEdge(
      { rect: { x: 0, y: 0, w: 100, h: 50 }, perimeter: "rectanglePerimeter" },
      { rect: { x: 400, y: 200, w: 100, h: 50 }, perimeter: "rectanglePerimeter" },
      null,
      null,
      [],
      { edgeStyle: "orthogonalEdgeStyle" },
    );
    expect(route).toEqual([
      { x: 100, y: 25 },
      { x: 450, y: 25 },
      { x: 450, y: 200 },
    ]);
  });

  it("honors fixed exit and entry points", () => {
    const route = routeEdge(
      { rect: { x: 0, y: 0, w: 100, h: 50 }, perimeter: "rectanglePerimeter" },
      { rect: { x: 0, y: 200, w: 100, h: 50 }, perimeter: "rectanglePerimeter" },
      null,
      null,
      [],
      { edgeStyle: "orthogonalEdgeStyle", exitX: "0.5", exitY: "1", entryX: "0.5", entryY: "0" },
    );
    expect(route).toEqual([
      { x: 50, y: 50 },
      { x: 50, y: 200 },
    ]);
    expect(constraintPort({ x: 0, y: 0, w: 10, h: 10 }, 1, 0.3).dir).toBe("E");
  });

  it("goes orthogonally through waypoints", () => {
    const route = routeEdge(
      { rect: { x: 0, y: 0, w: 100, h: 50 }, perimeter: "rectanglePerimeter" },
      { rect: { x: 300, y: 300, w: 100, h: 50 }, perimeter: "rectanglePerimeter" },
      null,
      null,
      [{ x: 200, y: 25 }],
      { edgeStyle: "orthogonalEdgeStyle" },
    );
    for (let i = 1; i < route.length; i++) {
      const [p, q] = [route[i - 1], route[i]];
      expect(p.x === q.x || p.y === q.y).toBe(true);
    }
    expect(route[0]).toEqual({ x: 100, y: 25 });
    expect(route[route.length - 1].y).toBe(300);
  });

  it("draws a straight edge between perimeters, and a loop on one box", () => {
    const box = { rect: { x: 0, y: 0, w: 100, h: 100 }, perimeter: "ellipsePerimeter" };
    const route = routeEdge(box, { rect: { x: 300, y: 0, w: 100, h: 100 }, perimeter: "rectanglePerimeter" }, null, null, [], {});
    expect(route[0].x).toBeCloseTo(100);
    expect(route[1]).toEqual({ x: 300, y: 50 });
    expect(routeEdge(box, box, null, null, [], {}).length).toBe(4);
    // A dangling edge uses its stored points.
    expect(routeEdge(null, null, { x: 1, y: 2 }, { x: 3, y: 4 }, [], {})).toEqual([
      { x: 1, y: 2 },
      { x: 3, y: 4 },
    ]);
  });

  it("simplifies duplicate and collinear points", () => {
    const pts: Pt[] = [
      { x: 0, y: 0 },
      { x: 0, y: 0 },
      { x: 5, y: 0 },
      { x: 10, y: 0 },
      { x: 10, y: 10 },
    ];
    expect(simplify(pts)).toEqual([
      { x: 0, y: 0 },
      { x: 10, y: 0 },
      { x: 10, y: 10 },
    ]);
  });

  it("places labels along the route", () => {
    const route = [
      { x: 0, y: 0 },
      { x: 100, y: 0 },
      { x: 100, y: 100 },
    ];
    expect(pointAlong(route, 0.5).p).toEqual({ x: 100, y: 0 });
    expect(edgeLabelPoint(route, null)).toEqual({ x: 100, y: 0 });
    const geo = { x: -1, y: 10, w: 0, h: 0, relative: true, points: [], sourcePoint: null, targetPoint: null, offset: { x: 1, y: 1 } };
    expect(edgeLabelPoint(route, geo)).toEqual({ x: 1, y: 11 });
  });

  it("rounds corners and curves through points", () => {
    const pts = [
      { x: 0, y: 0 },
      { x: 100, y: 0 },
      { x: 100, y: 100 },
    ];
    expect(routePath(pts, false, false)).toBe("M0 0L100 0L100 100");
    expect(routePath(pts, true, false)).toBe("M0 0L90 0Q100 0 100 10L100 100");
    expect(routePath(pts, false, true)).toBe("M0 0Q100 0 100 100");
  });
});

describe("Excalidraw geometry", () => {
  it("parses a scene, skipping deleted and unknown elements", () => {
    const scene = parseExcalidraw(
      JSON.stringify({
        type: "excalidraw",
        elements: [
          { id: "r", type: "rectangle", x: 0, y: 0, width: 100, height: 50, strokeColor: "#000", backgroundColor: "url(x)" },
          { id: "d", type: "ellipse", x: 0, y: 0, width: 10, height: 10, isDeleted: true },
          { id: "?", type: "laser", x: 0, y: 0, width: 10, height: 10 },
          { id: "a", type: "arrow", x: 10, y: 10, width: 50, height: 0, points: [[0, 0], [50, 0]] },
        ],
        appState: { viewBackgroundColor: "#fafafa" },
        files: { ok: { dataURL: "data:image/png;base64,iVBORw0KGgo=" }, bad: { dataURL: "https://evil/x.png" } },
      }),
    );
    expect(scene.elements.map((e) => e.id)).toEqual(["r", "a"]);
    expect(scene.skipped).toBe(1);
    expect(scene.elements[0].backgroundColor).toBe("transparent");
    expect(scene.elements[1].endArrowhead).toBe("arrow");
    expect([...scene.files.keys()]).toEqual(["ok"]);
    expect(scene.background).toBe("#fafafa");
    expect(() => parseExcalidraw("{}")).toThrow(/Excalidraw/);
  });

  it("bounds a rotated element by its corners", () => {
    const [el] = parseExcalidraw(
      JSON.stringify({ elements: [{ id: "r", type: "rectangle", x: 0, y: 0, width: 100, height: 100, angle: Math.PI / 4, strokeWidth: 0.1 }] }),
    ).elements;
    const b = elementBounds(el);
    const half = 50 * Math.SQRT2;
    expect(b.x).toBeCloseTo(50 - half - 0.2);
    expect(b.w).toBeCloseTo(half * 2 + 0.4);
  });

  it("rounds corners the way Excalidraw does", () => {
    expect(cornerRadius(100, null)).toBe(0);
    expect(cornerRadius(100, { type: 2 })).toBe(25);
    expect(cornerRadius(100, { type: 3 })).toBe(25);
    expect(cornerRadius(400, { type: 3 })).toBe(32);
  });

  it("places arrowheads on the end segment, scaled to short segments", () => {
    const head = arrowheadPoints(
      [
        [0, 0],
        [100, 0],
      ],
      "end",
      "arrow",
      2,
    )!;
    expect(head[0]).toBe(100);
    expect(head[1]).toBe(0);
    // The wings sit 25 back (the arrow's size), 20° either side.
    expect(head[2]).toBeCloseTo(100 - 25 * Math.cos((20 * Math.PI) / 180));
    expect(Math.abs(head[3])).toBeCloseTo(25 * Math.sin((20 * Math.PI) / 180));
    expect(head[5]).toBeCloseTo(-head[3]);
    const short = arrowheadPoints(
      [
        [0, 0],
        [10, 0],
      ],
      "start",
      "triangle",
      2,
    )!;
    expect(short[0]).toBe(0);
    // At most half the segment long.
    expect(Math.hypot(short[2] - short[0], short[3] - short[1])).toBeCloseTo(5);
    expect(arrowheadPoints([[0, 0]], "end", "arrow", 2)).toBeNull();
    expect(arrowheadPoints([[0, 0], [10, 0]], "end", "dot", 2)).toHaveLength(3);
  });

  it("puts the first baseline where Excalidraw's font metrics do", () => {
    // Virgil 20px at line height 1.25: ascent 17.72, half the leftover gap.
    expect(baselineOffset(1, 20, 25)).toBeCloseTo(17.72 + (25 - 17.72 - 7.48) / 2);
  });
});
