import type { ProBillingAttempt, ProStatus } from "../net/native";

import type { PlanChoice } from "./presentation";

/** Called only by an explicit purchase action; remembered drafts are never authority. */
export function explicitCheckoutChoice(status: ProStatus | null, fresh: boolean, selected: PlanChoice): PlanChoice | null {
  if (!fresh || !status?.available || !status.signed_in || status.initializing || status.error
    || status.sign_in != null || status.plan !== "none" || billingPending(status.billing)
    || billingNeedsReview(status.billing, false)) return null;
  return { ...selected };
}

export function billingPending(attempt: ProBillingAttempt | null | undefined): boolean {
  return attempt?.phase === "opening" || attempt?.phase === "waiting" || attempt?.phase === "confirming";
}

export function billingNeedsReview(attempt: ProBillingAttempt | null | undefined, subscribed: boolean): boolean {
  return !subscribed && attempt?.kind === "checkout"
    && ["expired", "failed", "confirmed"].includes(attempt.phase);
}

/** Billing return is a signal; only the account's plan can confirm entitlement. */
export function billingCopy(attempt: ProBillingAttempt, plan: ProStatus["plan"]): {
  title: string; detail: string; pending: boolean; check: boolean; success: boolean;
} {
  const checkout = attempt.kind === "checkout";
  const change = attempt.kind === "plan_change";
  const targetConfirmed = attempt.requested_plan != null && plan === attempt.requested_plan;
  if (checkout && (targetConfirmed || (attempt.requested_plan == null && (plan === "pro" || plan === "max")))) return { title: "Your plan is active", detail: "Your account has confirmed your plan.", pending: false, check: false, success: true };
  if (change && targetConfirmed) return { title: `You're on Chimaera ${plan === "max" ? "Max" : "Pro"}`, detail: "Your account has confirmed the new plan. Your updated usage allowances are shown below.", pending: false, check: false, success: true };
  switch (attempt.phase) {
    case "opening": return {
      title: checkout ? "Opening checkout" : change ? "Opening your plan review" : "Opening billing",
      detail: "Preparing your secure browser session. Your current plan stays unchanged.",
      pending: true, check: false, success: false,
    };
    case "waiting": return {
      title: checkout ? "Checkout is open in your browser" : change ? "Review your upgrade in your browser" : "Billing is open in your browser",
      detail: checkout ? "Complete checkout there. Chimaera will bring you back and confirm your plan automatically. You can keep working while you wait." : change ? "Review the final amount and timing before confirming. Your plan changes only after you confirm in the billing page." : "Manage your subscription there. Returning brings you back to Chimaera and refreshes your account.",
      pending: true, check: false, success: false,
    };
    case "confirming": return {
      title: checkout || change ? "Confirming your plan" : "Checking your billing account",
      detail: "You're back in Chimaera. We're checking your account; this can take a moment. You can keep working while it finishes.",
      pending: true, check: false, success: false,
    };
    case "confirmed": return checkout || change
      ? { title: "Check your account", detail: "The browser returned, but the requested plan hasn't been confirmed. Check your account before starting again.", pending: false, check: true, success: false }
      : { title: "You're back from billing", detail: "Your current plan and usage have been refreshed.", pending: false, check: false, success: true };
    case "canceled": return { title: "Stopped waiting for billing", detail: "Chimaera is no longer watching this browser session. This doesn't close the billing page or cancel a payment or subscription. You can reopen billing or check your account.", pending: false, check: true, success: false };
    case "expired": return { title: "Confirmation timed out", detail: checkout || change ? "We stopped waiting for this browser request. If you completed payment, check your account before opening checkout again." : "We stopped waiting for the billing page. Check your account to see any changes you made.", pending: false, check: true, success: false };
    case "failed": return { title: "We couldn't confirm this request", detail: checkout || change ? "Check your account to see its latest status. If you completed payment, do that before trying checkout again." : "We couldn't refresh your billing changes. Check your account to see its latest status.", pending: false, check: true, success: false };
  }
}

/** Only a fresh, confirmed Pro subscriber is offered an upgrade review. */
export function canReviewUpgrade(status: ProStatus | null, fresh: boolean): boolean {
  return fresh && status?.available === true && status.signed_in && !status.initializing
    && !status.error && status.sign_in == null && status.plan === "pro" && !billingPending(status.billing);
}

/** Delayed snapshots cannot roll a newer attempt or its terminal result backward. */
export function latestBilling(previous: ProBillingAttempt | null | undefined, incoming: ProBillingAttempt | null | undefined): ProBillingAttempt | null | undefined {
  if (!previous || !incoming) return incoming;
  if (incoming.id < previous.id) return previous;
  if (incoming.id > previous.id) return incoming;
  const stage = (value: ProBillingAttempt): number => value.phase === "opening" ? 0 : value.phase === "waiting" ? 1 : value.phase === "confirming" ? 2 : 3;
  return stage(incoming) < stage(previous) ? previous : incoming;
}
