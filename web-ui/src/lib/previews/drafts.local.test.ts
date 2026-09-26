import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The journal against a real-shaped IndexedDB: an in-memory stand-in that
 * runs requests asynchronously, in order, and commits a transaction once no
 * request is pending — the semantics drafts.ts relies on. (drafts.test.ts
 * runs without IndexedDB, as a private window would.)
 */
const idb = vi.hoisted(() => {
  type Rec = { path: string; text?: unknown; writer?: unknown };
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
    writer: string | null;
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

import { clear, find, journal, WRITER, type DraftRecord } from "./drafts";

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
  net.fsDraftPut.mockClear();
  net.fsDraftDelete.mockClear();
});

describe("finding the newest draft across both layers", () => {
  it("compares the mirror's writer clock with the local copy's, never the daemon's", async () => {
    const path = "/w/clock.md";
    // This origin typed "local" at t=2000 (its clock); the mirror holds an
    // older text sent at t=1000 — but its daemon's clock runs far ahead.
    await journal(rec(path, "local", T + 2_000));
    net.remote = {
      path,
      base_hash: "h0",
      text: "mirror",
      updated_ms: T + 9_000_000,
      client_updated_ms: T + 1_000,
      writer: "w2",
    };
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
    net.remote = { path, base_hash: "h0", text: "mirror", updated_ms: T + 1_500, client_updated_ms: null, writer: null };
    expect((await find(path))?.text).toBe("local");
    net.remote = { ...net.remote, updated_ms: T + 2_500 };
    expect((await find(path))?.text).toBe("mirror");
  });

  it("sends this client's time with the mirror write", async () => {
    await journal(rec("/w/sent.md", "t", T + 4_242));
    expect(net.fsDraftPut).toHaveBeenCalledWith("/w/sent.md", "h0", "t", T + 4_242, WRITER, false);
  });
});

describe("two windows of one origin share a path's record", () => {
  it("clears only this window's record, or one holding exactly the given text", async () => {
    const path = "/w/shared.md";
    // Another window of this origin journaled its own unsaved text last.
    idb.data.set(path, { ...rec(path, "theirs", T), writer: "other-window" });
    await clear(path); // this window saved or discarded
    expect(idb.data.get(path)?.text).toBe("theirs");
    // The mirror DELETE names this window: the daemon spares the other's.
    expect(net.fsDraftDelete).toHaveBeenLastCalledWith(path, WRITER, false);

    // A draft found on open and discarded (or equal to the disk) goes by
    // its own writer or text.
    await clear(path, { writer: "other-window", text: "theirs" });
    expect(idb.data.has(path)).toBe(false);
    expect(net.fsDraftDelete).toHaveBeenLastCalledWith(path, "other-window", false);

    // This window's own record goes on a plain clear.
    await journal(rec(path, "mine", T + 1));
    expect(idb.data.get(path)?.writer).toBe(WRITER);
    await clear(path);
    expect(idb.data.has(path)).toBe(false);

    // An older client's record names no writer: only its text matches it,
    // and the mirror drop is then unconditional.
    idb.data.set(path, rec(path, "legacy", T));
    await clear(path, { writer: undefined, text: "other" });
    expect(idb.data.has(path)).toBe(true);
    await clear(path, { writer: undefined, text: "legacy" });
    expect(idb.data.has(path)).toBe(false);
    expect(net.fsDraftDelete).toHaveBeenLastCalledWith(path, null, false);
  });

  it("carries the writer of a draft found only in the mirror", async () => {
    const path = "/w/remote-only.md";
    net.remote = { path, base_hash: "h0", text: "t", updated_ms: T, client_updated_ms: T, writer: "w9" };
    expect((await find(path))?.writer).toBe("w9");
  });
});
