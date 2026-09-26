import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  codeSpanRefs,
  groupByBases,
  hrefRef,
  HIT_TTL_MS,
  MISS_TTL_MS,
  PathResolver,
  reopenResolution,
  resolveScope,
  type PathHit,
  type Resolution,
  type ValidateAnswer,
} from "./paths";

describe("codeSpanRefs", () => {
  it("offers the whole span when it is a reference", () => {
    expect(codeSpanRefs("src/x.rs:12")).toEqual({ whole: { path: "src/x.rs", line: 12 }, parts: [] });
    expect(codeSpanRefs("Screenshot (1).png").whole).toEqual({ path: "Screenshot (1).png" });
  });
  it("offers the references inside a command", () => {
    const { whole, parts } = codeSpanRefs("cat results/x.csv");
    expect(whole).toBeNull();
    expect(parts.map((p) => p.ref.path)).toEqual(["results/x.csv"]);
  });
  it("offers both when a spaced span could be either", () => {
    const { whole, parts } = codeSpanRefs("my notes/plan draft.md");
    expect(whole?.path).toBe("my notes/plan draft.md");
    expect(parts.map((p) => p.ref.path)).toEqual(["notes/plan", "draft.md"]);
  });
  it("offers nothing for a plain command", () => {
    expect(codeSpanRefs("npm run check")).toEqual({ whole: null, parts: [] });
  });
});

describe("hrefRef", () => {
  it("reads a local link target, anchor and escapes included", () => {
    expect(hrefRef("src/a.rs#L10-L20")).toEqual({ path: "src/a.rs", line: 10, endLine: 20 });
    expect(hrefRef("demo%20assets/demo.csv")).toEqual({ path: "demo assets/demo.csv" });
    expect(hrefRef("./demo-assets/")).toEqual({ path: "./demo-assets/" });
  });
  it("refuses web links, in-page anchors and malformed targets", () => {
    expect(hrefRef("https://example.com/a.md")).toBeNull();
    expect(hrefRef("#install")).toBeNull();
    expect(hrefRef("javascript:alert(1)")).toBeNull();
    expect(hrefRef("")).toBeNull();
  });
});

describe("groupByBases", () => {
  const ctx = { cwd: "/w/sub", spawnCwd: "/w", root: "/w", workspaceId: "ws" };
  it("gives relative paths the whole ladder, ./ only the cwd, absolute any base", () => {
    const groups = groupByBases(["results/x.csv", "./run.sh", "/etc/hosts", "figs/a.png"], ctx);
    expect(groups).toEqual([
      { bases: ["/w/sub", "/w"], candidates: ["results/x.csv", "figs/a.png"] },
      { bases: ["/w/sub"], candidates: ["./run.sh", "/etc/hosts"] },
    ]);
  });
  it("drops what has nowhere to resolve", () => {
    expect(groupByBases(["x.rs"], { cwd: null, root: null, workspaceId: null })).toEqual([]);
  });
});

describe("PathResolver", () => {
  let now = 1_000;
  const clock = () => now;
  beforeEach(() => {
    vi.useFakeTimers();
    now = 1_000;
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  function answering(answer: (c: string[]) => ValidateAnswer | Promise<ValidateAnswer>) {
    const calls: string[][] = [];
    const resolver = new PathResolver(
      async (c) => {
        calls.push([...c]);
        return answer(c);
      },
      { now: clock, root: () => "/w" },
    );
    return { resolver, calls };
  }

  const hit = (path: string) => ({ path, kind: "file" as const });

  it("coalesces renderers into one batch and reports what became linkable", async () => {
    const { resolver, calls } = answering(() => ({ valid: { "a.rs": hit("/w/a.rs") } }));
    const one = resolver.resolve(["a.rs"]);
    const two = resolver.resolve(["b.rs", "a.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    expect(calls).toEqual([["a.rs", "b.rs"]]);
    expect(await one).toBe(true);
    expect(await two).toBe(true);
    expect(resolver.peek("a.rs")).toEqual({ state: "hit", hit: hit("/w/a.rs") });
    expect(resolver.peek("b.rs")).toEqual({ state: "miss" });
  });

  it("keeps several matches as ambiguous, one as a hit", async () => {
    const { resolver } = answering(() => ({
      valid: {},
      ambiguous: { "plot.png": [hit("/w/a/plot.png"), hit("/w/b/plot.png")], "x.md": [hit("/w/d/x.md")] },
    }));
    void resolver.resolve(["plot.png", "x.md"]);
    await vi.advanceTimersByTimeAsync(50);
    expect(resolver.peek("plot.png")).toEqual({
      state: "ambiguous",
      matches: [hit("/w/a/plot.png"), hit("/w/b/plot.png")],
    });
    expect(resolver.peek("x.md")).toEqual({ state: "hit", hit: hit("/w/d/x.md") });
    expect(resolver.label("/w/a/plot.png")).toBe("a/plot.png");
  });

  it("lets a miss stand for the TTL, then asks again", async () => {
    let exists = false;
    const { resolver, calls } = answering(() => ({
      valid: exists ? { "out.png": hit("/w/out.png") } : ({} as Record<string, PathHit>),
    }));
    const first = resolver.resolve(["out.png"]);
    await vi.advanceTimersByTimeAsync(50);
    expect(await first).toBe(false);
    now += MISS_TTL_MS - 1;
    expect(resolver.peek("out.png")).toEqual({ state: "miss" });
    expect(await resolver.resolve(["out.png"])).toBe(false); // still standing: not re-asked
    expect(calls.length).toBe(1);
    now += 1;
    exists = true;
    expect(resolver.peek("out.png")).toBeUndefined();
    const again = resolver.resolve(["out.png"]);
    await vi.advanceTimersByTimeAsync(50);
    expect(await again).toBe(true);
    expect(calls.length).toBe(2);
  });

  it("never caches a transient failure or an unanswered candidate", async () => {
    let fail = true;
    const { resolver, calls } = answering(() => {
      if (fail) throw new Error("daemon unreachable");
      return { valid: {}, unchecked: ["late.rs"] };
    });
    const first = resolver.resolve(["x.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    expect(await first).toBe(false);
    expect(resolver.peek("x.rs")).toBeUndefined();
    fail = false;
    const second = resolver.resolve(["x.rs", "late.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    expect(await second).toBe(false);
    expect(calls.length).toBe(2);
    expect(resolver.peek("x.rs")).toEqual({ state: "miss" });
    expect(resolver.peek("late.rs")).toBeUndefined();
  });

  it("joins a candidate already in flight instead of asking twice", async () => {
    let release: (a: ValidateAnswer) => void = () => {};
    const { resolver, calls } = answering(() => new Promise((r) => (release = r)));
    const first = resolver.resolve(["a.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    const second = resolver.resolve(["a.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    expect(calls.length).toBe(1);
    release({ valid: { "a.rs": hit("/w/a.rs") } });
    expect(await first).toBe(true);
    expect(await second).toBe(true);
  });

  it("re-asks a standing miss on a click, without the batching delay", async () => {
    let exists = false;
    const { resolver, calls } = answering(() => ({
      valid: exists ? { "n.rs": hit("/w/n.rs") } : ({} as Record<string, PathHit>),
    }));
    void resolver.resolve(["n.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    exists = true;
    const clicked = resolver.resolveNow("n.rs");
    await vi.advanceTimersByTimeAsync(0);
    expect(await clicked).toEqual({ state: "hit", hit: hit("/w/n.rs") });
    expect(calls.length).toBe(2);
  });

  it("drops only misses at a turn end and tells the renderers", async () => {
    const { resolver } = answering(() => ({ valid: { "a.rs": hit("/w/a.rs") } }));
    void resolver.resolve(["a.rs", "b.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    const heard = vi.fn();
    const stop = resolver.onExpire(heard);
    resolver.expireMisses();
    expect(heard).toHaveBeenCalledTimes(1);
    expect(resolver.peek("a.rs")?.state).toBe("hit");
    expect(resolver.peek("b.rs")).toBeUndefined();
    resolver.expireMisses(); // nothing left to drop: nobody is woken
    expect(heard).toHaveBeenCalledTimes(1);
    stop();
  });

  it("keys verdicts by scope: an answer for one cwd never stands for another", async () => {
    let cwd = "/w/one";
    const calls: string[][] = [];
    const resolver = new PathResolver(
      async (c) => {
        calls.push([...c]);
        return { valid: { "x.rs": hit(`${cwd}/x.rs`) } };
      },
      {
        now: clock,
        scope: (c) => resolveScope({ cwd, root: "/w", workspaceId: "ws" }, c),
      },
    );
    void resolver.resolve(["x.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    expect(resolver.peek("x.rs")).toEqual({ state: "hit", hit: hit("/w/one/x.rs") });
    cwd = "/w/two"; // the agent cd'd: the same text now means another file
    expect(resolver.peek("x.rs")).toBeUndefined();
    void resolver.resolve(["x.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    expect(calls.length).toBe(2);
    expect(resolver.peek("x.rs")).toEqual({ state: "hit", hit: hit("/w/two/x.rs") });
    cwd = "/w/one"; // and back: the first answer still stands for its own scope
    expect(resolver.peek("x.rs")).toEqual({ state: "hit", hit: hit("/w/one/x.rs") });
  });

  it("lets a hit stand for its TTL, then asks again", async () => {
    const { resolver, calls } = answering(() => ({ valid: { "a.rs": hit("/w/a.rs") } }));
    void resolver.resolve(["a.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    now += HIT_TTL_MS - 1;
    expect(resolver.peek("a.rs")?.state).toBe("hit");
    expect(await resolver.resolve(["a.rs"])).toBe(false); // standing: not re-asked
    expect(calls.length).toBe(1);
    now += 1;
    expect(resolver.peek("a.rs")).toBeUndefined();
    const again = resolver.resolve(["a.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    expect(await again).toBe(true);
    expect(calls.length).toBe(2);
  });

  it("re-checks even a fresh hit on a click, and hears that the file is gone", async () => {
    let exists = true;
    const { resolver, calls } = answering(() => ({
      valid: exists ? { "m.rs": hit("/w/m.rs") } : ({} as Record<string, PathHit>),
    }));
    void resolver.resolve(["m.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    exists = false; // moved or deleted since it was stamped
    const clicked = resolver.resolveNow("m.rs");
    await vi.advanceTimersByTimeAsync(0);
    expect(await clicked).toEqual({ state: "miss" });
    expect(calls.length).toBe(2);
    expect(resolver.peek("m.rs")).toEqual({ state: "miss" });
  });

  it("reports an unanswered click re-check as unknown, not as the stale hit", async () => {
    let fail = false;
    const { resolver } = answering(() => {
      if (fail) throw new Error("daemon unreachable");
      return { valid: { "u.rs": hit("/w/u.rs") } };
    });
    void resolver.resolve(["u.rs"]);
    await vi.advanceTimersByTimeAsync(50);
    fail = true;
    const clicked = resolver.resolveNow("u.rs");
    await vi.advanceTimersByTimeAsync(0);
    expect(await clicked).toBeUndefined();
  });

  it("answers every waiter on dispose", async () => {
    const { resolver } = answering(() => new Promise(() => {}));
    const pending = resolver.resolve(["a.rs"]);
    resolver.dispose();
    expect(await pending).toBe(false);
    expect(await resolver.resolve(["b.rs"])).toBe(false);
  });
});

describe("reopenResolution (a click on a stamped reference)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  const hit = (path: string) => ({ path, kind: "file" as const });
  const stamped: Resolution = { state: "hit", hit: hit("/w/old/plan.md") };
  const opts = { at: { x: 0, y: 0 }, label: (p: string) => p };

  function resolverAnswering(answer: () => ValidateAnswer) {
    return new PathResolver(async () => answer());
  }

  it("opens what the daemon answers now, not the stamp", async () => {
    const resolver = resolverAnswering(() => ({ valid: { "plan.md": hit("/w/new/plan.md") } }));
    const open = vi.fn();
    const done = reopenResolution(resolver, "plan.md", stamped, open, opts);
    await vi.advanceTimersByTimeAsync(0);
    expect((await done).state).toBe("hit");
    expect(open).toHaveBeenCalledWith("/w/new/plan.md", "file", { split: undefined, reveal: undefined });
  });

  it("opens nothing when the file is gone", async () => {
    const resolver = resolverAnswering(() => ({ valid: {} }));
    const open = vi.fn();
    const done = reopenResolution(resolver, "plan.md", stamped, open, opts);
    await vi.advanceTimersByTimeAsync(0);
    expect(await done).toEqual({ state: "miss" });
    expect(open).not.toHaveBeenCalled();
  });

  it("falls back to the stamp when the daemon cannot answer", async () => {
    const resolver = resolverAnswering(() => {
      throw new Error("daemon unreachable");
    });
    const open = vi.fn();
    const done = reopenResolution(resolver, "plan.md", stamped, open, opts);
    await vi.advanceTimersByTimeAsync(0);
    expect(await done).toBe(stamped);
    expect(open).toHaveBeenCalledWith("/w/old/plan.md", "file", { split: undefined, reveal: undefined });
  });
});
