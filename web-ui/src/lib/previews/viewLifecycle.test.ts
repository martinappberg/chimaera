import { describe, expect, it, vi } from "vitest";

// The module wires runes to the file store; its cores need neither.
vi.mock("./fileStore.svelte", () => ({ retain: vi.fn(), release: vi.fn() }));

import { DiskVersionLoop, entryFor, FileClaim, RevealQueue, ViewSignal } from "./viewLifecycle.svelte";
import { watchPaneVisible } from "./paneVisible";
import { OwnedUrls } from "./blobUrls";

/** A load the test settles by hand. */
function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void; reject: (e: unknown) => void } {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const flush = (): Promise<void> => new Promise((done) => setTimeout(done, 0));

describe("ViewSignal", () => {
  it("aborts the previous generation on renew", () => {
    const sig = new ViewSignal();
    const first = sig.renew();
    const gen = sig.generation;
    expect(sig.isCurrent(gen)).toBe(true);
    const second = sig.renew();
    expect(first.aborted).toBe(true);
    expect(second.aborted).toBe(false);
    expect(sig.isCurrent(gen)).toBe(false);
  });

  it("aborts on dispose and refuses later generations", () => {
    const sig = new ViewSignal();
    const live = sig.signal;
    const gen = sig.generation;
    sig.dispose();
    expect(live.aborted).toBe(true);
    expect(sig.isCurrent(gen)).toBe(false);
    expect(sig.renew().aborted).toBe(true);
    expect(sig.isCurrent(sig.generation)).toBe(false);
  });
});

describe("DiskVersionLoop", () => {
  function harness(whileHidden: "defer" | "run") {
    const loads: { signal: AbortSignal; d: ReturnType<typeof deferred<string>> }[] = [];
    const applied: string[] = [];
    const failed: unknown[] = [];
    const discarded: string[] = [];
    const loop = new DiskVersionLoop<string>({
      load: (signal) => {
        const d = deferred<string>();
        loads.push({ signal, d });
        return d.promise;
      },
      apply: (v) => applied.push(v),
      fail: (e) => failed.push(e),
      discard: (v) => discarded.push(v),
      whileHidden,
    });
    return { loop, loads, applied, failed, discarded };
  }

  it("loads on open and skips the token's first arrival", async () => {
    const h = harness("run");
    h.loop.token(null);
    expect(h.loads).toHaveLength(1);
    h.loop.token("m1");
    expect(h.loads).toHaveLength(1);
    h.loop.token("m1");
    expect(h.loads).toHaveLength(1);
    h.loads[0].d.resolve("v1");
    await flush();
    expect(h.applied).toEqual(["v1"]);
    h.loop.token("m2");
    expect(h.loads).toHaveLength(2);
  });

  it("drops a superseded generation and aborts its load", async () => {
    const h = harness("run");
    h.loop.token("m1");
    h.loop.token("m2");
    expect(h.loads[0].signal.aborted).toBe(true);
    h.loads[1].d.resolve("v2");
    h.loads[0].d.resolve("v1");
    await flush();
    expect(h.applied).toEqual(["v2"]);
    expect(h.discarded).toEqual(["v1"]);
  });

  it("never reports a stale failure", async () => {
    const h = harness("run");
    h.loop.token("m1");
    h.loop.token("m2");
    h.loads[0].d.reject(new Error("aborted"));
    await flush();
    expect(h.failed).toEqual([]);
    h.loads[1].d.reject(new Error("gone"));
    await flush();
    expect(h.failed).toHaveLength(1);
  });

  it("defers a parked change and applies it once on reveal", async () => {
    const h = harness("defer");
    h.loop.token("m1");
    h.loads[0].d.resolve("v1");
    await flush();
    h.loop.setVisible(false);
    h.loop.token("m2");
    h.loads[1].d.resolve("v2");
    await flush();
    expect(h.applied).toEqual(["v1"]);
    // A second change while parked replaces the held one.
    h.loop.token("m3");
    h.loads[2].d.resolve("v3");
    await flush();
    expect(h.applied).toEqual(["v1"]);
    expect(h.discarded).toEqual(["v2"]);
    h.loop.setVisible(true);
    expect(h.applied).toEqual(["v1", "v3"]);
    h.loop.setVisible(false);
    h.loop.setVisible(true);
    expect(h.applied).toEqual(["v1", "v3"]);
  });

  it("applies the open even while parked", async () => {
    const h = harness("defer");
    h.loop.setVisible(false);
    h.loop.token(null);
    h.loads[0].d.resolve("v1");
    await flush();
    expect(h.applied).toEqual(["v1"]);
  });

  it("runs a parked change at once in run mode", async () => {
    const h = harness("run");
    h.loop.token("m1");
    h.loads[0].d.resolve("v1");
    await flush();
    h.loop.setVisible(false);
    h.loop.token("m2");
    h.loads[1].d.resolve("v2");
    await flush();
    expect(h.applied).toEqual(["v1", "v2"]);
  });

  it("drops everything on dispose", async () => {
    const h = harness("defer");
    h.loop.token("m1");
    h.loads[0].d.resolve("v1");
    await flush();
    h.loop.setVisible(false);
    h.loop.token("m2");
    h.loads[1].d.resolve("v2");
    await flush();
    h.loop.token("m3");
    h.loop.dispose();
    expect(h.loads[2].signal.aborted).toBe(true);
    expect(h.discarded).toEqual(["v2"]);
    h.loads[2].d.resolve("v3");
    await flush();
    h.loop.setVisible(true);
    expect(h.applied).toEqual(["v1"]);
    expect(h.discarded).toEqual(["v2", "v3"]);
  });
});

describe("FileClaim", () => {
  it("balances refs when the path switches with a load in flight", async () => {
    const refs = new Map<string, number>();
    const pending = new Map<string, ReturnType<typeof deferred<void>>>();
    const claim = new FileClaim<{ path: string }>(
      (p) => {
        refs.set(p, (refs.get(p) ?? 0) + 1);
        pending.set(p, deferred<void>());
        return { path: p };
      },
      (p) => refs.set(p, (refs.get(p) ?? 0) - 1),
    );
    const a = claim.claim("a.ipynb");
    claim.claim("b.ipynb");
    // a's load lands after the switch: the claim no longer counts it.
    pending.get("a.ipynb")?.resolve();
    await flush();
    expect(refs.get("a.ipynb")).toBe(0);
    expect(refs.get("b.ipynb")).toBe(1);
    expect(entryFor(a, "b.ipynb")).toBeNull();
    claim.free();
    claim.free();
    expect(refs.get("b.ipynb")).toBe(0);
  });
});

describe("RevealQueue", () => {
  it("runs at once when shown, else the latest on reveal", () => {
    const q = new RevealQueue();
    const ran: number[] = [];
    q.run(() => ran.push(1));
    q.setShown(false);
    q.run(() => ran.push(2));
    q.run(() => ran.push(3));
    expect(ran).toEqual([1]);
    q.setShown(true);
    expect(ran).toEqual([1, 3]);
    q.setShown(false);
    q.run(() => ran.push(4));
    q.clear();
    q.setShown(true);
    expect(ran).toEqual([1, 3]);
  });
});

describe("watchPaneVisible", () => {
  class FakeObserver {
    static last: FakeObserver | null = null;
    disconnected = false;
    constructor(readonly fire: () => void) {
      FakeObserver.last = this;
    }
    observe(): void {}
    disconnect(): void {
      this.disconnected = true;
    }
  }

  it("follows the layer's inert flag", () => {
    const layer = { inert: false };
    const node = { closest: () => layer } as unknown as Element;
    const seen: boolean[] = [];
    const stop = watchPaneVisible(node, (v) => seen.push(v), FakeObserver);
    layer.inert = true;
    FakeObserver.last?.fire();
    layer.inert = false;
    FakeObserver.last?.fire();
    expect(seen).toEqual([true, false, true]);
    stop();
    expect(FakeObserver.last?.disconnected).toBe(true);
  });

  it("is visible outside a pane layer", () => {
    FakeObserver.last = null;
    const node = { closest: () => null } as unknown as Element;
    const seen: boolean[] = [];
    watchPaneVisible(node, (v) => seen.push(v), FakeObserver);
    expect(seen).toEqual([true]);
    expect(FakeObserver.last).toBeNull();
  });
});

describe("OwnedUrls", () => {
  it("revokes what it owns and what a library minted into its caches", () => {
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
    const urls = new OwnedUrls();
    urls.own("blob:a");
    const cache = urls.cache();
    cache.set("media/1.png", "blob:b");
    urls.revokeAll();
    expect(revoke.mock.calls.map((c) => c[0])).toEqual(["blob:a", "blob:b"]);
    expect(cache.size).toBe(0);
    urls.revokeAll();
    expect(revoke).toHaveBeenCalledTimes(2);
    revoke.mockRestore();
  });
});
