import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * A fake daemon mirror whose replies the test releases in any order: each
 * request is held until `deliver(i)` applies it to `store` and answers, so a
 * test can try to land a later request before an earlier one.
 */
const net = vi.hoisted(() => {
  type Req = {
    kind: "put" | "delete";
    path: string;
    text?: string;
    keepalive: boolean;
    settle: () => void;
    done: boolean;
  };
  const store = new Map<string, string>();
  const sent: Req[] = [];
  const hold = <T>(req: Omit<Req, "settle" | "done">, apply: () => T): Promise<T> =>
    new Promise<T>((resolve) => {
      const r: Req = {
        ...req,
        done: false,
        settle: () => {
          r.done = true;
          resolve(apply());
        },
      };
      sent.push(r);
    });
  return {
    store,
    sent,
    fsDraftPut: (path: string, _base: string, text: string, keepalive = false) =>
      hold({ kind: "put", path, text, keepalive }, () => {
        store.set(path, text);
        return "ok" as const;
      }),
    fsDraftDelete: (path: string, keepalive = false) =>
      hold({ kind: "delete", path, keepalive }, () => {
        store.delete(path);
      }),
    fsDraftList: vi.fn(async () => [...store.keys()].map((path) => ({ path, base_hash: "h0", updated_ms: 1, bytes: 1 }))),
    fsDraftGet: vi.fn(async (path: string) =>
      store.has(path) ? { path, base_hash: "h0", text: store.get(path) ?? "", updated_ms: 1 } : null,
    ),
  };
});

vi.mock("./files", async (orig) => ({
  draftPutBody: (await orig<typeof import("./files")>()).draftPutBody,
  fsDraftPut: net.fsDraftPut,
  fsDraftDelete: net.fsDraftDelete,
  fsDraftList: net.fsDraftList,
  fsDraftGet: net.fsDraftGet,
}));

import { clear, find, journal, KEEPALIVE_BUDGET_BYTES, LIST_TTL_MS, type DraftRecord } from "./drafts";

const rec = (path: string, text: string): DraftRecord => ({
  path,
  baseHash: "h0",
  baseText: null,
  text,
  updatedMs: 0,
});

/** Let queued promise continuations run. */
const flush = () => new Promise<void>((r) => setTimeout(r, 0));

function deliver(i: number): void {
  net.sent[i].settle();
}

beforeEach(() => {
  net.store.clear();
  net.sent.length = 0;
  net.fsDraftList.mockClear();
});

describe("the draft mirror's per-path order", () => {
  it("never lets a late journal PUT re-create the draft a save cleared", async () => {
    const put = journal(rec("/w/a.md", "typed"));
    await flush();
    const cleared = clear("/w/a.md");
    await flush();
    // The DELETE waits for the PUT: nothing can overtake it.
    expect(net.sent.map((r) => r.kind)).toEqual(["put"]);
    deliver(0);
    await flush();
    expect(net.sent.map((r) => r.kind)).toEqual(["put", "delete"]);
    deliver(1);
    await Promise.all([put, cleared]);
    expect(net.store.has("/w/a.md")).toBe(false);
  });

  it("skips a write still queued when a newer write or a clear is issued", async () => {
    const first = journal(rec("/w/b.md", "one"));
    await flush();
    const second = journal(rec("/w/b.md", "two")); // queued behind the first
    const cleared = clear("/w/b.md");
    deliver(0);
    await flush();
    expect(net.sent.map((r) => r.kind)).toEqual(["put", "delete"]);
    deliver(1);
    await cleared;
    expect((await first).remote).toBe("ok");
    expect((await second).remote).toBe("superseded");
    expect(net.store.has("/w/b.md")).toBe(false);
  });

  it("keeps other paths independent", async () => {
    void journal(rec("/w/c.md", "c"));
    void journal(rec("/w/d.md", "d"));
    await flush();
    expect(net.sent.map((r) => r.path)).toEqual(["/w/c.md", "/w/d.md"]);
    deliver(1);
    deliver(0);
    await flush();
    expect(net.store.get("/w/c.md")).toBe("c");
    expect(net.store.get("/w/d.md")).toBe("d");
  });

  it("sends a hide/pagehide write at once, then again in order if the page lives", async () => {
    const older = journal(rec("/w/e.md", "old"));
    await flush();
    const newer = journal(rec("/w/e.md", "new"), true);
    await flush();
    // Not queued behind the in-flight write: a closing page would drop it.
    expect(net.sent.map((r) => [r.kind, r.text, r.keepalive])).toEqual([
      ["put", "old", false],
      ["put", "new", true],
    ]);
    // Reordered replies: the newer lands first, the older would win...
    deliver(1);
    deliver(0);
    await flush();
    expect(net.store.get("/w/e.md")).toBe("old");
    // ...so the newer is sent once more, after both.
    expect(net.sent.map((r) => [r.text, r.keepalive])).toEqual([
      ["old", false],
      ["new", true],
      ["new", false],
    ]);
    deliver(2);
    await Promise.all([older, newer]);
    expect(net.store.get("/w/e.md")).toBe("new");
  });
});

describe("the keepalive budget (a pagehide flush sends every dirty buffer at once)", () => {
  /** What one flush asked for, per path. */
  const keepalives = () => Object.fromEntries(net.sent.map((r) => [r.path, r.keepalive]));

  it("shares one budget across the flush, counted in encoded bytes", async () => {
    // 20k "é" is 20k UTF-16 units but 40 KB of UTF-8: two cannot both fit.
    const wide = "é".repeat(20_000);
    const flushed = [
      journal(rec("/w/k1.md", wide), true),
      journal(rec("/w/k2.md", wide), true),
      journal(rec("/w/k3.md", "small"), true),
    ];
    await flush();
    expect(keepalives()).toEqual({ "/w/k1.md": true, "/w/k2.md": false, "/w/k3.md": true });
    net.sent.forEach((r) => r.settle());
    await Promise.all(flushed);
    // Everything was still sent: an over-budget body goes as a normal request.
    expect([...net.store.keys()].sort()).toEqual(["/w/k1.md", "/w/k2.md", "/w/k3.md"]);
  });

  it("frees the budget as keepalive requests complete", async () => {
    const big = "x".repeat(KEEPALIVE_BUDGET_BYTES - 200);
    const first = journal(rec("/w/f1.md", big), true);
    await flush();
    const blocked = journal(rec("/w/f2.md", big), true);
    await flush();
    expect(keepalives()).toEqual({ "/w/f1.md": true, "/w/f2.md": false });
    net.sent.forEach((r) => r.settle());
    await Promise.all([first, blocked]);
    net.sent.length = 0;
    const later = journal(rec("/w/f3.md", big), true);
    await flush();
    expect(keepalives()).toEqual({ "/w/f3.md": true });
    net.sent[0].settle();
    await later;
  });

  it("never asks for keepalive over the budget, whatever the text length says", async () => {
    // Under the budget in UTF-16 units, over it once encoded.
    const text = "€".repeat(Math.floor(KEEPALIVE_BUDGET_BYTES / 2));
    const put = journal(rec("/w/euro.md", text), true);
    await flush();
    expect(keepalives()).toEqual({ "/w/euro.md": false });
    net.sent[0].settle();
    await put;
  });
});

describe("looking for a draft on open", () => {
  it("reuses one listing for a few seconds, dropped by this window's own writes", async () => {
    let now = 1_000_000;
    const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
    try {
      net.store.set("/w/g1.md", "kept");
      // A restored layout opens several files at once: one listing.
      const [a, b] = await Promise.all([find("/w/g1.md"), find("/w/g2.md")]);
      expect(a?.text).toBe("kept");
      expect(b).toBeNull();
      now += LIST_TTL_MS - 1;
      await find("/w/g3.md");
      expect(net.fsDraftList).toHaveBeenCalledTimes(1);

      // This window journals a draft: the next open must see it.
      const put = journal(rec("/w/g2.md", "mine"));
      await flush();
      net.sent[0].settle();
      await put;
      expect((await find("/w/g2.md"))?.text).toBe("mine");
      expect(net.fsDraftList).toHaveBeenCalledTimes(2);

      // Past the TTL (another origin may have written): listed afresh.
      now += LIST_TTL_MS;
      await find("/w/g1.md");
      expect(net.fsDraftList).toHaveBeenCalledTimes(3);
    } finally {
      clock.mockRestore();
    }
  });
});
