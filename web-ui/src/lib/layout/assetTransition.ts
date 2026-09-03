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

export type AssetNavigation =
  | { kind: "reload" }
  | { kind: "replace"; target: string }
  | { kind: "replace-and-reload"; target: string };

export const assetTransition = writable<AssetTransition | null>(null);

/**
 * Choose a navigation primitive that always replaces the loaded document.
 *
 * `location.replace()` treats a same-origin URL whose only difference is its
 * hash as an in-document navigation. SSH recovery commonly reuses the loopback
 * port while rotating the token in that hash, so explicitly update the URL and
 * reload in that case.
 */
export function planAssetNavigation(
  currentHref: string,
  target: string | null,
): AssetNavigation {
  if (target === null) return { kind: "reload" };
  const current = new URL(currentHref);
  const next = new URL(target, current);
  const sameDocument =
    current.origin === next.origin &&
    current.pathname === next.pathname &&
    current.search === next.search;
  return sameDocument
    ? { kind: "replace-and-reload", target: next.href }
    : { kind: "replace", target: next.href };
}

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

/** Delay before an unforced navigation attempt that left this document alive
 *  is tried again. Navigation is asynchronous, so the document outliving the
 *  call proves nothing by itself; only after this long is the attempt treated
 *  as failed. It must comfortably exceed a remote entry-document round trip:
 *  WebKit cancels the in-flight load for every re-issued request, and a
 *  zero-delay re-arm once starved a remote window forever. */
export const ASSET_RETRY_BASE_MS = 5_000;
export const ASSET_RETRY_MAX_MS = 30_000;

/** Backoff for the n-th (1-based) unforced attempt: 5s, 10s, 20s, then 30s. */
export function assetNavigationRetryMs(attempt: number): number {
  const doubled = ASSET_RETRY_BASE_MS * 2 ** Math.max(0, attempt - 1);
  return Math.min(doubled, ASSET_RETRY_MAX_MS);
}

/** This document outlived a navigation attempt at `attemptedRevision`: a
 *  forced attempt met the beforeunload prompt (Stay keeps the document; Leave
 *  lets it go regardless), or an unforced load failed and the browser kept the
 *  old page. Drop the one-shot force and mint a revision so the safety gate
 *  re-evaluates — dirty state holds the next attempt, a clear state retries. */
export function rearmAssetNavigation(attemptedRevision: number): void {
  assetTransition.update((current) => {
    if (
      current === null ||
      !current.requested ||
      current.revision !== attemptedRevision
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
