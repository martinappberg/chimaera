import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  HIDDEN_RETRY_MS,
  Reconnector,
  reconnectingSockets,
  retryDelayMs,
  retryWaitingNow,
} from "./reconnect";

describe("retryDelayMs", () => {
  it("jitters ±20% around the backoff while visible", () => {
    expect(retryDelayMs(1000, false, () => 0)).toBe(800);
    expect(retryDelayMs(1000, false, () => 0.5)).toBe(1000);
    expect(retryDelayMs(1000, false, () => 1)).toBe(1200);
  });

  it("floors the delay at the slow tier while hidden", () => {
    expect(retryDelayMs(500, true, () => 0.5)).toBe(HIDDEN_RETRY_MS);
    // Jitter still applies to the floored value (no lockstep on return).
    expect(retryDelayMs(500, true, () => 0)).toBe(HIDDEN_RETRY_MS * 0.8);
    expect(retryDelayMs(500, true, () => 1)).toBe(HIDDEN_RETRY_MS * 1.2);
  });

  it("leaves a backoff already above the floor alone (jitter only)", () => {
    expect(retryDelayMs(120_000, true, () => 0.5)).toBe(120_000);
  });
});

describe("Reconnector", () => {
  // The suite runs without a `document`, so schedule() sees hidden=false and
  // the visibility listener is never armed — the timing below is the visible
  // fast path: first delay = 500ms ± 20%.
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

  it("retryWaitingNow fires a pending retry immediately, exactly once", () => {
    let retries = 0;
    const r = new Reconnector(() => {
      retries += 1;
    });
    r.schedule();
    retryWaitingNow(); // the events-recovery / visibility-return nudge
    expect(retries).toBe(1);
    // The armed timer was cancelled: advancing must not double-fire.
    vi.advanceTimersByTime(HIDDEN_RETRY_MS * 2);
    expect(retries).toBe(1);
    // Nothing pending: the nudge is a no-op.
    retryWaitingNow();
    expect(retries).toBe(1);
    r.clear();
  });

  it("cancel() leaves the waiting set — no late nudge can revive it", () => {
    let retries = 0;
    const r = new Reconnector(() => {
      retries += 1;
    });
    r.schedule();
    r.cancel();
    retryWaitingNow();
    vi.advanceTimersByTime(HIDDEN_RETRY_MS * 2);
    expect(retries).toBe(0);
    r.clear();
  });
});
