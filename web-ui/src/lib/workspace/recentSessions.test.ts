import { afterEach, expect, it, vi } from "vitest";
import { RecentSessions } from "./recentSessions";

afterEach(() => vi.useRealTimers());

it("prunes a fast-exiting session even when no later snapshot arrives", () => {
  vi.useFakeTimers();
  let live = new Set<string>();
  const recent = new RecentSessions(() => { live = new Set(); recent.protect(live); });
  recent.add("install");
  recent.protect(live);
  expect(live.has("install")).toBe(true);
  vi.advanceTimersByTime(10_000);
  expect(live.size).toBe(0);
  recent.dispose();
});

it("an explicit close removes protection without waiting for its grace", () => {
  vi.useFakeTimers();
  const recent = new RecentSessions(vi.fn());
  recent.add("closed");
  recent.add("other");
  recent.delete("closed");
  const live = new Set<string>();
  recent.protect(live);
  expect([...live]).toEqual(["other"]);
  recent.dispose();
  expect(vi.getTimerCount()).toBe(0);
});
