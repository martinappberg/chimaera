import { describe, expect, it } from "vitest";

import type { SeqEvent } from "./chatWs";
import {
  planFolds,
  planProgress,
  planSummary,
  stepDetail,
  stepKeys,
  stepMark,
  stepText,
} from "./planCard";
import { ChatStore, type PlanEntry } from "./store.svelte";

function fold(events: Record<string, unknown>[]): ChatStore {
  const store = new ChatStore();
  events.forEach((ev, i) => store.apply({ seq: i + 1, ts: 1000 + i, ev } as SeqEvent));
  return store;
}

/** The same three-step plan as each runtime's driver normalizes it: claude's
 *  Task family carries ids, active forms and blockers; codex `update_plan`
 *  and an ACP plan carry only text and status. */
const CLAUDE_PLAN = {
  type: "plan",
  entries: [
    { content: "Read the config", status: "done", id: "1" },
    { content: "Run tests", status: "in_progress", id: "2", active_form: "Running tests" },
    { content: "Write the report", status: "todo", id: "3", blocked_by: ["2"], description: "Summarize failures" },
  ],
};
const CODEX_PLAN = {
  type: "plan",
  entries: [
    { content: "Read the config", status: "done" },
    { content: "Run tests", status: "in_progress" },
    { content: "Write the report", status: "todo" },
  ],
};

describe("the plan card's reading of every runtime's plan", () => {
  for (const [runtime, event] of [
    ["claude", CLAUDE_PLAN],
    ["codex", CODEX_PLAN],
    ["acp", CODEX_PLAN],
  ] as const) {
    it(`${runtime}: one row per step, the current step named, in order`, () => {
      const plan = fold([{ type: "turn_started", turn_id: "t" }, event]).plan;
      expect(plan.map(stepMark)).toEqual(["done", "current", runtime === "claude" ? "blocked" : "todo"]);
      const p = planProgress(plan);
      expect(p).toMatchObject({ done: 1, total: 3, allDone: false });
      expect(p.current?.content).toBe("Run tests");
      expect(planSummary(plan)).toBe(runtime === "claude" ? "Running tests" : "Run tests");
      expect(planFolds(plan, true)).toBe(false);
      expect(planFolds(plan, false)).toBe(false);
    });
  }

  it("a level-set update keeps every row's key, so the card updates in place", () => {
    const store = fold([CLAUDE_PLAN]);
    const before = stepKeys(store.plan);
    store.apply({
      seq: 99,
      ts: 2000,
      ev: {
        type: "plan",
        entries: CLAUDE_PLAN.entries.map((e) => ({ ...e, status: "done", blocked_by: [] })),
      },
    } as SeqEvent);
    expect(stepKeys(store.plan)).toEqual(before);
    // Positional keys for id-less plans are stable across updates too.
    expect(stepKeys(fold([CODEX_PLAN]).plan)).toEqual(["ix:0", "ix:1", "ix:2"]);
  });

  it("keys stay unique even when the agent repeats an id", () => {
    const entry = (id: string | null, content: string): PlanEntry => ({
      content,
      status: "todo",
      id,
      activeForm: null,
      description: null,
      owner: null,
      blockedBy: [],
    });
    const keys = stepKeys([entry("1", "a"), entry("1", "b"), entry("1#1", "c"), entry("1", "d"), entry(null, "e")]);
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys[0]).toBe("id:1");
    expect(keys[4]).toBe("ix:4");
  });

  it("a finished plan folds to one line only once the turn is over", () => {
    const done = fold([
      { type: "plan", entries: CODEX_PLAN.entries.map((e) => ({ ...e, status: "done" })) },
    ]).plan;
    expect(planProgress(done).allDone).toBe(true);
    expect(planFolds(done, true)).toBe(false);
    expect(planFolds(done, false)).toBe(true);
    expect(planSummary(done)).toBe("3 tasks done");
    expect(planSummary(done.slice(0, 1))).toBe("1 task done");
  });

  it("between steps the folded line names what is next", () => {
    const plan: PlanEntry[] = fold([
      {
        type: "plan",
        entries: [
          { content: "a", status: "done" },
          { content: "b", status: "todo" },
        ],
      },
    ]).plan;
    expect(planProgress(plan).current).toBeNull();
    expect(planSummary(plan)).toBe("b");
  });

  it("a step's text and detail say only what adds something", () => {
    const [read, run, write] = fold([CLAUDE_PLAN]).plan;
    expect(stepText(read)).toBe("Read the config");
    expect(stepText(run)).toBe("Running tests");
    // Not in flight: the subject, not the active form.
    expect(stepText({ ...run, status: "todo" })).toBe("Run tests");
    expect(stepDetail(write)).toBe("Summarize failures");
    expect(stepDetail({ ...write, description: "Write the report" })).toBeNull();
    expect(planFolds([], false)).toBe(false);
  });
});
