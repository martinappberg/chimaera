import { describe, expect, it } from "vitest";

import { buildIndex, byDateDesc, cleanTitle, entriesForId, qualifiedId, resolveRef, searchEntries } from "./entries";
import { sample } from "./snapshot.fixture";

describe("buildIndex", () => {
  const idx = buildIndex(sample());

  it("keeps a reused id as distinct entries with unique keys", () => {
    const f177 = entriesForId(idx, "f-177");
    expect(f177.map((e) => e.topic).sort()).toEqual(["onboarding", "qc"]);
    expect(new Set(idx.entries.map((e) => e.ekey)).size).toBe(idx.entries.length);
  });

  it("qualifies a reused id by topic, and only then", () => {
    const [a] = entriesForId(idx, "F-177");
    expect(qualifiedId(idx, a)).toMatch(/^F-177 · (onboarding|qc)$/);
    expect(qualifiedId(idx, entriesForId(idx, "F-228")[0])).toBe("F-228");
  });

  it("indexes every kind with its file and line", () => {
    const kinds = new Set(idx.entries.map((e) => e.kind));
    expect([...kinds].sort()).toEqual(["convention", "decision", "finding", "learning", "session", "todo"]);
    const d = entriesForId(idx, "D-157")[0];
    expect(d.file).toBe(".living/decisions.md");
    expect(d.line).toBe(900);
    expect(d.stated).toBe("DECIDED (agent)");
  });

  it("shows a status only as written: none stays empty", () => {
    expect(entriesForId(idx, "F-228")[0].stated).toBe("");
    const corrected = entriesForId(idx, "F-171")[0];
    expect(corrected.stated).toBe("supported (provider manifest)");
  });

  it("resolves a reference to the citing finding's own topic first", () => {
    const qc = entriesForId(idx, "F-177").find((e) => e.topic === "qc")!;
    const near = resolveRef(idx, { kind: "finding", id: "F-177" }, qc);
    expect(near.map((e) => e.topic)).toEqual(["qc"]);
    const ambiguous = resolveRef(idx, { kind: "finding", id: "F-177" });
    expect(ambiguous).toHaveLength(2);
  });

  it("narrows by kind when the reference says one", () => {
    expect(resolveRef(idx, { kind: "decision", id: "D-152" }).map((e) => e.kind)).toEqual(["decision"]);
    expect(resolveRef(idx, { kind: "finding", id: "D-152" }).map((e) => e.kind)).toEqual(["decision"]);
  });

  it("builds backlinks from refs", () => {
    const d152 = entriesForId(idx, "D-152")[0];
    const citing = (idx.backlinks.get(d152.ekey) ?? []).map((e) => e.id).sort();
    expect(citing).toEqual(["D-157", "F-228"]);
    const f171 = entriesForId(idx, "F-171")[0];
    expect((idx.backlinks.get(f171.ekey) ?? []).map((e) => e.topic).sort()).toEqual(["onboarding", "qc"]);
  });

  it("never lists an entry as its own backlink", () => {
    for (const [key, list] of idx.backlinks) expect(list.some((e) => e.ekey === key)).toBe(false);
  });
});

describe("searchEntries", () => {
  const idx = buildIndex(sample());

  it("returns the same array for an empty query", () => {
    expect(searchEntries(idx.entries, "  ")).toBe(idx.entries);
  });

  it("needs every word, across ids, text and citations", () => {
    expect(searchEntries(idx.entries, "saturation").map((e) => e.id)).toEqual(["F-228"]);
    expect(searchEntries(idx.entries, "reference mixed").map((e) => e.id)).toEqual(["F-177"]);
    expect(searchEntries(idx.entries, "t-chaining").map((e) => e.kind)).toEqual(["todo"]);
  });
});

describe("byDateDesc", () => {
  it("sorts newest first with undated last", () => {
    const out = byDateDesc([{ date: "" }, { date: "2026-01-02" }, { date: "2026-03-01" }]);
    expect(out.map((x) => x.date)).toEqual(["2026-03-01", "2026-01-02", ""]);
  });
});

describe("cleanTitle", () => {
  it("drops a trailing date the heading repeats", () => {
    expect(cleanTitle("Depth has no knee (2026-09-28)")).toBe("Depth has no knee");
    expect(cleanTitle("Depth [2026-09-28]")).toBe("Depth");
  });

  it("keeps anything else, and never empties a title", () => {
    expect(cleanTitle("Stage 08 (v2)")).toBe("Stage 08 (v2)");
    expect(cleanTitle("(2026-09-28)")).toBe("(2026-09-28)");
  });
});
