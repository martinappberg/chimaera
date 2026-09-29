import type { ProStatus } from "../net/native";
import { billingNeedsReview, billingPending } from "./billing";
import { paid } from "./presentation";

/** A confirmed account with no plan (or signed out). Rendering reads the last
 * confirmed status; purchase actions separately require a fresh read. */
export function isConfirmedFree(status: ProStatus | null): boolean {
  return status?.available === true && !status.initializing && !status.error
    && (status.signed_in ? status.plan === "none" : true);
}

/** The Pro page's main area. Derived from the last confirmed status so a
 * background read never unmounts plans, the walkthrough or the overview. */
export type AccountPanel = "loading" | "unavailable" | "initializing" | "plans" | "subscriber" | "billing" | "checking" | "attention";

export function accountPanel(status: ProStatus | null, refreshing: boolean): AccountPanel {
  if (status === null) return "loading";
  if (!status.available) return "unavailable";
  if (status.initializing) return "initializing";
  const subscribed = status.signed_in && paid(status.plan);
  const billing = billingPending(status.billing) || billingNeedsReview(status.billing, subscribed);
  if (isConfirmedFree(status) && !billing) return "plans";
  if (subscribed) return "subscriber";
  if (billing) return "billing";
  // Only an uncertain account waits on a read; confirmed states stay put.
  return refreshing ? "checking" : "attention";
}

/** Identity of a reviewed billing result. "Return to plans" stays offered only
 * while the attempt, its phase and the account plan are the ones reviewed, so
 * an unrelated account event no longer withdraws it. */
export function reviewKey(status: ProStatus | null): string | null {
  const billing = status?.billing;
  return billing ? `${billing.id}:${billing.phase}:${status?.plan ?? "unknown"}` : null;
}

/** An explicit Check account completes its review only on a read that shows the
 * same checkout attempt still unresolved on a confirmed account with no plan. */
export function completesReview(status: ProStatus | null, requestedId: number | null): boolean {
  return requestedId !== null && status?.billing?.id === requestedId
    && isConfirmedFree(status) && billingNeedsReview(status.billing, false);
}
