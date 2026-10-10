import { describe, expect, it } from "vitest";

import { parseSnapshot, secondsLeft, type ComputeSelf } from "./compute";

const self = (over: Partial<ComputeSelf> = {}): ComputeSelf => ({
  job_id: "4242",
  node: "n042",
  partition: "batch",
  state: "RUNNING",
  time_left: "7:20",
  cpus: "8",
  mem: "64G",
  gres: "",
  ...over,
});

describe("secondsLeft", () => {
  it("counts every window to the daemon's one end, whenever it received it", () => {
    const end = 1_000_000;
    // Two windows into the same job: one received a snapshot whose
    // time_left was a minute old, the other a fresh one. Both agree.
    const stale = self({ time_left: "8:20", ends_at_ms: end });
    const fresh = self({ time_left: "7:20", ends_at_ms: end });
    const now = end - 400_000;
    expect(secondsLeft(stale, now - 59_000, now)).toBe(400);
    expect(secondsLeft(fresh, now, now)).toBe(400);
    expect(secondsLeft(fresh, now, end + 5_000)).toBe(0);
  });

  it("falls back to the receive-time countdown for an older daemon", () => {
    const received = 2_000_000;
    expect(secondsLeft(self(), received, received + 20_000)).toBe(420);
    expect(secondsLeft(self({ time_left: "UNLIMITED" }), received, received)).toBeNull();
  });
});

describe("parseSnapshot self block", () => {
  it("keeps the end and the attached flag when the daemon sends them", () => {
    const snap = parseSnapshot({
      scheduler: "slurm",
      self: { ...self(), ends_at_ms: 1_234, attached: true },
    });
    expect(snap.self?.ends_at_ms).toBe(1_234);
    expect(snap.self?.attached).toBe(true);
  });

  it("omits them when an older daemon doesn't", () => {
    const snap = parseSnapshot({ scheduler: "slurm", self: self() });
    expect(snap.self?.ends_at_ms).toBeUndefined();
    expect(snap.self?.attached).toBeUndefined();
  });
});
