import { describe, expect, it } from "vitest";
import { CooperativeQueue } from "./cooperativeQueue";

function rig<T>(consume: (item: T) => void, maxItems = 2) {
  const scheduled: Array<() => void> = [];
  const cancelled = new Set<() => void>();
  const queue = new CooperativeQueue(consume, {
    maxItems,
    budgetMs: 100,
    now: () => 0,
    schedule: (run) => {
      scheduled.push(run);
      return () => cancelled.add(run);
    },
  });
  const runNext = () => {
    const next = scheduled.shift();
    if (next !== undefined && !cancelled.has(next)) next();
  };
  return { queue, scheduled, runNext };
}

describe("CooperativeQueue", () => {
  it("preserves FIFO order while yielding between bounded slices", () => {
    const seen: number[] = [];
    const { queue, scheduled, runNext } = rig<number>((item) => seen.push(item));
    queue.pushMany([1, 2, 3, 4, 5]);

    expect(scheduled).toHaveLength(1);
    runNext();
    expect(seen).toEqual([1, 2]);
    expect(scheduled).toHaveLength(1);
    runNext();
    expect(seen).toEqual([1, 2, 3, 4]);
    runNext();
    expect(seen).toEqual([1, 2, 3, 4, 5]);
  });

  it("accepts work appended while a slice is draining", () => {
    const seen: number[] = [];
    let queue!: CooperativeQueue<number>;
    const rigged = rig<number>((item) => {
      seen.push(item);
      if (item === 1) queue.pushMany([3, 4]);
    }, 8);
    queue = rigged.queue;
    queue.pushMany([1, 2]);
    rigged.runNext();
    expect(seen).toEqual([1, 2, 3, 4]);
  });

  it("cancels queued work on clear", () => {
    const seen: number[] = [];
    const { queue, runNext } = rig<number>((item) => seen.push(item));
    queue.pushMany([1, 2, 3]);
    queue.clear();
    runNext();
    expect(seen).toEqual([]);
  });

  it("stops cleanly when a consumer clears during a slice", () => {
    const seen: number[] = [];
    let queue!: CooperativeQueue<number>;
    const rigged = rig<number>((item) => {
      seen.push(item);
      if (item === 1) queue.clear();
    });
    queue = rigged.queue;
    queue.pushMany([1, 2, 3]);

    rigged.runNext();
    expect(seen).toEqual([1]);
    expect(rigged.scheduled).toHaveLength(0);
  });
});
