import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { haltReconnects, nudgeReconnectors, Reconnector, reconnectingSockets } from "./reconnect";

// Its own file: the halt is module state for the rest of the page's life.
describe("haltReconnects", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("cancels a pending retry, clears the indicator, and no later drop or nudge retries", () => {
    const retry = vi.fn();
    const r = new Reconnector(retry);
    r.schedule();
    expect(get(reconnectingSockets)).toBe(1);
    haltReconnects();
    expect(get(reconnectingSockets)).toBe(0);
    vi.advanceTimersByTime(60_000);
    expect(retry).not.toHaveBeenCalled();

    const later = vi.fn();
    const other = new Reconnector(later);
    other.schedule();
    nudgeReconnectors(() => 0);
    vi.advanceTimersByTime(60_000);
    expect(later).not.toHaveBeenCalled();
    expect(get(reconnectingSockets)).toBe(0);
  });
});
