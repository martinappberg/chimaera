import { afterEach, describe, expect, it, vi } from "vitest";

import { ASLEEP_NOTE } from "../net/api";
import { ownerAwake } from "../net/reconnect";
import { activateTimelineWorkspace, timelineStore } from "./timeline.svelte";

const reads = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../net/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../net/api")>()),
  api: reads.api,
}));

const json = (body: unknown, status = 200): Response => new Response(JSON.stringify(body), { status });
const page = { schema: 1, epoch: 1, entries: [{ seq: 1, ts: 1, kind: "episode", sid: "s-1", title: "fix QC" }], more: false };

afterEach(async () => {
  reads.api.mockReset();
  await activateTimelineWorkspace(null);
  vi.useRealTimers();
});

describe("a Timeline read while the project's owner sleeps", () => {
  it("is a quiet note, not an error, and reads again once the owner answers", async () => {
    vi.useFakeTimers();
    reads.api.mockResolvedValueOnce(json({ error: "worker_asleep" }, 409));
    await activateTimelineWorkspace("w-asleep");
    expect(timelineStore.note).toBe(ASLEEP_NOTE);
    expect(timelineStore.error).toBeNull();
    expect(timelineStore.entries).toEqual([]);
    // No timer runs against a sleeping owner.
    expect(vi.getTimerCount()).toBe(0);

    reads.api.mockResolvedValueOnce(json(page));
    ownerAwake(() => 0);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(reads.api).toHaveBeenCalledTimes(2);
    expect(timelineStore.note).toBeNull();
    expect(timelineStore.error).toBeNull();
    expect(timelineStore.entries.map((e) => e.title)).toEqual(["fix QC"]);
  });

  it("says a project that is not reachable yet the same quiet way", async () => {
    reads.api.mockResolvedValueOnce(json({ error: "project_unavailable" }, 503));
    await activateTimelineWorkspace("w-routed");
    expect(timelineStore.note).toBe("This project isn’t reachable right now.");
    expect(timelineStore.error).toBeNull();
  });

  it("asks a project that was unreachable again on its own, since nothing announces its return", async () => {
    vi.useFakeTimers();
    reads.api.mockResolvedValueOnce(json({ error: "project_unavailable" }, 503));
    await activateTimelineWorkspace("w-routed");
    expect(timelineStore.note).not.toBeNull();
    reads.api.mockResolvedValueOnce(json(page));
    await vi.advanceTimersByTimeAsync(30_000);
    expect(reads.api).toHaveBeenCalledTimes(2);
    expect(timelineStore.note).toBeNull();
    expect(timelineStore.entries).toHaveLength(1);
  });

  it("keeps a real failure an error", async () => {
    reads.api.mockResolvedValueOnce(json({ error: "disk on fire" }, 500));
    await activateTimelineWorkspace("w-broken");
    expect(timelineStore.error).toBe("disk on fire");
    expect(timelineStore.note).toBeNull();
  });
});
