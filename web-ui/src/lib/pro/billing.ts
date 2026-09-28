import type { ProBillingAttempt } from "../net/native";

export function billingPending(attempt: ProBillingAttempt | null | undefined): boolean {
  return attempt?.phase === "waiting" || attempt?.phase === "confirming";
}

export function billingNeedsReview(attempt: ProBillingAttempt | null | undefined, subscribed: boolean): boolean {
  return !subscribed && attempt?.kind === "checkout"
    && ["expired", "failed", "confirmed"].includes(attempt.phase);
}

/** Billing return is a signal; only the account's plan can confirm entitlement. */
export function billingCopy(attempt: ProBillingAttempt, subscribed: boolean): {
  title: string; detail: string; pending: boolean; check: boolean; success: boolean;
} {
  const checkout = attempt.kind === "checkout";
  if (checkout && subscribed) return { title: "Your plan is active", detail: "Your account has confirmed your plan. You can continue with your cloud setup below.", pending: false, check: false, success: true };
  switch (attempt.phase) {
    case "waiting": return {
      title: checkout ? "Checkout is open in your browser" : "Billing is open in your browser",
      detail: checkout ? "Complete checkout there. Chimaera will bring you back and confirm your plan automatically. You can keep working while you wait." : "Finish your changes there. Chimaera will bring you back and check your account automatically.",
      pending: true, check: false, success: false,
    };
    case "confirming": return {
      title: checkout ? "Confirming your plan" : "Checking your billing changes",
      detail: "You're back in Chimaera. We're checking your account; this can take a moment. You can keep working while it finishes.",
      pending: true, check: false, success: false,
    };
    case "confirmed": return checkout
      ? { title: "Check your account", detail: "Checkout returned, but an active plan hasn't been confirmed. Check your account before starting again.", pending: false, check: true, success: false }
      : { title: "Account updated", detail: "Your billing changes have been checked. Your current plan and usage are shown below.", pending: false, check: false, success: true };
    case "canceled": return { title: "Browser request closed", detail: "You can check your account or continue when you're ready. Stopping this request doesn't cancel a payment or subscription.", pending: false, check: true, success: false };
    case "expired": return { title: "Confirmation timed out", detail: checkout ? "We stopped waiting for this browser request. If you completed payment, check your account before opening checkout again." : "We stopped waiting for the billing page. Check your account to see any changes you made.", pending: false, check: true, success: false };
    case "failed": return { title: "We couldn't confirm this request", detail: checkout ? "Check your account to see its latest status. If you completed payment, do that before trying checkout again." : "We couldn't refresh your billing changes. Check your account to see its latest status.", pending: false, check: true, success: false };
  }
}
