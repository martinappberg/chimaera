import { describe, expect, it } from "vitest";

import { buildIndex } from "./entries";
import { filtersFor, listGroups, statusHead, todoRest } from "./list";
import { sample } from "./snapshot.fixture";

const idx = buildIndex(sample());
const findings = idx.entries.filter((e) => e.kind === "finding");

describe("filtersFor", () => {
  it("offers only chips that narrow what is there", () => {
    const f = filtersFor("findings", findings, "2026-09-22");
    const ids = f.map((x) => x.id);
    expect(ids[0]).toBe("all");
    expect(ids).toContain("week");
    expect(ids).toContain("amended");
    expect(ids).toContain("status:established");
    expect(ids).toContain("status:supported");
    for (const x of f.slice(1)) expect(x.count).toBeGreaterThan(0);
  });

  it("keeps a resolved entry out of Corrections", () => {
    const resolved = { ...findings[0], amends: [], state: { kind: "resolved", by: "" } };
    const c = filtersFor("findings", findings, "2026-09-22").find((x) => x.id === "amended")!;
    expect(c.label).toBe("Corrections");
    expect(c.test(resolved)).toBe(false);
    expect(c.test({ ...resolved, state: { kind: "corrected", by: "F-9" } })).toBe(true);
  });

  it("splits decisions into in force and superseded", () => {
    const d = filtersFor(
      "decisions",
      idx.entries.filter((e) => e.kind === "decision"),
      "2026-09-01",
    );
    expect(d.map((x) => x.id)).toEqual(["all", "force", "retired"]);
  });
});

describe("listGroups", () => {
  it("groups findings by topic, the topic with the newest entry first", () => {
    const g = listGroups("findings", findings);
    expect(g.map((x) => x.title)).toEqual(["depth", "onboarding", "qc"]);
    expect(g[0].entries.map((e) => e.id)).toEqual(["F-228", "F-227"]);
  });

  it("folds done to-dos", () => {
    const g = listGroups(
      "todos",
      idx.entries.filter((e) => e.kind === "todo"),
    );
    expect(g.find((x) => x.title === "Done")?.folded).toBe(true);
    expect(g[0].title).toBe("In progress");
  });
});

describe("statusHead and todoRest", () => {
  it("reads the first word of a status", () => {
    expect(statusHead("  Established by realignment")).toBe("established");
    expect(statusHead("")).toBe("");
  });

  it("gives what follows a to-do's first sentence", () => {
    const t = idx.entries.find((e) => e.id === "T-Chaining")!.todo!;
    expect(todoRest(t)).toBe("Replay 20 units.");
    const same = idx.entries.find((e) => e.kind === "todo" && e.todo?.item === "Publication figures")!.todo!;
    expect(todoRest(same)).toBe("");
  });
});
