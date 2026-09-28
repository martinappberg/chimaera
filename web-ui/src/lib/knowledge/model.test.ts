import { describe, expect, it } from "vitest";
import { normalizeKnowledge, type Knowledge } from "../workspace/knowledge";
import {
  categoryTone,
  ledgerCaption,
  ladderFill,
  ledgerSummary,
  statusMoves,
  searchKnowledge,
  sectionNav,
  statusRank,
  todoTone,
  whereThingsStand,
} from "./model";

function fixture(): Knowledge {
  return normalizeKnowledge({
    schema: 1,
    provider: "mycelium",
    updated_ms: 1,
    left_off: { current: "QC uses MAD", next: ["Re-run DE"], blockers: ["de_all needs 128G"], path: ".mycelium/last-session.md" },
    topics: [
      {
        slug: "batch-effects",
        description: "Is sample 3 different?",
        path: ".living/findings/batch-effects.md",
        findings: [
          {
            id: "F-003",
            claim: "Sample 3's separation is a batch effect",
            status: "supported",
            implications: "Exclude S3 from DE",
            tags: ["batch"],
            ledger: [
              { date: "2026-09-18", run: "run-12", dataset: "PBMC-S3", project: "p", result: "clusters by prep date", direction: "supports" },
              { date: "2026-09-25", run: "s-003", dataset: "atlas-v2", project: "p", result: "vanishes after Harmony", direction: "supports" },
            ],
            questions: ["Would a re-sequenced S3 confirm it?"],
            line: 10,
            updated: "2026-09-25",
          },
        ],
      },
      {
        slug: "exhaustion",
        description: "Exhaustion programs",
        path: ".living/findings/exhaustion.md",
        findings: [
          {
            id: "F-001",
            claim: "TOX marks exhausted CD8 cells",
            status: "robust",
            implications: "",
            tags: [],
            ledger: [{ date: "2026-09-02", run: "r", dataset: "d", project: "p", result: "x", direction: "refines" }],
            questions: [],
            line: 1,
            updated: "2026-09-19",
          },
          {
            id: "F-002",
            claim: "IL7R+ memory expands after treatment",
            status: "contradicted",
            implications: "",
            tags: ["memory"],
            ledger: [
              { date: "2026-09-10", run: "r", dataset: "d", project: "p", result: "x", direction: "supports" },
              { date: "2026-09-25", run: "r", dataset: "donor 4", project: "p", result: "no change", direction: "contradicts" },
            ],
            questions: [],
            line: 20,
            updated: "2026-09-25",
          },
        ],
      },
    ],
    decisions: [{ fp: "d1", date: "2026-09-20", title: "Harmony, not scVI", context: "", decision: "Use Harmony", alternatives: ["scVI"], rationale: "", consequences: "", tags: [], line: 1 }],
    learnings: [{ fp: "l1", date: "2026-09-25", title: "Excel mangles gene names", category: "gotcha", what: "MARCH1 became a date", why: "read as text", resolution: "TSV", tags: ["excel"], line: 1 }],
    todos: [{ item: "Bump de_all to 128G", priority: "high", status: "blocked", category: "pipeline", date: "2026-09-25", author: "claude", file: "" }],
    questions: [{ text: "Is the expansion donor-driven?", finding: "F-002" }],
    guidance: [{ path: "MYCELIUM.md", label: "MYCELIUM.md", description: "How agents work here" }],
    warnings: [],
  });
}

describe("the confidence ladder", () => {
  it("maps mycelium's statuses to filled marks (contradicted = ✕)", () => {
    expect(ladderFill("preliminary")).toBe(1);
    expect(ladderFill("supported")).toBe(2);
    expect(ladderFill("robust")).toBe(3);
    expect(ladderFill("contradicted")).toBeNull();
    expect(ladderFill("unknown")).toBe(0);
  });

  it("ranks contradicted first, then strongest", () => {
    expect([...["preliminary", "robust", "contradicted", "supported"]].sort((a, b) => statusRank(a) - statusRank(b))).toEqual([
      "contradicted",
      "robust",
      "supported",
      "preliminary",
    ]);
  });

  it("summarizes a ledger into the evidence strip's caption", () => {
    const f = fixture();
    const contradicted = f.topics[1].findings[1];
    expect(ledgerSummary(contradicted.ledger)).toEqual({ total: 2, supports: 1, contradicts: 1, refines: 0 });
    expect(ledgerCaption(contradicted.ledger)).toBe("2 runs · 1 contradicts");
    expect(ledgerCaption(f.topics[0].findings[0].ledger)).toBe("2 runs");
    expect(ledgerCaption([])).toBe("no evidence yet");
  });
});

describe("whereThingsStand", () => {
  it("leads with the contradiction, then the strongest, capped", () => {
    const picks = whereThingsStand(fixture(), 2);
    expect(picks.map((p) => p.finding.id)).toEqual(["F-002", "F-001"]);
    expect(whereThingsStand(fixture(), 5).map((p) => p.finding.id)).toEqual(["F-002", "F-001", "F-003"]);
  });
});

describe("searchKnowledge", () => {
  it("returns the same reference for an empty query", () => {
    const k = fixture();
    expect(searchKnowledge(k, "   ")).toBe(k);
  });

  it("filters every section and drops topics with no surviving finding", () => {
    const k = searchKnowledge(fixture(), "s3");
    expect(k.topics.map((t) => t.slug)).toEqual(["batch-effects"]);
    expect(k.counts.findings).toBe(1);
    expect(k.decisions).toHaveLength(0);
    expect(k.learnings).toHaveLength(0);
    expect(k.todos).toHaveLength(0);
  });

  it("matches ids, tags, ledger text, todos and questions case-insensitively", () => {
    expect(searchKnowledge(fixture(), "f-002").counts.findings).toBe(1);
    expect(searchKnowledge(fixture(), "EXCEL").learnings).toHaveLength(1);
    expect(searchKnowledge(fixture(), "donor 4").topics[0].findings[0].id).toBe("F-002");
    expect(searchKnowledge(fixture(), "128g").todos).toHaveLength(1);
    expect(searchKnowledge(fixture(), "donor-driven").questions).toHaveLength(1);
    expect(searchKnowledge(fixture(), "harmony").decisions).toHaveLength(1);
  });
});

describe("sectionNav", () => {
  it("lists only sections with content, with counts where they mean something", () => {
    const nav = sectionNav(fixture());
    expect(nav.map((n) => `${n.key}:${n.count ?? "-"}`)).toEqual([
      "left:-",
      "found:3",
      "decided:1",
      "watch:1",
      "open:2",
      "guide:1",
    ]);
    const empty = normalizeKnowledge({ provider: null, guidance: [] });
    expect(sectionNav(empty)).toEqual([]);
  });
});

describe("tones pair colour with a word", () => {
  it("categories and todo statuses", () => {
    expect(categoryTone("gotcha")).toBe("warn");
    expect(categoryTone("failure")).toBe("err");
    expect(categoryTone("insight")).toBe("accent");
    expect(categoryTone("tip")).toBe("neutral");
    expect(todoTone("blocked")).toBe("warn");
    expect(todoTone("in-progress")).toBe("accent");
    expect(todoTone("open")).toBe("neutral");
  });
});

describe("statusMoves", () => {
  it("reads a finding's status move off the timeline within a day", () => {
    const now = 1_000_000_000;
    const entries = [
      { seq: 3, ts: now - 1000, kind: "knowledge", knowledge: { change: "status", id: "F-002", from: "supported", to: "contradicted", claim: "" } },
      { seq: 2, ts: now - 2000, kind: "knowledge", knowledge: { change: "status", id: "F-003", from: "preliminary", to: "supported", claim: "" } },
      { seq: 1, ts: now - 2 * 86_400_000, kind: "knowledge", knowledge: { change: "new", id: "F-001", to: "preliminary", claim: "" } },
    ] as const;
    expect(statusMoves(entries as never, now).get("F-002")).toEqual({ text: "contradicted today", tone: "err" });
    expect(statusMoves(entries as never, now).get("F-003")).toEqual({ text: "now supported", tone: "accent" });
    expect(statusMoves(entries as never, now).get("F-001")).toBeNull();
    expect(statusMoves(entries as never, now).get("F-009")).toBeUndefined();
  });
});

describe("normalizeKnowledge", () => {
  // A real .living/ files "### F-027 addendum:" under F-027 and numbers some
  // decisions twice; a repeated list key threw in Svelte and wedged the window.
  it("gives every row a unique key even when ids and fingerprints repeat", () => {
    const k = normalizeKnowledge({
      provider: "mycelium",
      topics: [
        { slug: "cohort", path: "a.md", findings: [{ id: "F-027" }, { id: "F-027" }, { id: "F-027~2" }, {}] },
        { slug: "cohort", path: "b.md", findings: [{ id: "F-027" }] },
      ],
      decisions: [{ fp: "d1" }, { fp: "d1" }],
      learnings: [{ fp: "l1" }, { fp: "l1" }, { fp: "l1" }],
    });
    const findings = k.topics.flatMap((t) => t.findings);
    expect(findings.map((f) => f.id)).toEqual(["F-027", "F-027", "F-027~2", "", "F-027"]);
    expect(new Set(findings.map((f) => f.key)).size).toBe(findings.length);
    expect(findings[0].key).toBe("F-027");
    expect(new Set(k.topics.map((t) => t.key)).size).toBe(2);
    expect(k.decisions.map((d) => d.key)).toEqual(["d1", "d1~2"]);
    expect(k.learnings.map((l) => l.key)).toEqual(["l1", "l1~2", "l1~3"]);
  });

  it("turns non-string fields and non-object rows into something renderable", () => {
    const k = normalizeKnowledge({
      provider: "mycelium",
      topics: [{ slug: 7, findings: [{ id: 3, claim: null, tags: ["a", 1], status: 5 }, "junk"] }],
      todos: [{ item: "x", status: null }, 4],
      questions: [{ text: 9, finding: null }],
      decisions: [null, { fp: "d", alternatives: [1, "b"] }],
    });
    const f = k.topics[0].findings;
    expect(f).toHaveLength(1);
    expect(f[0]).toMatchObject({ id: "", claim: "", status: "unknown", tags: ["a"] });
    expect(k.todos).toEqual([expect.objectContaining({ item: "x", status: "" })]);
    expect(k.questions).toEqual([{ text: "", finding: "" }]);
    expect(k.decisions).toHaveLength(1);
    expect(k.decisions[0].alternatives).toEqual(["b"]);
    expect(searchKnowledge(k, "x").todos).toHaveLength(1);
  });
});
