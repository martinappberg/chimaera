import { afterEach, describe, expect, it, vi } from "vitest";
import type { ClusterOverview } from "../net/native";
const native = vi.hoisted(() => ({ overview: vi.fn() }));
vi.mock("../net/native", () => ({ clusterOverview: native.overview }));
import { clusterOverviews } from "./clusterStore.svelte";
const overview = { now_ms: 1, jobs: [], workspaces: [] } as unknown as ClusterOverview;
afterEach(() => { clusterOverviews.forget("cluster"); native.overview.mockReset(); });
describe("cluster overview refresh", () => {
  it("asks the shell with the alias alone and honours the floor", async () => {
    native.overview.mockResolvedValue(overview);
    await clusterOverviews.refresh("cluster", 0);
    await clusterOverviews.refresh("cluster", 0);
    expect(native.overview.mock.calls).toEqual([["cluster"], ["cluster"]]);
    await clusterOverviews.refresh("cluster", 60_000);
    expect(native.overview).toHaveBeenCalledTimes(2);
  });
  it("drops an overtaken response and keeps the last good overview on failure", async () => {
    let finish!: (value: ClusterOverview) => void;
    native.overview.mockImplementationOnce(() => new Promise<ClusterOverview>((resolve) => { finish = resolve; }));
    const old = clusterOverviews.refresh("cluster", 0);
    const fresh = { ...overview, now_ms: 2 };
    native.overview.mockResolvedValueOnce(fresh);
    await clusterOverviews.refresh("cluster", 0);
    finish(overview); await old;
    expect(clusterOverviews.entry("cluster")?.overview).toEqual(fresh);
    native.overview.mockRejectedValueOnce(new Error("ssh failed"));
    await clusterOverviews.refresh("cluster", 0);
    expect(clusterOverviews.entry("cluster")).toMatchObject({ overview: fresh, error: "ssh failed", loading: false });
  });
});
