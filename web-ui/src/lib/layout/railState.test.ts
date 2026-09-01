import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defaultRailChrome, loadRailChrome, RAIL_MAX, saveRailChrome } from "./railState";
import { windowKey } from "./viewState";

/** The Storage surface railState touches: get/set/remove plus the
 *  length/key(i) enumeration pruning walks (sessionStorage comes from
 *  vitest.setup.ts; localStorage is stubbed per test). */
class MemoryStorage {
  private values = new Map<string, string>();

  get length(): number {
    return this.values.size;
  }

  key(index: number): string | null {
    return [...this.values.keys()][index] ?? null;
  }

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }

  removeItem(key: string): void {
    this.values.delete(key);
  }

  keys(): string[] {
    return [...this.values.keys()];
  }
}

/** A store whose next `refusals` writes throw, like a quota-full localStorage. */
class FlakyStorage extends MemoryStorage {
  refusals = 0;

  override setItem(key: string, value: string): void {
    if (this.refusals > 0) {
      this.refusals -= 1;
      throw new Error("QuotaExceededError");
    }
    super.setItem(key, value);
  }
}

const PREFIX = "chimaera.rail.";
const chrome = { width: 300, filesOpen: true, filesFrac: 0.4 };

describe("rail chrome persistence", () => {
  let storage: MemoryStorage;

  beforeEach(() => {
    storage = new MemoryStorage();
    vi.stubGlobal("localStorage", storage);
  });

  afterEach(() => vi.unstubAllGlobals());

  it("round-trips this window's chrome (clamped on read) and stamps the write", () => {
    saveRailChrome({ width: 9999, filesOpen: false, filesFrac: 0.5 });
    const raw = JSON.parse(storage.getItem(PREFIX + windowKey()) ?? "null") as {
      savedAt?: unknown;
    };
    expect(typeof raw.savedAt).toBe("number");
    expect(loadRailChrome()).toEqual({ width: RAIL_MAX, filesOpen: false, filesFrac: 0.5 });
  });

  it("keeps only the newest windows' records, treating unstamped ones as oldest", () => {
    // 20 other windows: 19 stamped (t=1..19) plus one legacy record without a
    // stamp. Every tab mints its own window key, so these are what a
    // long-lived profile accumulates.
    for (let i = 1; i <= 19; i++) {
      storage.setItem(`${PREFIX}w${i}`, JSON.stringify({ ...chrome, savedAt: i }));
    }
    storage.setItem(`${PREFIX}legacy`, JSON.stringify(chrome));
    storage.setItem("chimaera.other", "untouched");

    saveRailChrome(chrome);

    const railKeys = storage.keys().filter((k) => k.startsWith(PREFIX));
    expect(railKeys).toHaveLength(16);
    expect(railKeys).toContain(PREFIX + windowKey());
    // Evicted: the legacy record (no stamp = oldest) and the four oldest stamps.
    for (const gone of ["legacy", "w1", "w2", "w3", "w4"]) {
      expect(storage.getItem(PREFIX + gone)).toBeNull();
    }
    expect(storage.getItem(`${PREFIX}w5`)).not.toBeNull();
    expect(storage.getItem(`${PREFIX}w19`)).not.toBeNull();
    // Foreign keys are never touched.
    expect(storage.getItem("chimaera.other")).toBe("untouched");
  });

  it("leaves a within-budget set alone", () => {
    for (let i = 1; i <= 10; i++) {
      storage.setItem(`${PREFIX}w${i}`, JSON.stringify({ ...chrome, savedAt: i }));
    }
    saveRailChrome(chrome);
    expect(storage.keys().filter((k) => k.startsWith(PREFIX))).toHaveLength(11);
  });

  it("prunes before a refused write, then sheds the rest and retries once", () => {
    // A full store throws on setItem: the bound must hold anyway (the prune
    // runs first), and the quota path sheds every other record and retries.
    const flaky = new FlakyStorage();
    vi.stubGlobal("localStorage", flaky);
    for (let i = 1; i <= 20; i++) {
      flaky.setItem(`${PREFIX}w${i}`, JSON.stringify({ ...chrome, savedAt: i }));
    }
    flaky.setItem("chimaera.other", "untouched");
    flaky.refusals = 1;

    saveRailChrome(chrome);

    const railKeys = flaky.keys().filter((k) => k.startsWith(PREFIX));
    expect(railKeys.length).toBeLessThanOrEqual(16);
    expect(railKeys).toEqual([PREFIX + windowKey()]);
    expect(flaky.refusals).toBe(0);
    expect(flaky.getItem("chimaera.other")).toBe("untouched");
  });

  it("stays bounded when every write is refused", () => {
    const flaky = new FlakyStorage();
    vi.stubGlobal("localStorage", flaky);
    for (let i = 1; i <= 20; i++) {
      flaky.setItem(`${PREFIX}w${i}`, JSON.stringify({ ...chrome, savedAt: i }));
    }
    flaky.refusals = Number.POSITIVE_INFINITY;
    expect(() => saveRailChrome(chrome)).not.toThrow();
    expect(flaky.keys().filter((k) => k.startsWith(PREFIX)).length).toBeLessThanOrEqual(16);
  });

  it("never throws when storage is unavailable", () => {
    vi.stubGlobal("localStorage", {
      length: 0,
      key: () => null,
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
      removeItem: () => {},
    });
    expect(() => saveRailChrome(chrome)).not.toThrow();
    expect(loadRailChrome()).toEqual(defaultRailChrome());
  });
});
