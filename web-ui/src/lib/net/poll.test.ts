import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  healthPollDelayMs,
  POLL_FAST_MS,
  POLL_SLOW_MS,
  sessionsPollDelayMs,
  startVisibilityPoll,
} from "./poll";

describe("poll cadence selection", () => {
  it("health is a slow safety net while the events socket is up", () => {
    expect(healthPollDelayMs(true, false)).toBe(POLL_SLOW_MS);
    expect(healthPollDelayMs(true, true)).toBe(POLL_SLOW_MS);
  });

  it("health is the fast recovery probe only in a visible window", () => {
    expect(healthPollDelayMs(false, false)).toBe(POLL_FAST_MS);
    expect(healthPollDelayMs(false, true)).toBe(POLL_SLOW_MS);
  });

  it("the sessions fallback probes fast only while visible", () => {
    expect(sessionsPollDelayMs(false)).toBe(POLL_FAST_MS);
    expect(sessionsPollDelayMs(true)).toBe(POLL_SLOW_MS);
  });
});

describe("startVisibilityPoll", () => {
  // Minimal document stand-in (the suite runs in node, no jsdom): visibility
  // state + the visibilitychange listener list the poller registers on.
  let visibility: "visible" | "hidden";
  let listeners: Array<() => void>;

  const fireVisibility = (state: "visible" | "hidden"): void => {
    visibility = state;
    for (const l of [...listeners]) l();
  };

  beforeEach(() => {
    vi.useFakeTimers();
    visibility = "visible";
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

  it("ticks immediately, then at the returned delay", async () => {
    let ticks = 0;
    const stop = startVisibilityPoll(
      () => {
        ticks += 1;
      },
      () => 5000,
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(ticks).toBe(1);
    await vi.advanceTimersByTimeAsync(5000);
    expect(ticks).toBe(2);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(ticks).toBe(4);
    stop();
  });

  it("re-evaluates the delay per arm (tier changes take effect)", async () => {
    const seen: boolean[] = [];
    const stop = startVisibilityPoll(
      () => {},
      (hidden) => {
        seen.push(hidden);
        return hidden ? 60_000 : 5000;
      },
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(seen).toEqual([false]);
    visibility = "hidden"; // no event: the next arm still sees the new state
    await vi.advanceTimersByTimeAsync(5000);
    expect(seen).toEqual([false, true]);
    stop();
  });

  it("catches up immediately on visibility return instead of waiting out the slow tier", async () => {
    let ticks = 0;
    const stop = startVisibilityPoll(
      () => {
        ticks += 1;
      },
      (hidden) => (hidden ? 60_000 : 5000),
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(ticks).toBe(1);
    fireVisibility("hidden");
    // The delay armed BEFORE hiding still fires (tiers apply at the next
    // arm), and that tick re-arms at the slow tier.
    await vi.advanceTimersByTimeAsync(5000);
    expect(ticks).toBe(2);
    // 10s into the 60s slow-tier delay: nothing…
    await vi.advanceTimersByTimeAsync(10_000);
    expect(ticks).toBe(2);
    // …until the document returns, which ticks NOW.
    fireVisibility("visible");
    await vi.advanceTimersByTimeAsync(0);
    expect(ticks).toBe(3);
    stop();
  });

  it("stop() cancels the timer and unhooks the listener", async () => {
    let ticks = 0;
    const stop = startVisibilityPoll(
      () => {
        ticks += 1;
      },
      () => 5000,
    );
    await vi.advanceTimersByTimeAsync(0);
    stop();
    expect(listeners).toEqual([]);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(ticks).toBe(1);
  });
});
