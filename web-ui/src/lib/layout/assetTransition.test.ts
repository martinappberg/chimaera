import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  assetTransition,
  BUILD_META_PLACEHOLDER,
  buildSource,
  clearChunkFailure,
  documentBuildSource,
  noteChunkFailure,
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
});
