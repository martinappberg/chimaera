import { describe, expect, it, vi } from "vitest";
// The IO adapter is not exercised against a daemon by these domain tests.
vi.mock("../pro/kept", () => ({ fetchKeptFile: vi.fn(), resolveKept: vi.fn(), resolveAllKept: vi.fn() }));
vi.mock("../pro/keptReviews.svelte", () => ({ keptReviews: { byWorkspace: {}, refresh: vi.fn(), set: vi.fn() } }));
import { bindKeptReview, keptReviewProjection, type KeptFile, type KeptReview, type KeptReviewBackend, type KeptViewSnapshot } from "./keptReview";
const original: KeptReview = {
  workspace_id: "workspace-one", files: 1, total: 1, returned_at: 1, unlisted: 0, here: true, trash: true, branches: [],
  pairs: [{ path: "notes.md", mine_path: "notes.md.mine-20261003-1010", size: 3, mine_size: 4, changed_at: 2, mine_changed_at: 3 }],
};
const file: KeptFile = { path: original.pairs[0].path, mine_path: original.pairs[0].mine_path,
  mine: { size: 4, changed_at: 3, text: "mine" }, cloud: { size: 3, changed_at: 2, text: "new" } };
function deferred<T>() { let resolve!: (v: T) => void; let reject!: (e: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
function fixture(requireFresh = false) {
  let observer: ((v: KeptReview | null | undefined) => void) | null = null;
  let current = true; let snapshot!: KeptViewSnapshot;
  const stop = vi.fn();
  const answer = { ...original, files: 0, pairs: [], returned_at: null };
  const backend: KeptReviewBackend = {
    observe: vi.fn(() => ({ subscribe(send: (value: KeptReview | null | undefined) => void) { observer = send; send(original); return stop; } })),
    refresh: vi.fn(async () => {}), read: vi.fn(async () => file), choose: vi.fn(async () => answer),
    chooseAll: vi.fn(async () => answer), set: vi.fn(),
  };
  const domain = bindKeptReview({ workspaceId: original.workspace_id, current: () => current, requireFresh, hereName: "this Mac",
    editor: { fontSize: 13, lineHeight: 1.5, tabSize: 2, lineNumbers: true } }, backend);
  const unsubscribe = domain.subscribe((value) => { snapshot = value; });
  return { backend, domain, stop, unsubscribe, snapshot: () => snapshot,
    publish(value: KeptReview) { observer!(value); }, retire() { current = false; } };
}
describe("original kept review host domain", () => {
  it("closes projections and refuses malformed or duplicate pair paths", () => {
    const value = { ...original, extra: "host-only", pairs: [{ ...original.pairs[0], extra: "host-only" }] };
    const projected = keptReviewProjection(value, original.workspace_id);
    expect(projected).toEqual(original); expect(Object.isFrozen(projected.pairs[0])).toBe(true);
    for (const bad of [{ ...original, workspace_id: "successor" }, { ...original, pairs: [{ ...original.pairs[0], mine_path: "../outside" }] },
      { ...original, files: 2, pairs: [original.pairs[0], original.pairs[0]] }, { ...original, unlisted: 1 }]) {
      expect(() => keptReviewProjection(bad, original.workspace_id)).toThrow();
    }
  });
  it("reads and chooses only the original listed workspace, publishing to the same host backend", async () => {
    const f = fixture(); const at = f.snapshot().revision;
    const result = await f.domain.read(file.mine_path, at, new AbortController().signal);
    expect(result).toEqual(file); expect(f.backend.read).toHaveBeenCalledWith("workspace-one", file.mine_path, expect.any(AbortSignal));
    await expect(f.domain.choose("unlisted", "use_mine", at)).rejects.toMatchObject({ code: "changed" });
    const answer = await f.domain.choose(file.mine_path, "keep_both", at);
    expect(f.backend.choose).toHaveBeenCalledTimes(1); expect(f.backend.set).toHaveBeenCalledWith("workspace-one", answer);
    expect(f.snapshot().review?.files).toBe(0); f.domain.dispose(); expect(f.stop).toHaveBeenCalledTimes(1);
  });
  it("refuses stale, copied and replayed all-pairs confirmations before any mutation", async () => {
    const f = fixture(); const confirmation = f.domain.confirmAll("use_cloud", f.snapshot().revision);
    await expect(f.domain.chooseAll({ ...confirmation })).rejects.toMatchObject({ code: "changed" });
    f.publish({ ...original, trash: false });
    await expect(f.domain.chooseAll(confirmation)).rejects.toMatchObject({ code: "changed" });
    expect(f.backend.chooseAll).not.toHaveBeenCalled();
    const fresh = f.domain.confirmAll("use_cloud", f.snapshot().revision);
    await f.domain.chooseAll(fresh); await expect(f.domain.chooseAll(fresh)).rejects.toMatchObject({ code: "changed" });
    expect(f.backend.chooseAll).toHaveBeenCalledTimes(1); f.domain.dispose();
  });
  it("a recovered owner cannot choose cached pairs before its successful fresh list", async () => {
    const f = fixture(true);
    expect(f.snapshot().review).toBeUndefined();
    expect(() => f.domain.confirmAll("keep_both", f.snapshot().revision)).toThrow();
    f.backend.refresh = vi.fn().mockRejectedValueOnce(new Error("refused")).mockResolvedValueOnce(undefined);
    await expect(f.domain.refresh()).rejects.toThrow(); expect(f.snapshot().review).toBeUndefined();
    await f.domain.refresh(); const confirmation = f.domain.confirmAll("keep_both", f.snapshot().revision);
    await f.domain.chooseAll(confirmation); expect(f.backend.chooseAll).toHaveBeenCalledTimes(1); f.domain.dispose();
  });
  it("explicit Refresh retries a failed read even when listed metadata is unchanged", async () => {
    const f = fixture(); const at = f.snapshot().revision;
    f.backend.read = vi.fn().mockRejectedValueOnce(new Error("temporary read failure")).mockResolvedValueOnce(file);
    await expect(f.domain.read(file.mine_path, at, new AbortController().signal)).rejects.toThrow("temporary read failure");
    await f.domain.refresh(); expect(f.snapshot().revision).toBeGreaterThan(at);
    expect(await f.domain.read(file.mine_path, f.snapshot().revision, new AbortController().signal)).toEqual(file);
    expect(f.backend.read).toHaveBeenCalledTimes(2); expect(f.backend.choose).not.toHaveBeenCalled();
    expect(f.backend.chooseAll).not.toHaveBeenCalled(); f.domain.dispose();
  });
  it("Done preserves unnamed copies with one exact keep-both acknowledgment", async () => {
    const f = fixture(); f.publish({ ...original, files: 2, total: 2, pairs: [], unlisted: 2 });
    const at = f.snapshot().revision;
    for (const choice of ["use_cloud", "use_mine"] as const) {
      expect(() => f.domain.confirmAll(choice, at)).toThrow();
    }
    const done = f.domain.confirmAll("keep_both", at);
    expect(done.count).toBe(0);
    await f.domain.chooseAll(done);
    expect(f.backend.chooseAll).toHaveBeenCalledWith("workspace-one", "keep_both");
    expect(f.backend.chooseAll).toHaveBeenCalledTimes(1); expect(f.backend.read).not.toHaveBeenCalled();
    await expect(f.domain.chooseAll(done)).rejects.toMatchObject({ code: "changed" });
    f.domain.dispose();
  });
  it("rejects changed content metadata and late read results after retirement", async () => {
    const f = fixture(); const at = f.snapshot().revision;
    f.backend.read = vi.fn(async () => ({ ...file, mine: { ...file.mine, changed_at: 4 } }));
    await expect(f.domain.read(file.mine_path, at, new AbortController().signal)).rejects.toMatchObject({ code: "changed" });
    expect(f.snapshot()).toMatchObject({ review: null, error: "invalid" });
    await expect(f.domain.choose(file.mine_path, "use_mine", at)).rejects.toMatchObject({ code: "changed" });
    expect(f.backend.choose).not.toHaveBeenCalled(); f.publish(original);
    expect(f.snapshot().review).toBeNull(); await f.domain.refresh();
    const wait = deferred<KeptFile>(); f.backend.read = vi.fn(() => wait.promise);
    const running = f.domain.read(file.mine_path, f.snapshot().revision, new AbortController().signal); f.retire(); wait.resolve(file);
    await expect(running).rejects.toMatchObject({ code: "retired" }); f.domain.dispose();
  });
  it("retains read quota until aborted IO settles and never publishes a late mutation", async () => {
    const f = fixture(); const at = f.snapshot().revision; const wait = deferred<KeptFile>();
    f.backend.read = vi.fn(() => wait.promise); const abort = new AbortController();
    const one = f.domain.read(file.mine_path, at, abort.signal); const two = f.domain.read(file.mine_path, at, abort.signal); abort.abort();
    await expect(f.domain.read(file.mine_path, at, new AbortController().signal)).rejects.toMatchObject({ code: "busy" });
    wait.resolve(file); await expect(one).rejects.toMatchObject({ code: "retired" }); await expect(two).rejects.toMatchObject({ code: "retired" });
    const mutation = deferred<KeptReview>(); f.backend.choose = vi.fn(() => mutation.promise);
    const choosing = f.domain.choose(file.mine_path, "use_mine", at); f.retire(); mutation.resolve(original);
    await expect(choosing).rejects.toMatchObject({ code: "retired" }); expect(f.backend.set).not.toHaveBeenCalled();
    expect(f.backend.choose).toHaveBeenCalledTimes(1); f.domain.dispose();
  });
  it("lost mutation reply consumes the original confirmation once without replay", async () => {
    const f = fixture(); f.backend.chooseAll = vi.fn(async () => { throw new Error("reply lost"); });
    const confirmation = f.domain.confirmAll("use_cloud", f.snapshot().revision);
    await expect(f.domain.chooseAll(confirmation)).rejects.toThrow("reply lost");
    await expect(f.domain.chooseAll(confirmation)).rejects.toMatchObject({ code: "changed" });
    expect(f.backend.chooseAll).toHaveBeenCalledTimes(1); expect(f.backend.set).not.toHaveBeenCalled(); f.domain.dispose();
  });
  it("malformed updates fence choices without escaping the host observer", async () => {
    const f = fixture(); expect(() => f.publish({ ...original, workspace_id: "successor" })).not.toThrow();
    expect(f.snapshot()).toMatchObject({ review: null, error: "invalid" });
    await expect(f.domain.choose(file.mine_path, "use_mine", f.snapshot().revision)).rejects.toMatchObject({ code: "changed" });
    expect(f.backend.choose).not.toHaveBeenCalled(); f.domain.dispose(); f.unsubscribe(); expect(f.stop).toHaveBeenCalledTimes(1);
  });
  it("unsubscribes partial listener failures and refuses known unavailable mine choices", async () => {
    const f = fixture(); f.publish({ ...original, pairs: [{ ...original.pairs[0], can_use_mine: false }] });
    await expect(f.domain.choose(file.mine_path, "use_mine", f.snapshot().revision)).rejects.toMatchObject({ code: "invalid" });
    expect(() => f.domain.subscribe(() => { throw new Error("presentation failed"); })).toThrow("presentation failed");
    expect(f.backend.choose).not.toHaveBeenCalled(); f.unsubscribe(); expect(f.stop).toHaveBeenCalledTimes(1); f.domain.dispose();
  });
});
