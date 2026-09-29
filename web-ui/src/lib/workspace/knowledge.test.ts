import { describe, expect, it } from "vitest";

import { normalizeKnowledge } from "./knowledge";
import { RAW } from "../knowledge/snapshot.fixture";

describe("normalizeKnowledge (0.2 fields)", () => {
  const k = normalizeKnowledge(structuredClone(RAW));

  it("keeps spans as {path, line, end_line}", () => {
    const f = k.topics[0].findings[0];
    expect(f.span).toEqual({ path: ".living/findings/depth.md", line: 40, end_line: 60 });
    expect(k.left_off?.span?.end_line).toBe(20);
    expect(k.left_off?.sources).toHaveLength(2);
  });

  it("keys findings by the provider's key, unique across reused ids", () => {
    const keys = k.topics.flatMap((t) => t.findings.map((f) => f.key));
    expect(keys).toContain("onboarding/F-177");
    expect(keys).toContain("qc/F-177");
  });

  it("counts open to-dos by the provider's closed flag", () => {
    expect(k.counts.todos).toBe(3);
    expect(k.todos.find((t) => t.key === "todo/r1")?.closed).toBe(true);
  });

  it("reads the provider's words", () => {
    expect(k.labels?.kinds.todo).toBe("to-do");
    expect(k.labels?.status_words[0]).toEqual({ word: "supported", rank: 2, tone: "good" });
    expect(k.id_shapes).toHaveLength(4);
  });

  it("drops a span without a path or line", () => {
    const odd = normalizeKnowledge({ topics: [{ slug: "a", findings: [{ id: "F-1", span: { path: "", line: 3 } }] }] });
    expect(odd.topics[0].findings[0].span).toBeNull();
  });

  it("classifies an older provider's to-do status itself", () => {
    const old = normalizeKnowledge({ todos: [{ item: "x", status: "**done** 2026-09-01" }, { item: "y", status: "open" }] });
    expect(old.todos.map((t) => t.closed)).toEqual([true, false]);
    expect(old.todos[0].title).toBe("x");
  });
});
