import { describe, expect, it } from "vitest";
import {
  boardsDirFor,
  chartProvenance,
  collectShownBoards,
  collectShownByGroup,
  hasProvenanceDetail,
  uniqueBoardName,
  workspaceRootFor,
  type ShownToolLike,
} from "./shownBoards";

const done = (text: string): ShownToolLike => ({
  status: "completed",
  denied: false,
  content: { kind: "output", text },
});

const SHOWN = "shown chart · 9 bars · talk-dark · 720×450 → ";

describe("collectShownBoards", () => {
  it("detects the shown signature and resolves relative paths against cwd", () => {
    const tools = [done(`${SHOWN}.chimaera/board/shown/a3f1.board.json`)];
    expect(collectShownBoards(tools, "/ws")).toEqual([
      { path: "/ws/.chimaera/board/shown/a3f1.board.json", revision: 1 },
    ]);
  });

  it("passes absolute paths through and keeps raw ones without a cwd", () => {
    const tools = [done(`${SHOWN}/abs/x.board.json\n${SHOWN}./rel/y.board.json`)];
    expect(collectShownBoards(tools).map((s) => s.path)).toEqual([
      "/abs/x.board.json",
      "./rel/y.board.json",
    ]);
  });

  it("ignores running, failed, denied, and non-output blocks", () => {
    const line = `${SHOWN}a.board.json`;
    const cases: ShownToolLike[] = [
      { status: "in_progress", denied: false, content: { kind: "output", text: line } },
      { status: "failed", denied: false, content: { kind: "output", text: line } },
      { status: "completed", denied: true, content: { kind: "output", text: line } },
      { status: "completed", denied: false, content: { kind: "diff", text: line } },
      { status: "completed", denied: false, content: null },
    ];
    expect(collectShownBoards(cases, "/ws")).toEqual([]);
  });

  it("requires the line-anchored signature, not a substring", () => {
    const tools = [done(`echo shown fake → x.board.json trailing`)];
    expect(collectShownBoards(tools, "/ws")).toEqual([]);
  });

  it("dedupes a same-path re-show within one run, counting revisions", () => {
    const tools = [done(`${SHOWN}s.board.json`), done(`${SHOWN}s.board.json`)];
    expect(collectShownBoards(tools, "/ws")).toEqual([
      { path: "/ws/s.board.json", revision: 2 },
    ]);
  });

  it("keeps distinct boards as distinct cards in shown order", () => {
    const tools = [done(`${SHOWN}a.board.json\n${SHOWN}b.board.json`)];
    expect(collectShownBoards(tools, "/ws").map((s) => s.path)).toEqual([
      "/ws/a.board.json",
      "/ws/b.board.json",
    ]);
  });

  it("matches the canonical .board extension the CLI now emits", () => {
    const tools = [done(`${SHOWN}.chimaera/board/shown/a3f1.board`)];
    expect(collectShownBoards(tools, "/ws")).toEqual([
      { path: "/ws/.chimaera/board/shown/a3f1.board", revision: 1 },
    ]);
  });

  it("matches .board and legacy .board.json side by side", () => {
    const tools = [done(`${SHOWN}new.board\n${SHOWN}old.board.json`)];
    expect(collectShownBoards(tools, "/ws").map((s) => s.path)).toEqual([
      "/ws/new.board",
      "/ws/old.board.json",
    ]);
  });
});

describe("collectShownByGroup (update-in-place across turns)", () => {
  it("moves a re-shown board's single card to the latest group", () => {
    const groups = [
      { key: "g1", tools: [done(`${SHOWN}s.board.json`)] },
      { key: "g2", tools: [done(`${SHOWN}s.board.json`)] },
    ];
    const out = collectShownByGroup(groups, "/ws");
    expect(out.get("g1")).toBeUndefined();
    expect(out.get("g2")).toEqual([{ path: "/ws/s.board.json", revision: 2 }]);
  });

  it("leaves unrelated boards where they were shown", () => {
    const groups = [
      { key: "g1", tools: [done(`${SHOWN}a.board.json`)] },
      { key: "g2", tools: [done(`${SHOWN}b.board.json`)] },
    ];
    const out = collectShownByGroup(groups, "/ws");
    expect(out.get("g1")).toEqual([{ path: "/ws/a.board.json", revision: 1 }]);
    expect(out.get("g2")).toEqual([{ path: "/ws/b.board.json", revision: 1 }]);
  });

  it("a third re-show keeps exactly one card and bumps the revision again", () => {
    const groups = [
      { key: "g1", tools: [done(`${SHOWN}s.board.json`)] },
      { key: "g2", tools: [done(`${SHOWN}s.board.json`)] },
      { key: "g3", tools: [done(`${SHOWN}s.board.json`)] },
    ];
    const out = collectShownByGroup(groups, "/ws");
    expect([...out.keys()]).toEqual(["g3"]);
    expect(out.get("g3")).toEqual([{ path: "/ws/s.board.json", revision: 3 }]);
  });
});

describe("workspaceRootFor / boardsDirFor", () => {
  it("derives the root from the shown/ pen prefix exactly", () => {
    const p = "/home/u/proj/.chimaera/board/shown/a.board.json";
    expect(workspaceRootFor(p, "/elsewhere")).toBe("/home/u/proj");
    expect(boardsDirFor(p)).toBe("/home/u/proj/boards");
  });

  it("falls back to the session cwd for boards outside the pen", () => {
    expect(boardsDirFor("/tmp/x.board.json", "/ws")).toBe("/ws/boards");
  });

  it("hides the affordance (null) when nothing anchors a workspace", () => {
    expect(boardsDirFor("/tmp/x.board.json")).toBeNull();
  });
});

describe("uniqueBoardName", () => {
  it("returns the desired name when free", () => {
    expect(uniqueBoardName("a.board.json", new Set())).toBe("a.board.json");
  });

  it("suffixes before the FULL compound extension on collision", () => {
    const existing = new Set(["a.board.json"]);
    expect(uniqueBoardName("a.board.json", existing)).toBe("a-2.board.json");
  });

  it("walks past every taken suffix", () => {
    const existing = new Set(["a.board.json", "a-2.board.json", "a-3.board.json"]);
    expect(uniqueBoardName("a.board.json", existing)).toBe("a-4.board.json");
  });

  it("survives a name with no extension", () => {
    expect(uniqueBoardName("plain", new Set(["plain"]))).toBe("plain-2");
  });

  it("keeps the canonical .board extension whole on collision", () => {
    const existing = new Set(["a.board"]);
    expect(uniqueBoardName("a.board", existing)).toBe("a-2.board");
  });
});

describe("chartProvenance", () => {
  const board = (data: Record<string, unknown>) =>
    JSON.stringify({
      format: "chimaera.board",
      pages: [{ id: "p", objects: [{ id: "c", type: "chart", data }] }],
    });

  it("maps origin to the schema's label vocabulary", () => {
    const p = chartProvenance(board({ origin: "derived-by-agent" }));
    expect(p).toEqual({ origin: "derived by agent", source: null, inputs: [], trace: null });
  });

  it("carries source, inputs, and trace through", () => {
    const p = chartProvenance(
      board({
        origin: "command",
        source: "results/t.csv",
        inputs: ["logs/a.txt", "logs/b.txt"],
        trace: "median of 3 runs via hyperfine",
      }),
    );
    expect(p).toEqual({
      origin: "from command",
      source: "results/t.csv",
      inputs: ["logs/a.txt", "logs/b.txt"],
      trace: "median of 3 runs via hyperfine",
    });
  });

  it("finds a chart nested in a group", () => {
    const json = JSON.stringify({
      pages: [
        {
          id: "p",
          objects: [
            {
              id: "g",
              type: "group",
              objects: [{ id: "c", type: "chart", data: { origin: "file" } }],
            },
          ],
        },
      ],
    });
    expect(chartProvenance(json)?.origin).toBe("from file");
  });

  it("is null for chartless boards, malformed json, and non-objects", () => {
    expect(chartProvenance("not json")).toBeNull();
    expect(chartProvenance("[1,2]")).toBeNull();
    expect(
      chartProvenance(JSON.stringify({ pages: [{ objects: [{ type: "text" }] }] })),
    ).toBeNull();
  });

  it("drops non-string junk from inputs and bounds them", () => {
    const p = chartProvenance(
      board({ origin: "command", inputs: [1, "ok.txt", null, "", "b.txt"] }),
    );
    expect(p?.inputs).toEqual(["ok.txt", "b.txt"]);
  });
});

describe("hasProvenanceDetail", () => {
  it("origin alone earns no chrome — the chip the user called noise", () => {
    expect(
      hasProvenanceDetail({ origin: "from command", source: null, inputs: [], trace: null }),
    ).toBe(false);
    expect(hasProvenanceDetail(null)).toBe(false);
  });

  it("any of source / inputs / trace does", () => {
    const base = { origin: "from command", source: null, inputs: [], trace: null };
    expect(hasProvenanceDetail({ ...base, source: "t.csv" })).toBe(true);
    expect(hasProvenanceDetail({ ...base, inputs: ["a"] })).toBe(true);
    expect(hasProvenanceDetail({ ...base, trace: "how" })).toBe(true);
  });
});
