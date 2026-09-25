import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  codeSpanRefs,
  groupByBases,
  hrefRef,
  MISS_TTL_MS,
  PathResolver,
  type PathHit,
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

  it("answers every waiter on dispose", async () => {
    const { resolver } = answering(() => new Promise(() => {}));
    const pending = resolver.resolve(["a.rs"]);
    resolver.dispose();
    expect(await pending).toBe(false);
    expect(await resolver.resolve(["b.rs"])).toBe(false);
  });
});
