import type { ProStatus } from "../net/native";
import { billingNeedsReview, billingPending } from "./billing";
import { paid } from "./presentation";
import { accountFailure, paymentDue } from "./status";

/** A confirmed account with no plan (or signed out). Rendering reads the last
 * confirmed status; purchase actions separately require a fresh read. */
export function isConfirmedFree(status: ProStatus | null): boolean {
  return status?.available === true && !status.initializing && accountFailure(status) === null
    && !paymentDue(status) && (status.signed_in ? status.plan === "none" : true);
}

/** The Pro page's main area. Derived from the last confirmed status so a
 * background read never unmounts plans, the walkthrough or the overview. */
export type AccountPanel = "loading" | "unavailable" | "initializing" | "plans" | "payment" | "subscriber" | "billing" | "checking" | "attention";

export function accountPanel(status: ProStatus | null, refreshing: boolean): AccountPanel {
  if (status === null) return "loading";
  if (!status.available) return "unavailable";
  if (status.initializing) return "initializing";
  const subscribed = status.signed_in && paid(status.plan);
  const billing = billingPending(status.billing) || billingNeedsReview(status.billing, subscribed);
  if (isConfirmedFree(status) && !billing) return "plans";
  if (subscribed) return "subscriber";
  // A lapsed payment is fixed in billing; it never falls back to plan choice.
  if (paymentDue(status) && accountFailure(status) === null) return "payment";
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

/** Max is offered where a limit is near or reached, never as a standing
 * upsell: 80 % of an allowance, a used-up allowance, or use with no allowance. */
export function nearLimit(status: Pick<ProStatus, "usage" | "limits" | "hours_exhausted"> | null): boolean {
  if (status?.hours_exhausted === true) return true;
  const near = (used: number | undefined, limit: number | undefined): boolean =>
    typeof used === "number" && typeof limit === "number" && Number.isFinite(used) && Number.isFinite(limit)
    && used > 0 && (limit <= 0 || used / limit >= 0.8);
  return near(status?.usage?.cloud_hours, status?.limits?.cloud_hours)
    || near(status?.usage?.storage_bytes, status?.limits?.storage_bytes);
}

/** An explicit Check account completes its review only on a read that shows the
 * same checkout attempt still unresolved on a confirmed account with no plan. */
export function completesReview(status: ProStatus | null, requestedId: number | null): boolean {
  return requestedId !== null && status?.billing?.id === requestedId
    && isConfirmedFree(status) && billingNeedsReview(status.billing, false);
}
