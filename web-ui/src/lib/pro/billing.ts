import type { ProBillingAttempt, ProPlanPrice, ProStatus } from "../net/native";

import type { PlanChoice } from "./presentation";
import { accountFailure, grantedPlan, paymentDue } from "./status";

/** Called only by an explicit purchase action; remembered drafts are never authority. */
export function explicitCheckoutChoice(status: ProStatus | null, fresh: boolean, selected: PlanChoice): PlanChoice | null {
  if (!fresh || !status?.available || !status.signed_in || status.initializing || accountFailure(status) !== null
    || status.sign_in != null || grantedPlan(status) !== "none" || paymentDue(status) || billingPending(status.billing)
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
    case "unconfirmed": return {
      title: plan === "pro" ? "Your plan is still Pro" : plan === "max" ? "Your plan is still Max" : "No plan change confirmed",
      detail: "No plan change has been confirmed. If you confirmed a change in billing, check your account again shortly.",
      pending: false, check: true, success: false,
    };
    case "canceled": return { title: "Stopped waiting for billing", detail: "Chimaera is no longer watching this browser session. This doesn't close the billing page or cancel a payment or subscription. You can reopen billing or check your account.", pending: false, check: true, success: false };
    case "expired": return { title: "Confirmation timed out", detail: checkout || change ? "We stopped waiting for this browser request. If you completed payment, check your account before opening checkout again." : "We stopped waiting for the billing page. Check your account to see any changes you made.", pending: false, check: true, success: false };
    case "failed": return { title: "We couldn't confirm this request", detail: checkout || change ? "Check your account to see its latest status. If you completed payment, do that before trying checkout again." : "We couldn't refresh your billing changes. Check your account to see its latest status.", pending: false, check: true, success: false };
  }
}

/** Only a fresh, confirmed Pro subscriber is offered an upgrade review. */
export function canReviewUpgrade(status: ProStatus | null, fresh: boolean): boolean {
  return fresh && status?.available === true && status.signed_in && !status.initializing
    && accountFailure(status) === null && status.sign_in == null && grantedPlan(status) === "pro" && !billingPending(status.billing);
}

/** A display price from the account's own list. Prices are never built into
 * the app: an absent, malformed or unknown-currency entry shows no amount. */
export function planPrice(plans: ProPlanPrice[] | null | undefined, plan: "pro" | "max", interval: "month" | "year", locale?: string): string | null {
  const entry = Array.isArray(plans) ? plans.find(value => value?.plan === plan && value.interval === interval) : undefined;
  if (!entry || !Number.isSafeInteger(entry.amount_cents) || entry.amount_cents < 0
    || typeof entry.currency !== "string" || !/^[A-Za-z]{3}$/.test(entry.currency)) return null;
  try {
    const currency = entry.currency.toUpperCase();
    // Intl formats any well-formed code; show only currencies it knows.
    if (typeof Intl.supportedValuesOf === "function" && !Intl.supportedValuesOf("currency").includes(currency)) return null;
    // Minor units follow the currency (cents for USD, none for JPY).
    const digits = new Intl.NumberFormat("en", { style: "currency", currency }).resolvedOptions().maximumFractionDigits ?? 2;
    const amount = entry.amount_cents / Math.pow(10, digits);
    return new Intl.NumberFormat(locale, { style: "currency", currency, minimumFractionDigits: Number.isInteger(amount) ? 0 : digits, maximumFractionDigits: digits }).format(amount);
  } catch { return null; }
}

/** Every displayed plan price: Pro and Max, monthly and yearly. */
export type PlanPrices = Record<"pro" | "max", Record<"month" | "year", string>>;

/** All four prices, or null when the account leaves any out or malformed.
 * Prices are all-or-none: a page showing some amounts and "shown at checkout"
 * for others would read as a partial or inconsistent offer. */
export function planPrices(plans: ProPlanPrice[] | null | undefined, locale?: string): PlanPrices | null {
  const table: Partial<PlanPrices> = {};
  for (const plan of ["pro", "max"] as const) {
    const month = planPrice(plans, plan, "month", locale);
    const year = planPrice(plans, plan, "year", locale);
    if (month === null || year === null) return null;
    table[plan] = { month, year };
  }
  return table as PlanPrices;
}

/** How many times Max's monthly cloud time and storage are Pro's, as the
 * service states them (whole numbers relative to Pro; no absolute allowance
 * ever reaches the app). Null where a number is missing, malformed or not above
 * Pro's (1 is Pro's own). */
export interface PlanMultiples {
  cloud: number | null;
  storage: number | null;
}

/** A multiple worth saying: a safe integer of at least 2 ("1x" is Pro itself,
 * and zero, a string or a fraction is not a multiple the service meant). */
const isMultiple = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 2;

/** Max's multiples of Pro. Both intervals of a plan carry the same pair, so
 * each number is read from the first Max entry that states it validly. */
export function planMultiples(plans: ProPlanPrice[] | null | undefined): PlanMultiples {
  const multiples: PlanMultiples = { cloud: null, storage: null };
  if (!Array.isArray(plans)) return multiples;
  for (const entry of plans) {
    if (entry?.plan !== "max") continue;
    if (multiples.cloud === null && isMultiple(entry.cloud_time_multiple)) multiples.cloud = entry.cloud_time_multiple;
    if (multiples.storage === null && isMultiple(entry.storage_multiple)) multiples.storage = entry.storage_multiple;
  }
  return multiples;
}

/** The Max card's capacity line: how many times more cloud time and storage it
 * gives than Pro, whole numbers from the service. One shared multiple reads
 * once ("5× the cloud time and storage of Pro"); two different ones each name
 * their own; one alone names only that. Null when the service states none (an
 * older service), so the caller keeps its generic line. Nothing here is an
 * absolute amount, and none is built into the app. */
export function maxCapacityNote(plans: ProPlanPrice[] | null | undefined, locale?: string): string | null {
  const { cloud, storage } = planMultiples(plans);
  if (cloud === null && storage === null) return null;
  try {
    const times = (value: number): string => `${new Intl.NumberFormat(locale, { maximumFractionDigits: 0 }).format(value)}×`;
    if (cloud !== null && storage !== null) {
      return cloud === storage
        ? `${times(cloud)} the cloud time and storage of Pro`
        : `${times(cloud)} the cloud time and ${times(storage)} the storage of Pro`;
    }
    return cloud !== null ? `${times(cloud)} the cloud time of Pro` : `${times(storage as number)} the storage of Pro`;
  } catch { return null; }
}

/** Delayed snapshots cannot roll a newer attempt or its terminal result backward. */
export function latestBilling(previous: ProBillingAttempt | null | undefined, incoming: ProBillingAttempt | null | undefined): ProBillingAttempt | null | undefined {
  if (!previous || !incoming) return incoming;
  if (incoming.id < previous.id) return previous;
  if (incoming.id > previous.id) return incoming;
  const stage = (value: ProBillingAttempt): number => value.phase === "opening" ? 0 : value.phase === "waiting" ? 1 : value.phase === "confirming" ? 2 : 3;
  return stage(incoming) < stage(previous) ? previous : incoming;
}
