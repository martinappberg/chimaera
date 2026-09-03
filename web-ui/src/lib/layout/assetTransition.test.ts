import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  ASSET_RETRY_BASE_MS,
  ASSET_RETRY_MAX_MS,
  assetNavigationRetryMs,
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
      revision: attempted!.revision + 1,
    });
    // A stale callback cannot perturb a newer transition.
    rearmAssetNavigation(attempted!.revision);
    expect(get(assetTransition)?.revision).toBe(attempted!.revision + 1);
  });

  it("retries an unforced navigation whose document is still alive", () => {
    requireAssetNavigation("build", "http://127.0.0.1:9801/#token=fresh");
    const attempted = get(assetTransition)!;
    expect(attempted.forced).toBe(false);

    // The load failed (or was cancelled) and the old page is still showing:
    // the same target is attempted again under a fresh revision.
    rearmAssetNavigation(attempted.revision);
    expect(get(assetTransition)).toMatchObject({
      reason: "build",
      target: "http://127.0.0.1:9801/#token=fresh",
      requested: true,
      forced: false,
      revision: attempted.revision + 1,
    });
    // Repeated health polls reporting the same build do not mint revisions,
    // so a slow reload is never re-issued underneath itself.
    requireAssetNavigation("build", null);
    expect(get(assetTransition)?.revision).toBe(attempted.revision + 1);
  });

  it("backs off retries from five seconds to a thirty-second ceiling", () => {
    expect(assetNavigationRetryMs(1)).toBe(ASSET_RETRY_BASE_MS);
    expect(assetNavigationRetryMs(2)).toBe(ASSET_RETRY_BASE_MS * 2);
    expect(assetNavigationRetryMs(3)).toBe(ASSET_RETRY_BASE_MS * 4);
    expect(assetNavigationRetryMs(4)).toBe(ASSET_RETRY_MAX_MS);
    expect(assetNavigationRetryMs(40)).toBe(ASSET_RETRY_MAX_MS);
    // A retry must never race a remote round trip: the first delay alone
    // exceeds any sane entry-document fetch.
    expect(ASSET_RETRY_BASE_MS).toBeGreaterThanOrEqual(5_000);
  });
});
