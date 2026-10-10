import { afterEach, describe, expect, it, vi } from "vitest";
const calls = vi.hoisted(() => ({ api: vi.fn(), capture: vi.fn(() => ({ current: () => true })) }));
vi.mock("../net/api", () => ({ api: calls.api, captureApiGuard: calls.capture, isRemoteHost: () => false }));
import { fetchKept, type KeptReview } from "./kept";
import { KeptReviews } from "./keptReviews.svelte";
const review = { workspace_id: "w-one", files: 0, total: 0, pairs: [], branches: [], returned_at: null, unlisted: 0, here: true } as KeptReview;
afterEach(() => { vi.clearAllMocks(); });
describe("kept original route publication", () => {
  it("does not deliver a JSON body after the captured owner retires", async () => {
    let current = true, finish!: (value: KeptReview) => void;
    const guard = { owner: { current: () => current }, current: () => current };
    calls.api.mockResolvedValueOnce({ ok: true, json: () => new Promise((resolve) => { finish = resolve; }) });
    const request = fetchKept("w-one", undefined, guard); await Promise.resolve();
    current = false; finish(review); await expect(request).rejects.toMatchObject({ code: "not_here" });
  });
  it("split panes coalesce only a fresh list for the same captured route owner", async () => {
    const store = new KeptReviews(); const owner = { current: () => true };
    const first = { owner, current: () => true }, second = { owner, current: () => true };
    let finish!: (value: Response) => void;
    calls.api.mockImplementationOnce(() => new Promise<Response>((resolve) => { finish = resolve; }));
    const a = store.refresh("w-one", first), b = store.refresh("w-one", second);
    finish(Response.json(review)); await Promise.all([a, b]);
    expect(calls.api).toHaveBeenCalledTimes(1); expect(store.byWorkspace["w-one"]).toEqual(review);
  });
  it("retains the predecessor slot and requires a fresh successor read without stale publication", async () => {
    const store = new KeptReviews(); let current = true, finish!: (value: Response) => void;
    const old = { owner: { current: () => current }, current: () => current }, fresh = { owner: { current: () => true }, current: () => true };
    calls.api.mockImplementationOnce(() => new Promise<Response>((resolve) => { finish = resolve; })).mockResolvedValueOnce(Response.json(review));
    const first = store.refresh("w-one", old); current = false;
    await expect(store.refresh("w-one", fresh)).rejects.toThrow("settling"); expect(calls.api).toHaveBeenCalledTimes(1);
    finish(Response.json({ ...review, files: 7 })); await expect(first).rejects.toThrow();
    expect(store.byWorkspace["w-one"]).toBeUndefined();
    await store.refresh("w-one", fresh); expect(calls.api).toHaveBeenCalledTimes(2); expect(store.byWorkspace["w-one"]).toEqual(review);
  });
});
