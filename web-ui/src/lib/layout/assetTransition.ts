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
  /** Navigation attempts at this transition that left the document alive —
   *  the load failed, or the safety gate held. Drives the retry backoff and
   *  the notice's retry state; a new reason or target starts over. */
  attempts: number;
}

export type AssetNavigation =
  | { kind: "reload" }
  | { kind: "replace"; target: string }
  | { kind: "replace-and-reload"; target: string };

export const assetTransition = writable<AssetTransition | null>(null);

/** Revisions never repeat within a document, even across a cleared chunk
 *  failure: App remembers the last revision it navigated for, and a fresh
 *  transition re-minting that number would be silently swallowed. */
let lastRevision = 0;
function nextRevision(): number {
  return ++lastRevision;
}

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
 *  outrank a generic chunk failure; a more precise reason is never hidden.
 *  Re-reporting the same reason and target (every health poll, every repeated
 *  "connected" event) mints nothing, so a navigation in flight is never
 *  re-issued underneath itself. */
export function requireAssetNavigation(
  reason: Exclude<AssetTransitionReason, "chunk">,
  target: string | null,
): void {
  assetTransition.update((current) => {
    const nextReason =
      current !== null && rank(current.reason) > rank(reason) ? current.reason : reason;
    const nextTarget = target ?? current?.target ?? null;
    const sameIdentity =
      current !== null && current.reason === nextReason && current.target === nextTarget;
    if (sameIdentity && current.requested) return current;
    return {
      reason: nextReason,
      target: nextTarget,
      requested: true,
      forced: false,
      revision: nextRevision(),
      attempts: sameIdentity ? current.attempts : 0,
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
      revision: nextRevision(),
      attempts: 0,
    };
  });
}

/** Request the pending reload. `force` is an explicit user choice to cross a
 *  volatile-state guard; the browser's dirty-file confirmation remains too.
 *  A manual re-request keeps the attempt count: it is one more try at the
 *  same transition, not a fresh one. */
export function requestAssetReload(force = false): void {
  assetTransition.update((current) => ({
    reason: current?.reason ?? "chunk",
    target: current?.target ?? null,
    requested: true,
    forced: force,
    revision: nextRevision(),
    attempts: current?.attempts ?? 0,
  }));
}

/** Retry schedule for an attempt that left the document alive: 10s, 20s,
 *  then 30s. Navigation is asynchronous — the document, timers included, runs
 *  on until the new one commits — so a surviving document proves nothing
 *  until the delay has passed, and every re-issue cancels the load in flight.
 *  The first step must clear a slow entry document on a loaded login node:
 *  the target answered the shell's health probe moments before, so an
 *  outright failed load is the rare case, a slow one is not. */
const RETRY_BASE_MS = 10_000;
const RETRY_MAX_MS = 30_000;

/** Delay before the n-th (1-based) retry of a transition. */
export function assetNavigationRetryMs(attempt: number): number {
  return Math.min(RETRY_BASE_MS * 2 ** Math.max(0, attempt - 1), RETRY_MAX_MS);
}

/** When to re-arm after issuing a navigation. `prompted` is a forced attempt
 *  with dirty files — the only kind that meets a beforeunload prompt. It hands
 *  back to the safety gate at once: whether the user keeps the document or
 *  lets it go, dirty state holds any next attempt, so re-arming before the
 *  prompt is even answered loses nothing. Nothing else can cancel an attempt
 *  from inside this document; one still here after the backoff has failed. */
export function assetRearmDelayMs(prompted: boolean, attempts: number): number {
  return prompted ? 0 : assetNavigationRetryMs(attempts + 1);
}

/** This document outlived the navigation attempt at `attemptedRevision`.
 *  Mint a revision so the safety gate re-evaluates — dirty state holds the
 *  next attempt, a clear state retries — and count the attempt. The one-shot
 *  force is dropped unless the caller knows no prompt could have consumed it
 *  (`keepForce`: a forced attempt blocked only by chat drafts), in which case
 *  the user's acknowledgement carries into the retry. A stale revision is a
 *  no-op. */
export function rearmAssetNavigation(attemptedRevision: number, keepForce = false): void {
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
      forced: keepForce && current.forced,
      revision: nextRevision(),
      attempts: current.attempts + 1,
    };
  });
}

/** A transient failure may be retried in place. Build/connection transitions
 *  are authoritative and cannot be dismissed. */
export function clearChunkFailure(): void {
  assetTransition.update((current) => (current?.reason === "chunk" ? null : current));
}
