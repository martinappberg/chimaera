import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PUT_DEBOUNCE_MS, SettingsWriter, type SavePhase } from "./settingsWriter";

/** One PUT the test answers by hand. */
interface Call {
  body: string;
  signal: AbortSignal;
  answer(status: number, error?: string): Promise<void>;
  fail(): Promise<void>;
}

function harness(opts: { hidden?: boolean } = {}) {
  let map = "{}";
  const calls: Call[] = [];
  const phases: SavePhase[] = [];
  // Declared before the writer so onChange can read it.
  let writer: SettingsWriter;
  writer = new SettingsWriter({
    snapshot: () => map,
    put: (body, signal) =>
      new Promise((resolve, reject) => {
        calls.push({
          body,
          signal,
          answer: async (status, error) => {
            resolve({ status, error });
            await vi.advanceTimersByTimeAsync(0);
          },
          fail: async () => {
            reject(new TypeError("network"));
            await vi.advanceTimersByTimeAsync(0);
          },
        });
      }),
    hidden: () => opts.hidden ?? false,
    onChange: () => phases.push(writer.phase),
  });
  const edit = (next: string) => {
    map = next;
    writer.edited();
  };
  return { writer, calls, phases, edit };
}

describe("SettingsWriter", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0.5);
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("debounces edits into one PUT of the latest map", async () => {
    const { writer, calls, edit } = harness();
    edit('{"a":1}');
    edit('{"a":2}');
    expect(writer.phase).toBe("pending");
    await vi.advanceTimersByTimeAsync(PUT_DEBOUNCE_MS);
    expect(calls.map((c) => c.body)).toEqual(['{"a":2}']);
    await calls[0].answer(204);
    expect(writer.phase).toBe("idle");
  });

  it("retries a failed write with backoff until it lands", async () => {
    const { writer, calls, edit } = harness();
    edit('{"a":1}');
    await vi.advanceTimersByTimeAsync(PUT_DEBOUNCE_MS);
    await calls[0].fail();
    expect(writer.phase).toBe("failed");
    await vi.advanceTimersByTimeAsync(999);
    expect(calls).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(calls).toHaveLength(2);
    await calls[1].answer(503);
    // Second failure doubles the delay.
    await vi.advanceTimersByTimeAsync(1_999);
    expect(calls).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(1);
    await calls[2].answer(204);
    expect(writer.phase).toBe("idle");
    expect(calls.map((c) => c.body)).toEqual(Array(3).fill('{"a":1}'));
  });

  it("slows the retry to the hidden tier once the grace attempts are spent", async () => {
    const { calls, edit } = harness({ hidden: true });
    edit('{"a":1}');
    await vi.advanceTimersByTimeAsync(PUT_DEBOUNCE_MS);
    await calls[0].fail();
    await vi.advanceTimersByTimeAsync(1_000);
    await calls[1].fail();
    await vi.advanceTimersByTimeAsync(2_000);
    await calls[2].fail();
    await vi.advanceTimersByTimeAsync(59_999);
    expect(calls).toHaveLength(3);
    await vi.advanceTimersByTimeAsync(1);
    expect(calls).toHaveLength(4);
  });

  it("never re-sends a refused map unprompted, and stops guarding it", async () => {
    const { writer, calls, edit } = harness();
    edit('{"a":1}');
    await vi.advanceTimersByTimeAsync(PUT_DEBOUNCE_MS);
    await calls[0].answer(413, "settings exceed 262144 bytes");
    expect(writer.phase).toBe("rejected");
    expect(writer.rejection).toBe("settings exceed 262144 bytes");
    // Frames may now replace the map, and they do not re-send it.
    expect(writer.dirty()).toBe(false);
    writer.remoteFrame();
    await vi.advanceTimersByTimeAsync(120_000);
    expect(calls).toHaveLength(1);
    // An edit (or an explicit flush) sends again.
    edit('{"a":2}');
    await vi.advanceTimersByTimeAsync(PUT_DEBOUNCE_MS);
    expect(calls.at(-1)?.body).toBe('{"a":2}');
    await calls[1].answer(204);
    expect(writer.phase).toBe("idle");
    expect(writer.rejection).toBe("");
  });

  it("resolves flush false when the write fails, true when it lands", async () => {
    const { writer, calls, edit } = harness();
    edit('{"a":1}');
    const first = writer.flush();
    await calls[0].fail();
    expect(await first).toBe(false);
    const second = writer.flush();
    await calls[1].answer(204);
    expect(await second).toBe(true);
  });

  it("treats a settings frame after a failure as reconnect and re-sends at once", async () => {
    const { writer, calls, edit } = harness();
    edit('{"a":1}');
    await vi.advanceTimersByTimeAsync(PUT_DEBOUNCE_MS);
    await calls[0].fail();
    writer.remoteFrame();
    expect(calls).toHaveLength(2);
    await calls[1].answer(204);
    expect(writer.phase).toBe("idle");
    // The cancelled backoff timer does not fire a stray PUT.
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toHaveLength(2);
  });

  it("ignores frames while a write is merely pending or in flight", async () => {
    const { writer, calls, edit } = harness();
    edit('{"a":1}');
    writer.remoteFrame();
    expect(calls).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(PUT_DEBOUNCE_MS);
    writer.remoteFrame();
    expect(calls).toHaveLength(1);
  });

  it("keeps an edit made during the PUT dirty and sends it next", async () => {
    const { writer, calls, edit } = harness();
    edit('{"a":1}');
    await vi.advanceTimersByTimeAsync(PUT_DEBOUNCE_MS);
    edit('{"a":2}');
    await vi.advanceTimersByTimeAsync(PUT_DEBOUNCE_MS);
    // One writer: the debounce does not start a second concurrent PUT.
    expect(calls).toHaveLength(1);
    await calls[0].answer(204);
    expect(writer.dirty()).toBe(true);
    expect(calls.map((c) => c.body)).toEqual(['{"a":1}', '{"a":2}']);
    await calls[1].answer(204);
    expect(writer.phase).toBe("idle");
  });

  it("supersedes an outstanding older PUT on flush and ignores its late answer", async () => {
    const { writer, calls, edit } = harness();
    edit('{"a":1}');
    await vi.advanceTimersByTimeAsync(PUT_DEBOUNCE_MS);
    edit('{"a":2}');
    // Page hide: the newer map cannot wait for the older request.
    const flushed = writer.flush();
    expect(calls[0].signal.aborted).toBe(true);
    expect(calls[1].body).toBe('{"a":2}');
    await calls[1].answer(204);
    expect(await flushed).toBe(true);
    expect(writer.phase).toBe("idle");
    // The aborted request answers late (as a failure or a success): no effect.
    await calls[0].fail();
    expect(writer.phase).toBe("idle");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toHaveLength(2);
  });

  it("joins an outstanding PUT of the same map instead of repeating it", async () => {
    const { writer, calls, edit } = harness();
    edit('{"a":1}');
    await vi.advanceTimersByTimeAsync(PUT_DEBOUNCE_MS);
    let settled = false;
    void writer.flush().then(() => (settled = true));
    expect(calls).toHaveLength(1);
    await calls[0].answer(204);
    expect(settled).toBe(true);
  });

  it("holds a superseded request's waiter until the newer body lands", async () => {
    const { writer, calls, edit } = harness();
    edit('{"a":1}');
    let older: boolean | null = null;
    // A caller waiting for {"a":1} to be held (GitView's git.path save).
    void writer.flush().then((ok) => (older = ok));
    edit('{"a":2}');
    void writer.flush();
    // The aborted older fetch rejects first: its waiter must not wake yet.
    await calls[0].fail();
    expect(older).toBeNull();
    await calls[1].answer(204);
    expect(older).toBe(true);
  });

  it("gives a superseded waiter the newer request's failure", async () => {
    const { writer, calls, edit } = harness();
    edit('{"a":1}');
    const older = writer.flush();
    edit('{"a":2}');
    void writer.flush();
    await calls[0].fail();
    await calls[1].fail();
    expect(await older).toBe(false);
  });

  it("flush with nothing unsaved sends nothing", async () => {
    const { writer, calls } = harness();
    expect(await writer.flush()).toBe(true);
    expect(calls).toHaveLength(0);
  });
});
