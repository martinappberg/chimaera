import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  assetNavigationRetryMs,
  assetRearmDelayMs,
  assetTransition,
  BUILD_META_PLACEHOLDER,
  buildSource,
  clearChunkFailure,
  documentBuildSource,
  noteChunkFailure,
  planAssetNavigation,
  rearmAssetNavigation,
  requestAssetReload,
  requireAssetNavigation,
} from "./assetTransition";

describe("asset transition identity", () => {
  afterEach(() => assetTransition.set(null));

  it("matches source builds while keeping unknown builds exact", () => {
    expect(buildSource("abc1234.100")).toBe("abc1234");
    expect(buildSource("abc1234.200")).toBe("abc1234");
    expect(buildSource("unknown.100")).toBe("unknown.100");
    expect(documentBuildSource(BUILD_META_PLACEHOLDER)).toBeNull();
  });

  it("reloads after replacing a rotated token on the same loopback origin", () => {
    expect(
      planAssetNavigation(
        "http://127.0.0.1:9800/#token=stale",
        "http://127.0.0.1:9800/#token=fresh",
      ),
    ).toEqual({
      kind: "replace-and-reload",
      target: "http://127.0.0.1:9800/#token=fresh",
    });
    expect(
      planAssetNavigation(
        "http://127.0.0.1:9800/#token=stale",
        "http://127.0.0.1:9801/#token=fresh",
      ),
    ).toEqual({
      kind: "replace",
      target: "http://127.0.0.1:9801/#token=fresh",
    });
  });

  it("keeps the strongest reason and the freshest navigation target", () => {
    noteChunkFailure();
    requireAssetNavigation("build", null);
    requireAssetNavigation("connection", "http://127.0.0.1:9800/#token=fresh");
    expect(get(assetTransition)).toMatchObject({
      reason: "build",
      target: "http://127.0.0.1:9800/#token=fresh",
      requested: true,
      forced: false,
      attempts: 0,
    });
  });

  it("lets transient failures retry or explicitly cross the safety guard", () => {
    noteChunkFailure();
    expect(get(assetTransition)?.requested).toBe(false);
    requestAssetReload(true);
    expect(get(assetTransition)).toMatchObject({ requested: true, forced: true });
    clearChunkFailure();
    expect(get(assetTransition)).toBeNull();
  });

  it("re-arms a forced navigation when beforeunload keeps the document alive", () => {
    requireAssetNavigation("build", null);
    requestAssetReload(true);
    const attempted = get(assetTransition);
    expect(attempted?.forced).toBe(true);

    rearmAssetNavigation(attempted!.revision);
    expect(get(assetTransition)).toMatchObject({
      requested: true,
      forced: false,
      attempts: 1,
    });
    expect(get(assetTransition)!.revision).toBeGreaterThan(attempted!.revision);
    // A stale callback cannot perturb a newer transition.
    const rearmed = get(assetTransition)!;
    rearmAssetNavigation(attempted!.revision);
    expect(get(assetTransition)).toBe(rearmed);
  });

  it("carries the force into a retry when no prompt could have consumed it", () => {
    requireAssetNavigation("connection", "http://127.0.0.1:9801/#token=fresh");
    requestAssetReload(true);
    const attempted = get(assetTransition)!;

    // Blocked only by chat drafts: no beforeunload handler exists, so a
    // surviving document means the load failed — the user's acknowledgement
    // still stands for the retry.
    rearmAssetNavigation(attempted.revision, true);
    expect(get(assetTransition)).toMatchObject({ forced: true, attempts: 1 });
    // A manual "reload now" keeps counting the same transition.
    requestAssetReload(true);
    expect(get(assetTransition)).toMatchObject({ forced: true, attempts: 1 });
  });

  it("retries an unforced navigation whose document is still alive", () => {
    requireAssetNavigation("build", "http://127.0.0.1:9801/#token=fresh");
    const attempted = get(assetTransition)!;
    expect(attempted.forced).toBe(false);

    // The load failed (or was cancelled) and the old page is still showing:
    // the same target is attempted again under a fresh revision.
    rearmAssetNavigation(attempted.revision);
    const retried = get(assetTransition)!;
    expect(retried).toMatchObject({
      reason: "build",
      target: "http://127.0.0.1:9801/#token=fresh",
      requested: true,
      forced: false,
      attempts: 1,
    });
    expect(retried.revision).toBeGreaterThan(attempted.revision);
    // Repeated health polls and "connected" events reporting the same
    // transition mint nothing, so a slow load is never re-issued underneath
    // itself.
    requireAssetNavigation("build", null);
    requireAssetNavigation("build", "http://127.0.0.1:9801/#token=fresh");
    expect(get(assetTransition)).toBe(retried);
    // A moved target is a new transition: its backoff starts over.
    requireAssetNavigation("connection", "http://127.0.0.1:9802/#token=fresh");
    expect(get(assetTransition)).toMatchObject({ reason: "build", attempts: 0 });
  });

  it("never re-mints a revision, even across a cleared chunk failure", () => {
    noteChunkFailure();
    requestAssetReload();
    const handled = get(assetTransition)!.revision;
    clearChunkFailure();
    noteChunkFailure();
    requestAssetReload();
    expect(get(assetTransition)!.revision).toBeGreaterThan(handled);
  });

  it("schedules 10s, 20s, then 30s retries and re-arms a prompted attempt at once", () => {
    expect([1, 2, 3, 4, 40].map(assetNavigationRetryMs)).toEqual([
      10_000, 20_000, 30_000, 30_000, 30_000,
    ]);
    expect(assetRearmDelayMs(true, 0)).toBe(0);
    expect(assetRearmDelayMs(true, 7)).toBe(0);
    expect(assetRearmDelayMs(false, 0)).toBe(10_000);
    expect(assetRearmDelayMs(false, 1)).toBe(20_000);
    expect(assetRearmDelayMs(false, 9)).toBe(30_000);
  });
});
