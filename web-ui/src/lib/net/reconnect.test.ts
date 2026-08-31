import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  HIDDEN_GRACE_ATTEMPTS,
  HIDDEN_RETRY_MS,
  NUDGE_DAMP_MS,
  NUDGE_SPREAD_MS,
  Reconnector,
  reconnectingSockets,
  nudgeReconnectors,
  retryDelayMs,
} from "./reconnect";

/**
 * The nudge damper's `lastNudgeAt` is module state, while `vi.useFakeTimers()`
 * resets the mocked clock to the real time each test — so a prior test's
 * nudge can sit at or past a fresh clock. Jump strictly beyond any clock a
 * previous test can have reached: the jump grows per call, and each test's
 * own intra-test advances are far smaller than one increment (nothing is
 * pending at the jump, so it is free).
 */
let dampBudgetCalls = 0;
function freshDampBudget(): void {
  dampBudgetCalls += 1;
  vi.advanceTimersByTime(NUDGE_DAMP_MS * 100 * dampBudgetCalls);
}

describe("retryDelayMs", () => {
  it("jitters ±20% around the backoff while visible", () => {
    expect(retryDelayMs(1000, false, 1, () => 0)).toBe(800);
    expect(retryDelayMs(1000, false, 1, () => 0.5)).toBe(1000);
    expect(retryDelayMs(1000, false, 1, () => 1)).toBe(1200);
  });

  it("grace attempts keep the fast backoff even while hidden", () => {
    for (let attempt = 1; attempt <= HIDDEN_GRACE_ATTEMPTS; attempt += 1) {
      expect(retryDelayMs(500, true, attempt, () => 0.5)).toBe(500);
    }
  });

  it("floors the delay at the slow tier from the attempt after grace", () => {
    const attempt = HIDDEN_GRACE_ATTEMPTS + 1;
    expect(retryDelayMs(500, true, attempt, () => 0.5)).toBe(HIDDEN_RETRY_MS);
    // Jitter still applies to the floored value (no lockstep on return).
    expect(retryDelayMs(500, true, attempt, () => 0)).toBe(HIDDEN_RETRY_MS * 0.8);
    expect(retryDelayMs(500, true, attempt, () => 1)).toBe(HIDDEN_RETRY_MS * 1.2);
  });

  it("leaves a backoff already above the floor alone (jitter only)", () => {
    expect(retryDelayMs(120_000, true, HIDDEN_GRACE_ATTEMPTS + 3, () => 0.5)).toBe(120_000);
  });

  it("never floors while visible, whatever the attempt", () => {
    expect(retryDelayMs(500, false, 50, () => 0.5)).toBe(500);
  });
});

describe("Reconnector", () => {
  // These run without a `document`, so schedule() sees hidden=false — the
  // visible fast path (first delay = 500ms ± 20%). The hidden plumbing gets
  // its own document-stub suite below. Each nudge test starts by buying
  // fresh damp budget (see freshDampBudget).
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("retries after the (jittered) backoff and counts one reconnecting socket", () => {
    let retries = 0;
    const r = new Reconnector(() => {
      retries += 1;
    });
    r.schedule();
    expect(get(reconnectingSockets)).toBe(1);
    vi.advanceTimersByTime(600); // 500ms + the 20% jitter ceiling
    expect(retries).toBe(1);
    r.succeeded();
    expect(get(reconnectingSockets)).toBe(0);
  });

  it("a nudge is deferred, spread, and fires the pending retry exactly once", () => {
    freshDampBudget();
    let retries = 0;
    const r = new Reconnector(() => {
      retries += 1;
    });
    r.schedule();
    nudgeReconnectors(() => 1); // spread slot = NUDGE_SPREAD_MS exactly
    // Deferred: nothing fires synchronously in the caller's stack.
    expect(retries).toBe(0);
    vi.advanceTimersByTime(NUDGE_SPREAD_MS);
    expect(retries).toBe(1);
    // The original timer was replaced, not doubled: no second fire.
    vi.advanceTimersByTime(HIDDEN_RETRY_MS * 2);
    expect(retries).toBe(1);
    r.cancel();
    r.clear();
  });

  it("nudges are damped: a second herd within NUDGE_DAMP_MS is a no-op", () => {
    freshDampBudget();
    let retries = 0;
    const r = new Reconnector(() => {
      retries += 1;
    });
    r.schedule();
    nudgeReconnectors(() => 0); // spread slot 0 — fires on the next tick
    vi.advanceTimersByTime(0);
    expect(retries).toBe(1);
    r.schedule(); // the nudged attempt failed; re-armed at backoff
    nudgeReconnectors(() => 0); // within the damp window: ignored
    vi.advanceTimersByTime(0);
    expect(retries).toBe(1); // still waiting out its own backoff
    vi.advanceTimersByTime(1300); // attempt 2 backoff 1000ms + jitter ceiling
    expect(retries).toBe(2);
    r.cancel();
    r.clear();
  });

  it("a nudge landing mid-attempt marks the socket: the failure retries immediately", () => {
    freshDampBudget();
    let retries = 0;
    const r = new Reconnector(() => {
      retries += 1;
    });
    r.schedule();
    vi.advanceTimersByTime(600); // the retry fires — attempt now in flight
    expect(retries).toBe(1);
    nudgeReconnectors(() => 0); // no timer pending: marks, doesn't fire
    vi.advanceTimersByTime(NUDGE_SPREAD_MS);
    expect(retries).toBe(1);
    r.schedule(); // the in-flight attempt failed
    vi.advanceTimersByTime(0); // nudged: re-armed at 0, not at the backoff
    expect(retries).toBe(2);
    r.cancel();
    r.clear();
  });

  it("after cancel()+clear() (permanent close) a nudge cannot revive the socket", () => {
    freshDampBudget();
    let retries = 0;
    const r = new Reconnector(() => {
      retries += 1;
    });
    r.schedule();
    r.cancel();
    r.clear();
    nudgeReconnectors(() => 0);
    vi.advanceTimersByTime(HIDDEN_RETRY_MS * 2);
    expect(retries).toBe(0);
  });
});

describe("Reconnector under a hidden document", () => {
  // Minimal document stand-in so documentHidden() and the module's
  // visibilitychange listener (armed on first schedule) both engage.
  let visibility: "visible" | "hidden";
  let listeners: Array<() => void>;

  beforeEach(() => {
    vi.useFakeTimers();
    visibility = "hidden";
    listeners = [];
    (globalThis as Record<string, unknown>).document = {
      get visibilityState() {
        return visibility;
      },
      addEventListener: (_type: string, fn: () => void) => {
        listeners.push(fn);
      },
      removeEventListener: (_type: string, fn: () => void) => {
        listeners = listeners.filter((l) => l !== fn);
      },
    };
  });

  afterEach(() => {
    vi.useRealTimers();
    delete (globalThis as Record<string, unknown>).document;
  });

  it("grace attempts stay fast, the floor lands after them, and visibility return nudges", () => {
    freshDampBudget();
    let retries = 0;
    const r = new Reconnector(() => {
      retries += 1;
    });
    // Attempts 1 + 2: grace — fast backoff even though hidden.
    r.schedule();
    vi.advanceTimersByTime(600); // 500 + jitter ceiling
    expect(retries).toBe(1);
    r.schedule();
    vi.advanceTimersByTime(1200); // 1000 + jitter ceiling
    expect(retries).toBe(2);
    // Attempt 3: the hidden floor engages (48-72s) — far beyond the fast max.
    r.schedule();
    vi.advanceTimersByTime(10_000);
    expect(retries).toBe(2);
    // The user looks back: the module's visibilitychange listener nudges the
    // pending retry into the spread window instead of waiting out the floor.
    visibility = "visible";
    for (const l of [...listeners]) l();
    vi.advanceTimersByTime(NUDGE_SPREAD_MS);
    expect(retries).toBe(3);
    r.cancel();
    r.clear();
  });
});
