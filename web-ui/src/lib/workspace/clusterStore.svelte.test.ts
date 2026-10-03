import { afterEach, describe, expect, it, vi } from "vitest";
import type { ClusterOverview } from "../net/native";
const native = vi.hoisted(() => ({ overview: vi.fn() }));
vi.mock("../net/native", () => ({ clusterOverview: native.overview }));
import { clusterOverviews } from "./clusterStore.svelte";
const overview = { now_ms: 1, jobs: [], workspaces: [] } as unknown as ClusterOverview;
afterEach(() => { clusterOverviews.forget("cluster"); native.overview.mockReset(); });
describe("cluster refresh intent", () => {
  it("keeps polls and action reloads passive; only a deliberate refresh queries live resources", async () => {
    native.overview.mockResolvedValue(overview);
    await clusterOverviews.refresh("cluster", 0);
    await clusterOverviews.refresh("cluster", 0, true);
    await clusterOverviews.refresh("cluster", 0);
    expect(native.overview.mock.calls).toEqual([["cluster", false], ["cluster", true], ["cluster", false]]);
    await clusterOverviews.refresh("cluster", 60_000);
    expect(native.overview).toHaveBeenCalledTimes(3);
  });
  it("drops an old poll after a deliberate refresh and preserves the last good overview on refusal", async () => {
    let finish!: (value: ClusterOverview) => void;
    native.overview.mockImplementationOnce(() => new Promise<ClusterOverview>((resolve) => { finish = resolve; }));
    const old = clusterOverviews.refresh("cluster", 0);
    const fresh = { ...overview, now_ms: 2 };
    native.overview.mockResolvedValueOnce(fresh);
    await clusterOverviews.refresh("cluster", 0, true);
    finish(overview); await old;
    expect(clusterOverviews.entry("cluster")?.overview).toEqual(fresh);
    native.overview.mockRejectedValueOnce(new Error("Connect required"));
    await clusterOverviews.refresh("cluster", 0, true);
    expect(clusterOverviews.entry("cluster")).toMatchObject({ overview: fresh, error: "Connect required", loading: false });
  });
});
