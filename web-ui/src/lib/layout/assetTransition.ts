import { writable } from "svelte/store";

export const BUILD_META_NAME = "chimaera-build";
export const BUILD_META_PLACEHOLDER = "__CHIMAERA_BUILD_ID__";

export type AssetTransitionReason = "build" | "connection" | "chunk";

export interface AssetTransition {
  reason: AssetTransitionReason;
  /** A fresh loopback origin, or null for a same-origin reload. */
  target: string | null;
  /** Build/connection changes navigate automatically once state is safe.
   *  Chunk failures wait for the user to retry the view or request reload. */
  requested: boolean;
  /** Explicit acknowledgement that volatile local state may be discarded. */
  forced: boolean;
  /** Monotonic request identity; one navigation attempt per revision. */
  revision: number;
}

export const assetTransition = writable<AssetTransition | null>(null);

/** Source identity of a build id, matching chimaera_core::builds_match. */
export function buildSource(build: string | null | undefined): string | null {
  if (build === null || build === undefined || build.length === 0) return null;
  const dot = build.lastIndexOf(".");
  const source = dot > 0 ? build.slice(0, dot) : build;
  // Source-less builds may contain different bytes, so only their complete
  // build ids match. This mirrors the daemon's conservative unknown policy.
  return source.startsWith("unknown") ? build : source;
}

/** Read the source build stamped into the entry document by the daemon. */
export function documentBuildSource(content: string | null | undefined): string | null {
  if (content === BUILD_META_PLACEHOLDER) return null; // Vite dev server
  return buildSource(content);
}

function rank(reason: AssetTransitionReason): number {
  switch (reason) {
    case "build":
      return 3;
    case "connection":
      return 2;
    case "chunk":
      return 1;
  }
}

/** Queue a daemon navigation. Build changes outrank connection moves, which
 *  outrank a generic chunk failure; a more precise reason is never hidden. */
export function requireAssetNavigation(
  reason: Exclude<AssetTransitionReason, "chunk">,
  target: string | null,
): void {
  assetTransition.update((current) => {
    const nextReason =
      current !== null && rank(current.reason) > rank(reason) ? current.reason : reason;
    const nextTarget = target ?? current?.target ?? null;
    if (
      current !== null &&
      current.reason === nextReason &&
      current.target === nextTarget &&
      current.requested
    ) {
      return current;
    }
    return {
      reason: nextReason,
      target: nextTarget,
      requested: true,
      forced: false,
      revision: (current?.revision ?? 0) + 1,
    };
  });
}

/** Surface any Vite dynamic-import failure, including nested preview chunks
 *  that never pass through Pane's top-level loader. */
export function noteChunkFailure(): void {
  assetTransition.update((current) => {
    if (current !== null && rank(current.reason) > rank("chunk")) return current;
    if (current?.reason === "chunk") return current;
    return {
      reason: "chunk",
      target: null,
      requested: false,
      forced: false,
      revision: (current?.revision ?? 0) + 1,
    };
  });
}

/** Request the pending reload. `force` is an explicit user choice to cross a
 *  volatile-state guard; the browser's dirty-file confirmation remains too. */
export function requestAssetReload(force = false): void {
  assetTransition.update((current) => ({
    reason: current?.reason ?? "chunk",
    target: current?.target ?? null,
    requested: true,
    forced: force,
    revision: (current?.revision ?? 0) + 1,
  }));
}

/** A navigation call returned and this document is still alive, which means
 *  beforeunload was cancelled. Drop the one-shot force and mint a revision so
 *  the normal safety gate waits for dirty state to clear before trying again. */
export function rearmAssetNavigation(cancelledRevision: number): void {
  assetTransition.update((current) => {
    if (
      current === null ||
      !current.requested ||
      current.revision !== cancelledRevision
    ) {
      return current;
    }
    return {
      ...current,
      forced: false,
      revision: current.revision + 1,
    };
  });
}

/** A transient failure may be retried in place. Build/connection transitions
 *  are authoritative and cannot be dismissed. */
export function clearChunkFailure(): void {
  assetTransition.update((current) => (current?.reason === "chunk" ? null : current));
}
