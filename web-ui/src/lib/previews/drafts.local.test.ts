import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The journal against a real-shaped IndexedDB: an in-memory stand-in that
 * runs requests asynchronously, in order, and commits a transaction once no
 * request is pending — the semantics drafts.ts relies on. (drafts.test.ts
 * runs without IndexedDB, as a private window would.)
 */
const idb = vi.hoisted(() => {
  type Rec = { path: string } & Record<string, unknown>;
  const data = new Map<string, Rec>();
  const later = (f: () => void) => setTimeout(f, 0);

  class Req<T> {
    result!: T;
    error: unknown = null;
    onsuccess: (() => void) | null = null;
    onerror: (() => void) | null = null;
    onupgradeneeded: (() => void) | null = null;
    onblocked: (() => void) | null = null;
  }

  class Tx {
    private pending = 0;
    private done = false;
    error: unknown = null;
    oncomplete: (() => void) | null = null;
    onabort: (() => void) | null = null;
    onerror: (() => void) | null = null;
    constructor() {
      this.settle();
    }
    private settle(): void {
      later(() => {
        if (this.pending > 0 || this.done) return;
        this.done = true;
        this.oncomplete?.();
      });
    }
    private request<T>(run: () => T): Req<T> {
      const req = new Req<T>();
      this.pending++;
      later(() => {
        req.result = run();
        this.pending--;
        req.onsuccess?.();
        this.settle();
      });
      return req;
    }
    objectStore() {
      return {
        put: (rec: Rec) => this.request(() => void data.set(rec.path, structuredClone(rec))),
        get: (key: string) => this.request(() => structuredClone(data.get(key))),
        getAll: () => this.request(() => [...data.values()].map((r) => structuredClone(r))),
        delete: (key: string) => this.request(() => void data.delete(key)),
      };
    }
  }

  const db = {
    objectStoreNames: { contains: () => true },
    createObjectStore: () => {},
    transaction: () => new Tx(),
  };
  (globalThis as Record<string, unknown>).indexedDB = {
    open: () => {
      const req = new Req<typeof db>();
      req.result = db;
      later(() => req.onsuccess?.());
      return req;
    },
  };
  return { data };
});

const net = vi.hoisted(() => ({
  remote: null as null | {
    path: string;
    base_hash: string;
    text: string;
    updated_ms: number;
    client_updated_ms: number | null;
  },
  fsDraftPut: vi.fn(async () => "ok" as const),
  fsDraftDelete: vi.fn(async () => {}),
}));

vi.mock("./files", async (orig) => ({
  draftPutBody: (await orig<typeof import("./files")>()).draftPutBody,
  fsDraftPut: net.fsDraftPut,
  fsDraftDelete: net.fsDraftDelete,
  fsDraftList: async () =>
    net.remote === null
      ? []
      : [{ path: net.remote.path, base_hash: "h0", updated_ms: net.remote.updated_ms, bytes: 1 }],
  fsDraftGet: async () => net.remote,
}));

import { find, journal, type DraftRecord } from "./drafts";

/** A recent base time: the journal prunes records older than 30 days. */
const T = Date.now();

const rec = (path: string, text: string, updatedMs: number): DraftRecord => ({
  path,
  baseHash: "h0",
  baseText: "base",
  text,
  updatedMs,
});

beforeEach(() => {
  idb.data.clear();
  net.remote = null;
});

describe("finding the newest draft across both layers", () => {
  it("compares the mirror's writer clock with the local copy's, never the daemon's", async () => {
    const path = "/w/clock.md";
    // This origin typed "local" at t=2000 (its clock); the mirror holds an
    // older text sent at t=1000 — but its daemon's clock runs far ahead.
    await journal(rec(path, "local", T + 2_000));
    net.remote = { path, base_hash: "h0", text: "mirror", updated_ms: T + 9_000_000, client_updated_ms: T + 1_000 };
    expect((await find(path))?.text).toBe("local");

    // A genuinely newer mirror copy (typed later on another origin) wins.
    net.remote = { ...net.remote, client_updated_ms: T + 3_000 };
    const found = await find(path);
    expect(found?.text).toBe("mirror");
    expect(found?.updatedMs).toBe(T + 3_000);
  });

  it("falls back to the daemon's stamp for a mirror copy without the writer's time", async () => {
    const path = "/w/legacy.md";
    await journal(rec(path, "local", T + 2_000));
    net.remote = { path, base_hash: "h0", text: "mirror", updated_ms: T + 1_500, client_updated_ms: null };
    expect((await find(path))?.text).toBe("local");
    net.remote = { ...net.remote, updated_ms: T + 2_500 };
    expect((await find(path))?.text).toBe("mirror");
  });

  it("sends this client's time with the mirror write", async () => {
    await journal(rec("/w/sent.md", "t", T + 4_242));
    expect(net.fsDraftPut).toHaveBeenCalledWith("/w/sent.md", "h0", "t", T + 4_242, false);
  });
});
