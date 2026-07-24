import { describe, expect, it } from "vitest";
import {
  applyFieldSet,
  attributeDiff,
  chartConfig,
  childFrameRect,
  composeBoardContext,
  configSig,
  coversPoint,
  diagramNodeIndex,
  diagramNodeLabel,
  editableText,
  editorFontPx,
  editorTextToNodeLabel,
  editorTextToParagraphs,
  fateCensus,
  findMember,
  gridLines,
  hitChild,
  hitMember,
  MARK_SWAP_KINDS,
  nextPinId,
  paragraphsToEditorText,
  parseBoard,
  pinAnchor,
  sameParagraphs,
  snapDrag,
  snapshotRegion,
  SORT_OPTIONS,
  unresolvedPins,
  SNAPSHOT_PAD_PT,
  type ChildFrame,
  type ExpectedChange,
  type Frame,
  type ObjInfo,
  type ObjSnap,
  type PinInfo,
} from "./boardInteract";
import type { BoardJournalEvent } from "./files";

/** An ObjInfo the way parseBoard builds one, from the object's raw JSON. */
function obj(raw: Record<string, unknown>): ObjInfo {
  return {
    id: String(raw.id ?? ""),
    kind: String(raw.type ?? "?"),
    at: Array.isArray(raw.at) ? [raw.at[0] as number, raw.at[1] as number] : null,
    size: Array.isArray(raw.size) ? [raw.size[0] as number, raw.size[1] as number] : null,
    text: editableText(raw.type as string | undefined, raw.text),
    raw,
    sig: configSig(raw),
    children: [],
    envelope: false,
  };
}

describe("editableText", () => {
  it("projects plain paragraphs for the text-op kinds only", () => {
    expect(editableText("text", ["a", "b"])).toEqual(["a", "b"]);
    expect(editableText("shape", ["label"])).toEqual(["label"]);
    // The daemon's text op bails for every other kind — no editor there.
    expect(editableText("callout", ["hi"])).toBeNull();
    expect(editableText("image", undefined)).toBeNull();
    expect(editableText(undefined, ["x"])).toBeNull();
  });

  it("flattens rich runs the same way the edit op would", () => {
    expect(
      editableText("text", [{ runs: [{ t: "bold" }, { t: " tail" }] }, "plain"]),
    ).toEqual(["bold tail", "plain"]);
  });

  it("treats a missing/malformed text field as an empty editor, not ineligible", () => {
    expect(editableText("shape", undefined)).toEqual([]);
    expect(editableText("text", "not-an-array")).toEqual([]);
    expect(editableText("text", [42, { runs: "nope" }, null])).toEqual(["", "", ""]);
  });
});

describe("editor text round trip (paragraphs = newlines)", () => {
  it("round-trips plain paragraphs exactly", () => {
    const paras = ["Results", "", "p < 0.01 across both cohorts"];
    expect(editorTextToParagraphs(paragraphsToEditorText(paras))).toEqual(paras);
  });

  it("normalizes pasted CR/CRLF line endings", () => {
    expect(editorTextToParagraphs("a\r\nb\rc")).toEqual(["a", "b", "c"]);
  });

  it("maps a fully empty editor to no paragraphs (clearing writes [])", () => {
    expect(editorTextToParagraphs("")).toEqual([]);
    expect(paragraphsToEditorText([])).toBe("");
  });

  it("gates the no-change no-op commit", () => {
    expect(sameParagraphs(["a", "b"], ["a", "b"])).toBe(true);
    expect(sameParagraphs(["a"], ["a", ""])).toBe(false);
    expect(sameParagraphs(["a"], ["b"])).toBe(false);
    expect(sameParagraphs([], [])).toBe(true);
  });
});

describe("editorFontPx", () => {
  it("scales with the box height per seeded line and the stage scale", () => {
    const one = editorFontPx([200, 40], 1, 1);
    const two = editorFontPx([200, 40], 1, 2);
    expect(one).toBeGreaterThan(two);
    expect(editorFontPx([200, 40], 2, 1)).toBeGreaterThan(one);
  });

  it("clamps to a sane band", () => {
    expect(editorFontPx([100, 800], 1, 1)).toBe(44); // pt ceiling
    expect(editorFontPx([100, 8], 1, 4)).toBe(11); // px floor
  });
});

describe("composeBoardContext", () => {
  it("matches the §6.4 shape with a trailing space and no newline", () => {
    const line = composeBoardContext("figures/fig2.board.json", "results", [
      "callout",
      "arrow-1",
    ]);
    expect(line).toBe("[board: figures/fig2.board.json › results › callout, arrow-1] ");
    expect(/[\r\n]/.test(line)).toBe(false);
  });

  it("keeps a single id unadorned", () => {
    expect(composeBoardContext("deck.board.json", "page-1", ["title"])).toBe(
      "[board: deck.board.json › page-1 › title] ",
    );
  });

  it("carries a derived child id verbatim — the deixis the daemon's describe speaks", () => {
    expect(composeBoardContext("coffee.board.json", "p1", ["flow/too-hot"])).toBe(
      "[board: coffee.board.json › p1 › flow/too-hot] ",
    );
  });
});

describe("composite children (childFrames hit-testing + node mapping)", () => {
  const kids: ChildFrame[] = [
    { id: "flow/lane.prep", frame: [40, 40, 300, 200] },
    { id: "flow/lane.prep.label", frame: [52, 44, 100, 16] },
    { id: "flow/pour", frame: [80, 80, 120, 40] },
    { id: "flow/too-hot", frame: [80, 160, 120, 40] },
  ];

  it("hitChild picks the topmost child (z = array order), like the stage hit-test", () => {
    // Inside both the lane hull and the node — the node wins (drawn later).
    expect(hitChild(kids, [100, 100])?.id).toBe("flow/pour");
    expect(hitChild(kids, [100, 180])?.id).toBe("flow/too-hot");
    // Lane-only territory falls back to the lane rect.
    expect(hitChild(kids, [300, 200])?.id).toBe("flow/lane.prep");
    expect(hitChild(kids, [10, 10])).toBeNull();
  });

  it("childFrameRect projects the wire tuple to the pane's Frame shape", () => {
    expect(childFrameRect({ id: "x", frame: [1, 2, 3, 4] })).toEqual({
      at: [1, 2],
      size: [3, 4],
    });
  });

  const diagram = obj({
    id: "flow",
    type: "diagram",
    at: [40, 40],
    size: [400, 300],
    nodes: [
      { id: "pour", label: "Pour water" },
      { id: "too-hot", label: "Too hot?" },
      // A duplicate declaration: the engine only ever emits the FIRST, so
      // the mapping must resolve to it too.
      { id: "pour", label: "shadowed" },
    ],
    edges: [{ from: "pour", to: "too-hot" }],
  });

  it("maps a derived child id to its node index, first declaration winning", () => {
    expect(diagramNodeIndex(diagram, "flow/pour")).toBe(0);
    expect(diagramNodeIndex(diagram, "flow/too-hot")).toBe(1);
  });

  it("refuses non-node children and foreign ids", () => {
    // Lane rects/labels expand under `<id>/lane.…` and are not draggable.
    expect(diagramNodeIndex(diagram, "flow/lane.prep")).toBeNull();
    expect(diagramNodeIndex(diagram, "other/pour")).toBeNull();
    expect(diagramNodeIndex(diagram, "flow/ghost")).toBeNull();
    // Only diagrams have node entries to anchor a set edit on.
    expect(diagramNodeIndex(obj({ id: "cb", type: "colorbar" }), "cb/slice[0]")).toBeNull();
  });

  it("reads the stored node label as the editor seed", () => {
    expect(diagramNodeLabel(diagram, 1)).toBe("Too hot?");
    expect(diagramNodeLabel(diagram, 9)).toBeNull();
  });

  it("collapses editor newlines into a single-line label", () => {
    expect(editorTextToNodeLabel("Blow and\r\nwait")).toBe("Blow and wait");
    expect(editorTextToNodeLabel("one\n\ntwo")).toBe("one two");
    expect(editorTextToNodeLabel("plain")).toBe("plain");
  });
});

describe("comment pins (journal → unresolved set)", () => {
  const comment = (
    seq: number,
    pin: string,
    extra: Partial<BoardJournalEvent> = {},
  ): BoardJournalEvent => ({
    seq,
    actor: "human",
    event: "comment",
    page: "bench",
    pin,
    text: `note ${pin}`,
    ...extra,
  });
  const resolved = (seq: number, pin: string): BoardJournalEvent => ({
    seq,
    actor: "human",
    event: "comment-resolved",
    pin,
  });
  const move = (seq: number): BoardJournalEvent => ({
    seq,
    actor: "agent",
    event: "move",
    object: "callout",
  });

  it("keeps comments without a matching resolve, in seq order", () => {
    const pins = unresolvedPins([
      comment(1, "c1", { object: "callout" }),
      move(2),
      comment(3, "c2", { at: [320, 96] }),
      resolved(4, "c1"),
    ]);
    expect(pins.map((p) => p.pin)).toEqual(["c2"]);
    expect(pins[0]).toMatchObject({
      pin: "c2",
      seq: 3,
      actor: "human",
      page: "bench",
      object: null,
      at: [320, 96],
      text: "note c2",
    });
  });

  it("is order-aware: a re-used pin id after its resolve is a fresh pin", () => {
    const pins = unresolvedPins([comment(1, "c1"), resolved(2, "c1"), comment(3, "c1")]);
    expect(pins).toHaveLength(1);
    expect(pins[0].seq).toBe(3);
  });

  it("ignores a resolve with no matching comment (its pair compacted away)", () => {
    expect(unresolvedPins([resolved(1, "c9"), comment(2, "c10")])).toHaveLength(1);
  });

  it("mints the next id past every c<n> seen, resolved included", () => {
    expect(nextPinId([])).toBe("c1");
    expect(nextPinId([comment(1, "c1"), resolved(2, "c1"), comment(3, "c4")])).toBe("c5");
    // Foreign id shapes never confuse the counter.
    expect(nextPinId([comment(1, "note-a"), move(2)])).toBe("c1");
  });
});

describe("pinAnchor", () => {
  const objects: ObjInfo[] = [
    obj({ id: "callout", type: "shape", geo: "rect", at: [520, 150], size: [200, 80], text: ["hi"] }),
    obj({ id: "ghost", type: "image", src: "x.png" }),
  ];
  const pin = (object: string | null, at: [number, number] | null): PinInfo => ({
    pin: "c1",
    seq: 1,
    actor: "human",
    page: "bench",
    object,
    at,
    text: "t",
  });

  it("anchors an object-bound pin to the frame's top-right corner (tracks moves)", () => {
    expect(pinAnchor(pin("callout", [1, 1]), objects)).toEqual([720, 150]);
    const moved = objects.map((o) =>
      o.id === "callout" ? { ...o, at: [100, 40] as [number, number] } : o,
    );
    expect(pinAnchor(pin("callout", [1, 1]), moved)).toEqual([300, 40]);
  });

  it("falls back to the stored point when the object is gone or frameless", () => {
    expect(pinAnchor(pin("vanished", [64, 32]), objects)).toEqual([64, 32]);
    expect(pinAnchor(pin("ghost", [8, 8]), objects)).toEqual([8, 8]);
  });

  it("sits a point pin at its stored point, and hides an unanchorable pin", () => {
    expect(pinAnchor(pin(null, [320, 96]), objects)).toEqual([320, 96]);
    expect(pinAnchor(pin("vanished", null), objects)).toBeNull();
    expect(pinAnchor(pin(null, null), objects)).toBeNull();
  });
});

describe("snapshotRegion", () => {
  const canvas: [number, number] = [960, 540];

  it("pads a single frame by the snapshot padding", () => {
    const r = snapshotRegion([{ at: [100, 80], size: [200, 100] }], canvas);
    expect(r).toEqual({
      at: [100 - SNAPSHOT_PAD_PT, 80 - SNAPSHOT_PAD_PT],
      size: [200 + 2 * SNAPSHOT_PAD_PT, 100 + 2 * SNAPSHOT_PAD_PT],
    });
  });

  it("unions multiple frames", () => {
    const r = snapshotRegion(
      [
        { at: [100, 100], size: [50, 50] },
        { at: [300, 200], size: [40, 40] },
      ],
      canvas,
      0,
    );
    expect(r).toEqual({ at: [100, 100], size: [240, 140] });
  });

  it("clamps to the canvas at the edges", () => {
    const r = snapshotRegion([{ at: [0, 0], size: [960, 540] }], canvas);
    expect(r).toEqual({ at: [0, 0], size: [960, 540] });
  });

  it("is null with no frames or a degenerate region", () => {
    expect(snapshotRegion([], canvas)).toBeNull();
    expect(snapshotRegion([{ at: [2000, 2000], size: [10, 10] }], canvas)).toBeNull();
  });
});

// --- the /board/edit set op's client half ------------------------------------

const barChart = (over: Record<string, unknown> = {}): Record<string, unknown> => ({
  id: "bench",
  type: "chart",
  at: [80, 80],
  size: [400, 300],
  data: { origin: "stated-by-user", values: [{ tool: "a", ms: 4 }] },
  x: { field: "tool", type: "nominal" },
  y: { field: "ms", type: "quantitative" },
  marks: [{ mark: "bar" }],
  ...over,
});

describe("applyFieldSet", () => {
  it("sets nested fields, indexes arrays, and creates missing objects", () => {
    const out = applyFieldSet(barChart(), {
      "x.sort": "-y",
      "y.title": "Time (ms)",
      "marks.0.fill": "@cat3",
      "axes.grid": "y",
    }) as Record<string, unknown>;
    expect((out.x as Record<string, unknown>).sort).toBe("-y");
    expect((out.y as Record<string, unknown>).title).toBe("Time (ms)");
    expect((out.marks as Record<string, unknown>[])[0].fill).toBe("@cat3");
    // The missing `axes` intermediate materializes as an object.
    expect((out.axes as Record<string, unknown>).grid).toBe("y");
  });

  it("removes a field on null (the clear spelling)", () => {
    const raw = barChart({ y: { field: "ms", type: "quantitative", title: "Time" } });
    const out = applyFieldSet(raw, { "y.title": null }) as Record<string, unknown>;
    expect("title" in (out.y as Record<string, unknown>)).toBe(false);
  });

  it("never mutates the input and skips unappliable paths", () => {
    const raw = barChart();
    const out = applyFieldSet(raw, { "marks.5.fill": "@cat1", "x.sort": "-y" }) as Record<
      string,
      unknown
    >;
    // Out-of-bounds index: skipped (the daemon rejects that whole request);
    // the rest still applies, and the input is untouched.
    expect((out.marks as unknown[]).length).toBe(1);
    expect((out.x as Record<string, unknown>).sort).toBe("-y");
    expect((raw.x as Record<string, unknown>).sort).toBeUndefined();
  });
});

describe("configSig", () => {
  it("ignores geometry and text, catches config", () => {
    const a = barChart();
    expect(configSig(a)).toBe(configSig({ ...a, at: [0, 0], size: [1, 1] }));
    expect(configSig(a)).not.toBe(configSig(applyFieldSet(a, { "x.sort": "-y" })));
  });

  it("is key-order insensitive (canonical bytes vs client prediction)", () => {
    expect(configSig({ id: "t", type: "chart", b: 1, a: [1, 2] })).toBe(
      configSig({ a: [1, 2], b: 1, type: "chart", id: "t" }),
    );
  });
});

describe("chartConfig", () => {
  it("projects a vertical bar chart: sort on x, swappable mark, fill color", () => {
    const c = chartConfig(obj(barChart({ marks: [{ mark: "bar", fill: "@cat2" }] })));
    expect(c).not.toBeNull();
    expect(c?.x).toEqual({ field: "tool", title: "" });
    expect(c?.y).toEqual({ field: "ms", title: "" });
    expect(c?.sortChannel).toBe("x");
    expect(c?.sort).toBe("");
    expect(c?.markKind).toBe("bar");
    expect(c?.markSwappable).toBe(true);
    expect(c?.markColor).toBe("@cat2");
  });

  it("mirrors chart.rs's orient rule: horizontal charts sort on y", () => {
    const c = chartConfig(
      obj(
        barChart({
          x: { field: "ms", type: "quantitative" },
          y: { field: "tool", type: "nominal", sort: "-y", title: "Tool" },
        }),
      ),
    );
    expect(c?.sortChannel).toBe("y");
    expect(c?.sort).toBe("-y");
    expect(c?.y).toEqual({ field: "tool", title: "Tool" });
  });

  it("defaults undeclared channel types like the engine (x nominal, y quantitative)", () => {
    const c = chartConfig(obj(barChart({ x: { field: "tool" }, y: { field: "ms" } })));
    expect(c?.sortChannel).toBe("x");
  });

  it("leaves box, interval bars, and multi-mark charts alone", () => {
    expect(chartConfig(obj(barChart({ marks: [{ mark: "box" }] })))?.markSwappable).toBe(false);
    expect(
      chartConfig(obj(barChart({ marks: [{ mark: "bar", fields: { y2: "end" } }] })))
        ?.markSwappable,
    ).toBe(false);
    const multi = chartConfig(obj(barChart({ marks: [{ mark: "bar" }, { mark: "rule", y: 3 }] })));
    expect(multi?.markKind).toBeNull();
    expect(multi?.markCount).toBe(2);
  });

  it("reads the stroke token when no fill is stated (series_color's order)", () => {
    const c = chartConfig(obj(barChart({ marks: [{ mark: "line", stroke: "@cat5" }] })));
    expect(c?.markColor).toBe("@cat5");
  });

  it("is null for non-charts", () => {
    expect(chartConfig(obj({ id: "t", type: "text", text: ["hi"] }))).toBeNull();
  });
});

describe("sort options", () => {
  it("offers exactly the values chart.rs's category_order accepts", () => {
    expect(SORT_OPTIONS.map((o) => o.value)).toEqual(["", "x", "-x", "y", "-y"]);
    expect([...MARK_SWAP_KINDS]).toEqual(["bar", "line", "point"]);
  });
});

describe("fateCensus (export preflight)", () => {
  it("counts tiers best-first, present tiers only", () => {
    expect(
      fateCensus([
        { tier: "grouped" },
        { tier: "native" },
        { tier: "grouped" },
        { tier: "raster" },
      ]),
    ).toBe("1 native · 2 grouped · 1 raster");
  });

  it("is empty for no fates", () => {
    expect(fateCensus([])).toBe("");
  });

  it("appends an unknown tier from a newer daemon rather than hiding it", () => {
    expect(fateCensus([{ tier: "native" }, { tier: "holographic" }])).toBe(
      "1 native · 1 holographic",
    );
  });
});

describe("attributeDiff with config fingerprints", () => {
  const snap = (raw: Record<string, unknown>): ObjSnap => ({
    at: raw.at as [number, number],
    size: raw.size as [number, number],
    sig: configSig(raw),
  });

  it("flashes an external restyle even though no frame moved", () => {
    const before = barChart();
    const after = applyFieldSet(before, { "x.sort": "-y" }) as Record<string, unknown>;
    const changed = attributeDiff(
      new Map([["bench", snap(before)]]),
      new Map([["bench", snap(after)]]),
      new Map(),
    );
    expect(changed.map((c) => c.id)).toEqual(["bench"]);
  });

  it("consumes a matching own-set expectation without flashing", () => {
    const before = barChart();
    const after = applyFieldSet(before, { "x.sort": "-y" }) as Record<string, unknown>;
    const expected = new Map<string, ExpectedChange>([["bench", { sig: configSig(after) }]]);
    const changed = attributeDiff(
      new Map([["bench", snap(before)]]),
      new Map([["bench", snap(after)]]),
      expected,
    );
    expect(changed).toEqual([]);
    expect(expected.size).toBe(0);
  });
});

describe("parseBoard grid", () => {
  const bytes = (v: unknown): Uint8Array => new TextEncoder().encode(JSON.stringify(v));

  it("parses a column-only grid", () => {
    const b = parseBoard(bytes({ canvas: { size: [960, 540], grid: { cols: 12 } }, pages: [] }));
    expect(b?.grid).toEqual({ cols: 12, rows: null, margin: 0, gutter: 0 });
  });

  it("carries rows, margin and gutter", () => {
    const b = parseBoard(
      bytes({ canvas: { size: [960, 540], grid: { cols: 6, rows: 4, margin: 24, gutter: 8 } } }),
    );
    expect(b?.grid).toEqual({ cols: 6, rows: 4, margin: 24, gutter: 8 });
  });

  it("drops a structurally broken grid to null (leniency, like the daemon)", () => {
    expect(parseBoard(bytes({ canvas: { size: [960, 540], grid: { cols: "x" } } }))?.grid).toBeNull();
    expect(parseBoard(bytes({ canvas: { size: [960, 540], grid: 7 } }))?.grid).toBeNull();
    // cols < 1 reads as absent (an overlay of one column is noise).
    expect(parseBoard(bytes({ canvas: { size: [960, 540], grid: { cols: 0 } } }))?.grid).toBeNull();
  });

  it("is null when the board has no grid", () => {
    expect(parseBoard(bytes({ canvas: { size: [960, 540] } }))?.grid).toBeNull();
  });
});

describe("parseBoard groups", () => {
  const bytes = (v: unknown): Uint8Array => new TextEncoder().encode(JSON.stringify(v));
  /** The specimen shape an agent writes: a group with NO at/size (both are
   *  skip-if-none in the schema and `parse()` does not normalize), holding
   *  page-absolute members. */
  const boardWith = (objects: unknown[]): ReturnType<typeof parseBoard> =>
    parseBoard(bytes({ canvas: { size: [960, 540] }, pages: [{ id: "p1", objects }] }));

  const stage = {
    id: "s01-genome-stage",
    type: "group",
    objects: [
      { id: "disc", type: "shape", at: [88, 338], size: [72, 72] },
      { id: "icon", type: "icon", at: [106, 356], size: [36, 36] },
      { id: "label", type: "text", at: [56, 420], size: [136, 48] },
    ],
  };

  it("computes a group's envelope as the union of its members", () => {
    const g = boardWith([stage])?.pages[0].objects[0];
    // Exactly normalize.rs's union_frame over the same three frames.
    expect(g?.at).toEqual([56, 338]);
    expect(g?.size).toEqual([136, 130]);
    expect(g?.envelope).toBe(true);
    expect(g?.children.map((c) => c.id)).toEqual(["disc", "icon", "label"]);
  });

  it("unions a nested group's own envelope (the subtree, not one level)", () => {
    const g = boardWith([
      {
        id: "outer",
        type: "group",
        objects: [
          { id: "far", type: "text", at: [400, 400], size: [40, 40] },
          stage,
        ],
      },
    ])?.pages[0].objects[0];
    expect(g?.at).toEqual([56, 338]);
    expect(g?.size).toEqual([384, 130]);
  });

  it("floors an extent at the daemon's minimum, and skips frameless members", () => {
    const g = boardWith([
      {
        id: "g",
        type: "group",
        objects: [
          { id: "line", type: "connector", from: { object: "a" }, to: { object: "b" } },
          { id: "dot", type: "shape", at: [10, 10], size: [0, 0] },
        ],
      },
    ])?.pages[0].objects[0];
    expect(g?.at).toEqual([10, 10]);
    expect(g?.size).toEqual([8, 8]);
  });

  it("leaves a group with nothing positioned unset, like the daemon's warning", () => {
    const g = boardWith([
      { id: "g", type: "group", objects: [{ id: "line", type: "connector" }] },
    ])?.pages[0].objects[0];
    expect(g?.at).toBeNull();
    expect(g?.envelope).toBe(false);
  });

  it("drills to the deepest member under a point, topmost first", () => {
    const g = boardWith([stage])?.pages[0].objects[0];
    expect(hitMember(g!, [120, 370])?.id).toBe("icon");
    // Inside the disc but outside the icon: the disc, not the group.
    expect(hitMember(g!, [92, 342])?.id).toBe("disc");
    // The envelope's own empty space keeps the group selected.
    expect(hitMember(g!, [60, 340])).toBeNull();
  });

  it("covers a group only where a member is, so its empty space falls through", () => {
    const objects = boardWith([stage])?.pages[0].objects ?? [];
    const g = objects[0];
    // Over a member: the group is the hit (the stage's press selects it).
    expect(coversPoint(g, [120, 370])).toBe(true);
    // Inside the envelope [56,338]+[136,130] but between the members: empty
    // space, so a press there belongs to whatever sits underneath.
    expect(coversPoint(g, [60, 340])).toBe(false);
    // Outside the envelope entirely.
    expect(coversPoint(g, [400, 400])).toBe(false);
  });

  it("keeps a neighbour under a group's envelope reachable (the shadowing bug)", () => {
    // The hand-authored shape the field deck is full of: a top-level object
    // that happens to sit inside a big group's envelope without being a member.
    const page = boardWith([
      { id: "caption", type: "text", at: [56, 338], size: [24, 16] },
      stage,
    ])?.pages[0].objects ?? [];
    const topmostAt = (pt: [number, number]): string | null => {
      for (let i = page.length - 1; i >= 0; i--) if (coversPoint(page[i], pt)) return page[i].id;
      return null;
    };
    // The group is drawn LAST (topmost) and its envelope contains the caption,
    // but the caption is what is actually painted there.
    expect(topmostAt([60, 342])).toBe("caption");
    expect(topmostAt([120, 370])).toBe("s01-genome-stage");
  });

  it("treats a group with no parsed members as its own box", () => {
    // No `objects` to union: the stored box is all the group has, so it stays
    // hittable rather than becoming unreachable.
    const g = boardWith([{ id: "g", type: "group", at: [10, 10], size: [40, 40] }])?.pages[0]
      .objects[0];
    expect(coversPoint(g!, [20, 20])).toBe(true);
  });

  it("finds a member anywhere in the subtree by id", () => {
    const g = boardWith([
      { id: "outer", type: "group", objects: [stage] },
    ])?.pages[0].objects[0];
    expect(findMember(g!, "label")?.kind).toBe("text");
    expect(findMember(g!, "nope")).toBeNull();
  });
});

describe("gridLines", () => {
  it("mirrors the engine's 12-column design grid (80pt columns, x only)", () => {
    const { xs, ys } = gridLines([960, 540], { cols: 12, rows: null, margin: 0, gutter: 0 });
    expect(xs).toEqual([0, 80, 160, 240, 320, 400, 480, 560, 640, 720, 800, 880]);
    // A column-only grid snaps x only — no row lines, exactly like snap-grid.
    expect(ys).toEqual([]);
  });

  it("emits row-start lines when the grid has rows", () => {
    const { xs, ys } = gridLines([960, 540], { cols: 12, rows: 6, margin: 0, gutter: 0 });
    expect(xs).toHaveLength(12);
    expect(ys).toHaveLength(6);
    expect(ys[0]).toBe(0);
  });

  it("insets by margin and separates by gutter, 8pt-quantized", () => {
    const { xs } = gridLines([960, 540], { cols: 2, rows: null, margin: 16, gutter: 16 });
    // content 928, cell (928-16)/2=456; starts 16 and 16+456+16=488, both 8pt.
    expect(xs).toEqual([16, 488]);
  });
});

describe("snapDrag", () => {
  const grid = { xs: [0, 80, 160, 240], ys: [] as number[] };
  const canvas: [number, number] = [960, 540];

  it("does nothing with no neighbours and no grid", () => {
    const r = snapDrag({ at: [123, 45], size: [40, 40] }, [], null, canvas, 8);
    expect(r).toEqual({ dx: 0, dy: 0, guides: [] });
  });

  it("snaps a near-aligned left edge and draws a vertical guide", () => {
    const other: Frame = { at: [100, 200], size: [40, 40] };
    const r = snapDrag({ at: [103, 52], size: [40, 40] }, [other], null, canvas, 8);
    // left 103→100 (dx -3), top 52→200-edge is far; but the other's top 200 is
    // 148 away, so no y snap here.
    expect(r.dx).toBe(-3);
    expect(r.dy).toBe(0);
    const vx = r.guides.find((g) => g.axis === "x");
    expect(vx?.pos).toBe(100);
    expect(vx?.grid).toBe(false);
  });

  it("snaps both axes to one neighbour's top-left", () => {
    const other: Frame = { at: [100, 50], size: [40, 40] };
    const r = snapDrag({ at: [103, 52], size: [40, 40] }, [other], null, canvas, 8);
    expect(r.dx).toBe(-3);
    expect(r.dy).toBe(-2);
    expect(r.guides.some((g) => g.axis === "x" && g.pos === 100)).toBe(true);
    expect(r.guides.some((g) => g.axis === "y" && g.pos === 50)).toBe(true);
  });

  it("snaps to a grid line when nothing else aligns", () => {
    const r = snapDrag({ at: [82, 300], size: [40, 40] }, [], grid, canvas, 8);
    expect(r.dx).toBe(-2);
    const vx = r.guides.find((g) => g.axis === "x");
    expect(vx?.grid).toBe(true);
    expect(vx?.pos).toBe(80);
    // A grid guide spans the whole canvas cross-axis.
    expect(vx?.from).toBe(0);
    expect(vx?.to).toBe(canvas[1]);
  });

  it("prefers an object edge over a grid line at equal distance", () => {
    // other's left at 84 (2 away) ties the grid line at 80... make the object
    // strictly closer to prove the tie-break toward object edges.
    const other: Frame = { at: [83, 300], size: [40, 40] };
    const r = snapDrag({ at: [82, 300], size: [40, 40] }, [other], grid, canvas, 8);
    const vx = r.guides.find((g) => g.axis === "x");
    expect(vx?.grid).toBe(false);
    expect(vx?.pos).toBe(83);
  });

  it("stays free past the threshold", () => {
    // Every anchor (left/center/right = 123/143/163) sits >8pt from the other's
    // edges and from any grid line (0/80/160/240), so nothing snaps.
    const other: Frame = { at: [400, 400], size: [40, 40] };
    const r = snapDrag({ at: [123, 55], size: [40, 40] }, [other], grid, canvas, 8);
    expect(r).toEqual({ dx: 0, dy: 0, guides: [] });
  });
});
