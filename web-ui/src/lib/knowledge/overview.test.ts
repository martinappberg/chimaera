import { describe, expect, it } from "vitest";

import { buildIndex, entriesForId } from "./entries";
import {
  isBadNews,
  isRetired,
  isoDay,
  openWork,
  priorityRank,
  providerLabels,
  stateLabel,
  statusWord,
  todoGroup,
  todoGroups,
  waitingOnYou,
  whatChanged,
} from "./overview";
import { sample } from "./snapshot.fixture";

describe("providerLabels", () => {
  it("lays the provider's words over neutral defaults", () => {
    const l = providerLabels(sample());
    expect(l.sections.learnings).toBe("Watch out for");
    expect(l.sections.findings).toBe("Findings");
    expect(l.kinds.todo).toBe("to-do");
    expect(l.kinds.finding).toBe("finding");
    expect(l.source).toBe("sample · read-only");
  });

  it("names no provider's status words without labels", () => {
    const k = sample();
    k.labels = null;
    const l = providerLabels(k);
    expect(l.status_words).toEqual([]);
    expect(l.source).toBe("");
  });
});

describe("statusWord", () => {
  const l = providerLabels(sample());

  it("matches the first word of a stated status to the provider's words", () => {
    expect(statusWord(l, "supported (measured twice)")?.rank).toBe(2);
    expect(statusWord(l, "Contradicted by F-9")?.tone).toBe("bad");
  });

  it("leaves any other wording alone", () => {
    expect(statusWord(l, "established by realignment")).toBeNull();
    expect(statusWord(l, "")).toBeNull();
  });
});

describe("states", () => {
  const idx = buildIndex(sample());

  it("labels a correction and a supersession with who did it", () => {
    const f171 = entriesForId(idx, "F-171")[0];
    expect(stateLabel(f171)).toEqual({ text: "corrected by F-177", tone: "bad" });
    const d152 = entriesForId(idx, "D-152")[0];
    expect(stateLabel(d152)).toEqual({ text: "superseded → D-157", tone: "warn" });
    expect(isRetired(d152)).toBe(true);
  });

  it("counts amending entries as bad news", () => {
    const f177 = entriesForId(idx, "F-177").find((e) => e.topic === "onboarding")!;
    expect(isBadNews(f177)).toBe(true);
    expect(isBadNews(entriesForId(idx, "F-228")[0])).toBe(false);
  });
});

describe("whatChanged", () => {
  const idx = buildIndex(sample());

  it("groups the last week by day, newest first", () => {
    const days = whatChanged(idx, "2026-09-28", 7);
    expect(days.map((d) => d.day)).toEqual(["2026-09-28", "2026-09-27", "2026-09-26"]);
    expect(days[0].entries.map((e) => e.id)).toEqual(["F-228", "D-157"]);
  });

  it("leads a day with bad news", () => {
    const days = whatChanged(idx, "2026-09-04", 7);
    expect(days[0].day).toBe("2026-09-03");
    expect(days[0].entries[0].id).toBe("F-177");
  });

  it("is empty for a quiet week", () => {
    expect(whatChanged(idx, "2027-01-01", 7)).toEqual([]);
  });
});

describe("to-dos", () => {
  const idx = buildIndex(sample());
  const todos = idx.entries.filter((e) => e.kind === "todo");

  it("groups by the status as written", () => {
    expect(todos.map((e) => todoGroup(e.todo!))).toEqual(["progress", "done", "blocked", "open"]);
  });

  it("orders groups and folds done", () => {
    expect(todoGroups(todos).map((g) => g.group)).toEqual(["progress", "blocked", "open", "done"]);
  });

  it("ranks priorities, unknown last", () => {
    expect(priorityRank("**critical**")).toBe(0);
    expect(priorityRank("Medium")).toBe(2);
    expect(priorityRank("whenever")).toBe(5);
  });

  it("lists open work: in progress, blocked, then critical or high", () => {
    const w = openWork(idx);
    expect(w.open).toBe(3);
    expect(w.progress).toBe(1);
    expect(w.blocked).toBe(1);
    expect(w.items.map((e) => todoGroup(e.todo!))).toEqual(["progress", "blocked"]);
  });
});

describe("waitingOnYou", () => {
  it("maps asks to their entries, newest first", () => {
    const k = sample();
    const w = waitingOnYou(k, buildIndex(k));
    expect(w.total).toBe(2);
    const withEntry = w.items.find((x) => x.entry !== null)!;
    expect(withEntry.entry!.id).toBe("F-228");
    const handoff = w.items.find((x) => x.entry === null)!;
    expect(handoff.sourceLabel).toBe("handoff");
  });

  it("dates a handoff ask by the reader's own day of the handoff write", () => {
    const k = sample();
    const w = waitingOnYou(k, buildIndex(k));
    const handoff = w.items.find((x) => x.entry === null)!;
    expect(handoff.date).toBe(isoDay(k.left_off!.written_ms));
  });
});
