import { derived, readable, toStore } from "svelte/store";
import { accountPlan, accountSignedOut, proOffered } from "../net/plan";
import { pageVisible } from "../shared/visibility";
import { keptReviews } from "../pro/keptReviews.svelte";
import { relativeFilePath, type ApplicationRuntime, type HostSurfaceActions } from "./application";

/** One dormant projection of the existing window services, never a new client. */
export const keptApplicationRuntime: ApplicationRuntime = Object.freeze({
  version: 1 as const,
  account: derived([accountPlan, accountSignedOut, proOffered], ([plan, signedOut, offered]) =>
    Object.freeze({ version: 1 as const, available: offered === true, plan, signedOut })),
  visibility: pageVisible,
  kept: { observe: (workspaceId: string) => toStore(() => {
    const review = keptReviews.byWorkspace[workspaceId];
    if (review === undefined || review === null || !Number.isSafeInteger(review.files) || review.files < 0 ||
      (review.returned_at !== null && (!Number.isSafeInteger(review.returned_at) || review.returned_at < 0 || review.returned_at > 8_640_000_000_000_000))) return null;
    return Object.freeze({ version: 1 as const,
      workspaceId, pendingFiles: review.files,
      returnedAt: review.returned_at === null ? null : new Date(review.returned_at).toISOString() });
  }) },
  // A kept-only presentation has no onboarding completion authority.
  onboarding: readable(null),
});

export interface KeptHostSnapshot {
  paneId: string; workspaceId: string; root: string; tab: object;
  route: { current(): boolean };
}
export interface KeptHostCallbacks {
  openFile(paneId: string, absolute: string): void;
  openFolder(paneId: string, absolute: string): void;
  close(tab: object, paneId: string): void;
  modal(node: HTMLElement): { destroy(): void };
}
/** Tab object identity is intentional: close/reopen of v:kept must not inherit
 * the old actions even if every visible selector is unchanged. */
export function captureKeptHost(original: KeptHostSnapshot, read: () => KeptHostSnapshot | null,
  callbacks: KeptHostCallbacks): { actions: HostSurfaceActions; openFolder(): void } {
  if (!original.root.startsWith("/") || original.root.includes("\0") || original.root.length > 4096) {
    throw new Error("Project folder unavailable");
  }
  const current = () => {
    const now = read();
    return now !== null && now.tab === original.tab && now.paneId === original.paneId &&
      now.workspaceId === original.workspaceId && now.root === original.root && now.route === original.route && original.route.current();
  };
  const requireCurrent = () => { if (!current()) throw new Error("Kept view retired"); };
  const root = original.root.replace(/\/$/, "");
  return {
    actions: { current,
      async openFile(relative) {
        requireCurrent(); if (!relativeFilePath(relative)) throw new Error("Invalid relative file");
        callbacks.openFile(original.paneId, `${root}/${relative}`);
      },
      completeOnboarding() { throw new Error("No onboarding authority in kept view"); },
      close() { requireCurrent(); callbacks.close(original.tab, original.paneId); },
      modal: (node) => { requireCurrent(); return callbacks.modal(node); },
    },
    openFolder() { requireCurrent(); callbacks.openFolder(original.paneId, original.root); },
  };
}
