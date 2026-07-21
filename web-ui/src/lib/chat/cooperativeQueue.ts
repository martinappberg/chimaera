/**
 * A small FIFO that yields between bounded slices of synchronous work.
 * WebSocket replay can arrive much faster than Svelte can reduce and render a
 * large transcript; running every event in the socket callback monopolizes the
 * browser task queue and makes tab clicks look dead. This queue preserves wire
 * order while returning control to input/layout between slices.
 */

interface QueueOptions {
  maxItems?: number;
  budgetMs?: number;
  now?: () => number;
  schedule?: (run: () => void) => () => void;
}

const defaultSchedule = (run: () => void): (() => void) => {
  const timer = globalThis.setTimeout(run, 0);
  return () => globalThis.clearTimeout(timer);
};

export class CooperativeQueue<T> {
  private readonly items: T[] = [];
  private cursor = 0;
  private cancelScheduled: (() => void) | null = null;
  private readonly maxItems: number;
  private readonly budgetMs: number;
  private readonly now: () => number;
  private readonly schedule: (run: () => void) => () => void;

  constructor(
    private readonly consume: (item: T) => void,
    options: QueueOptions = {},
  ) {
    this.maxItems = Math.max(1, options.maxItems ?? 48);
    this.budgetMs = Math.max(1, options.budgetMs ?? 6);
    this.now = options.now ?? (() => performance.now());
    this.schedule = options.schedule ?? defaultSchedule;
  }

  push(item: T): void {
    this.items.push(item);
    this.ensureScheduled();
  }

  pushMany(items: readonly T[]): void {
    if (items.length === 0) return;
    this.items.push(...items);
    this.ensureScheduled();
  }

  /** Drop queued work and cancel the next slice (used by deliberate teardown). */
  clear(): void {
    this.cancelScheduled?.();
    this.cancelScheduled = null;
    this.items.length = 0;
    this.cursor = 0;
  }

  private ensureScheduled(): void {
    if (this.cancelScheduled !== null) return;
    this.cancelScheduled = this.schedule(() => this.drain());
  }

  private drain(): void {
    this.cancelScheduled = null;
    const started = this.now();
    let consumed = 0;
    while (this.cursor < this.items.length && consumed < this.maxItems) {
      const item = this.items[this.cursor];
      // Advance before calling out: a consumer may deliberately clear the
      // queue during teardown. Advancing afterward would resurrect cursor=1
      // over an empty array and reschedule empty slices forever.
      this.cursor += 1;
      consumed += 1;
      this.consume(item);
      if (this.now() - started >= this.budgetMs) break;
    }

    if (this.cursor === this.items.length) {
      this.items.length = 0;
      this.cursor = 0;
      return;
    }

    // Avoid retaining an arbitrarily large processed prefix during a long
    // replay. The remaining suffix stays in exact FIFO order.
    if (this.cursor >= 1024 && this.cursor * 2 >= this.items.length) {
      this.items.splice(0, this.cursor);
      this.cursor = 0;
    }
    this.ensureScheduled();
  }
}
