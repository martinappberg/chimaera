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
    fsDraftList: async () => [],
    fsDraftGet: async () => null,
  };
});

vi.mock("./files", () => ({
  fsDraftPut: net.fsDraftPut,
  fsDraftDelete: net.fsDraftDelete,
  fsDraftList: net.fsDraftList,
  fsDraftGet: net.fsDraftGet,
}));

import { clear, journal, type DraftRecord } from "./drafts";

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
