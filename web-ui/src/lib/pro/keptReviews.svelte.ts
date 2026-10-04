/**
 * What each project's last return kept in both versions, as far as this
 * window knows — shared by the chat's "back" line and the review pane, so a
 * choice made in the pane settles the chat line at once. Read on demand (a
 * chat of the project mounts, the review opens), again when a `kept_both`
 * notice arrives, and replaced by every choice's answer; never polled.
 *
 * A project the daemon cannot answer for (an older daemon, a browser view of
 * the project, which reaches only its own routes) reads as nothing kept.
 */

import { fetchKept, type KeptReview } from "./kept";
import { captureApiGuard, type ApiGuard } from "../net/api";

export class KeptReviews {
  /** Per project: its last answer; null = nothing to review or no answer. */
  byWorkspace = $state<Record<string, KeptReview | null>>({});
  #inflight = new Map<string, { promise: Promise<void>; guard?: ApiGuard }>();

  /** Read the project's answer once (a later `refresh` reads it again). */
  ensure(workspaceId: string): void {
    if (workspaceId in this.byWorkspace || this.#inflight.has(workspaceId)) return;
    void this.refresh(workspaceId);
  }

  refresh(workspaceId: string, guard?: ApiGuard): Promise<void> {
    const running = this.#inflight.get(workspaceId);
    if (running !== undefined) {
      // Keep one retained request slot. A successor cannot adopt a predecessor
      // answer as a fresh read, or bypass its cleanup by creating another map.
      if (guard !== undefined) {
        if (running.guard === undefined || running.guard.owner !== guard.owner || running.guard.placement !== guard.placement) {
          return Promise.reject(new Error("Kept refresh still settling"));
        }
        return running.promise.then(() => { if (!guard.current()) throw new Error("Kept route retired"); });
      }
      return running.promise;
    }
    const original = guard ?? captureApiGuard();
    let entry!: { promise: Promise<void>; guard?: ApiGuard };
    const request = fetchKept(workspaceId, guard === undefined ? undefined : AbortSignal.timeout(4000), guard)
      .then((review) => {
        if (!original.current()) throw new Error("Kept route retired");
        this.set(workspaceId, review);
      })
      .catch((error: unknown) => {
        if (guard !== undefined) throw error;
        // Ordinary readers retain known data; a stale predecessor never
        // clears a recovered owner's store or publishes an initial null.
        if (original.current() && !(workspaceId in this.byWorkspace)) this.set(workspaceId, null);
      })
      .finally(() => { if (this.#inflight.get(workspaceId) === entry) this.#inflight.delete(workspaceId); });
    entry = { promise: request, ...(guard === undefined ? {} : { guard }) };
    this.#inflight.set(workspaceId, entry);
    return request;
  }

  set(workspaceId: string, review: KeptReview | null): void {
    this.byWorkspace = { ...this.byWorkspace, [workspaceId]: review };
  }
}

export const keptReviews = new KeptReviews();

/** The review is still worth opening: something waits for a choice. */
export function waitingReview(review: KeptReview | null | undefined): review is KeptReview {
  return review !== null && review !== undefined && review.files > 0 && review.returned_at !== null;
}

/** Ask the workbench to open (or focus) a project's review. App listens and
 *  switches this window to the project first when it shows another one. */
export function requestKeptReview(workspaceId: string): void {
  window.dispatchEvent(new CustomEvent("chimaera:kept-review", { detail: workspaceId }));
}
