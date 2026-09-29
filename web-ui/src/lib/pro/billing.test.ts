import { describe, expect, it } from "vitest";
import type { ProBillingAttempt, ProStatus } from "../net/native";
import { billingCopy, billingPending, billingNeedsReview, explicitCheckoutChoice, canReviewUpgrade, latestBilling, planPrice, planPrices } from "./billing";
const attempt = (phase: ProBillingAttempt["phase"], kind: ProBillingAttempt["kind"] = "checkout"): ProBillingAttempt => ({ id: 7, kind, phase, expires_at: 123, error: "private failure token=never-render-this" });
// Copy is free to change: these tests pin the pending/check/success flags and
// which outcomes read alike or apart, never the wording.

describe("native billing presentation", () => {
  it("does not promote a return or confirmed attempt to a paid account", () => {
    for (const phase of ["waiting", "confirming", "confirmed"] as const) expect(billingCopy(attempt(phase), "none").success).toBe(false);
    expect(billingCopy(attempt("confirmed"), "none").check).toBe(true);
    expect(billingCopy(attempt("confirming"), "pro")).toMatchObject({ pending: false, check: false, success: true });
  });
  it("only keeps the native waiting and confirming phases active", () => {
    expect(billingPending(undefined)).toBe(false); expect(billingPending(null)).toBe(false);
    for (const phase of ["opening", "waiting", "confirming"] as const) expect(billingPending(attempt(phase))).toBe(true);
    for (const phase of ["confirmed", "unconfirmed", "canceled", "expired", "failed"] as const) expect(billingPending(attempt(phase))).toBe(false);
  });
  it("explains timeout and cancellation without claiming payment was canceled or exposing errors", () => {
    for (const phase of ["canceled", "expired", "failed"] as const) {
      const copy = billingCopy(attempt(phase), "none");
      expect(copy.check).toBe(true); expect(copy.pending).toBe(false);
      expect(JSON.stringify(copy)).not.toContain("never-render");
    }
    // Stopping the wait is not the same outcome as the browser timing out.
    expect(billingCopy(attempt("canceled"), "none").title).not.toBe(billingCopy(attempt("expired"), "none").title);
  });
  it("holds uncertain checkout results out of plan selection until explicitly reviewed", () => {
    for (const phase of ["expired", "failed", "confirmed"] as const) {
      expect(billingNeedsReview(attempt(phase), false)).toBe(true);
      expect(billingNeedsReview(attempt(phase), true)).toBe(false);
      expect(billingNeedsReview(attempt(phase, "portal"), false)).toBe(false);
    }
    expect(billingNeedsReview(attempt("canceled"), false)).toBe(false);
    expect(billingNeedsReview(null, false)).toBe(false);
  });
  it("describes a completed portal return without claiming new entitlement", () => {
    expect(billingCopy(attempt("confirmed", "portal"), "none")).toMatchObject({ pending: false, check: false });
    expect(billingCopy(attempt("confirmed", "portal"), "none").title).not.toBe(billingCopy(attempt("confirmed"), "none").title);
    expect(billingCopy(attempt("waiting", "portal"), "pro").pending).toBe(true);
  });
});


describe("explicit checkout choice", () => {
  const free: ProStatus = { available: true, signed_in: true, email: "fixture@example.invalid", plan: "none", error: null };
  const choice = { plan: "max", interval: "year" } as const;
  it("uses the current review choice, independently of the earlier sign-in selection", () => {
    expect(explicitCheckoutChoice(free, true, choice)).toEqual(choice);
    expect(explicitCheckoutChoice(free, true, { plan: "pro", interval: "month" })).toEqual({ plan: "pro", interval: "month" });
  });
  it("never accepts a signed-out, stale, initializing, failed or unknown account for purchase", () => {
    expect(explicitCheckoutChoice(null, true, choice)).toBeNull();
    expect(explicitCheckoutChoice(free, false, choice)).toBeNull();
    for (const delta of [{ available: false }, { signed_in: false }, { initializing: true }, { error: "refresh failed" }, { plan: null }, { plan: "pro" as const }, { plan: "max" as const }, { sign_in: { phase: "finishing" as const, expires_at: 123 } }]) {
      expect(explicitCheckoutChoice({ ...free, ...delta }, true, choice)).toBeNull();
    }
  });
  it("never opens checkout while a payment needs attention", () => {
    expect(explicitCheckoutChoice({ ...free, payment_due: true }, true, choice)).toBeNull();
    expect(explicitCheckoutChoice({ ...free, payment_due: false }, true, choice)).toEqual(choice);
  });
  it("does not let an informational connection message block a confirmed choice", () => {
    expect(explicitCheckoutChoice({ ...free, connection_warning: "starting" }, true, choice)).toEqual(choice);
    expect(explicitCheckoutChoice({ ...free, error: "You're signed in. Your Pro connection is preparing; Chimaera will reconnect automatically." }, true, choice)).toEqual(choice);
    expect(canReviewUpgrade({ ...free, plan: "pro", connection_warning: "starting" }, true)).toBe(true);
  });
  it("does not start a second checkout while a browser attempt or uncertain result needs attention", () => {
    for (const phase of ["waiting", "confirming", "expired", "failed", "confirmed"] as const) {
      expect(explicitCheckoutChoice({ ...free, billing: attempt(phase) }, true, choice)).toBeNull();
    }
    expect(explicitCheckoutChoice({ ...free, billing: attempt("canceled") }, true, choice)).toEqual(choice);
  });
});

describe("subscriber upgrade review", () => {
  const pro: ProStatus = { available: true, signed_in: true, email: "fixture@example.invalid", plan: "pro", error: null };
  it("requires a fresh Pro account and no active browser attempt", () => {
    expect(canReviewUpgrade(pro, true)).toBe(true);
    expect(canReviewUpgrade(pro, false)).toBe(false);
    for (const delta of [{ plan: "max" as const }, { plan: "none" as const }, { plan: null }, { signed_in: false }, { initializing: true }, { error: "unavailable" }, { billing: attempt("opening", "portal") }]) {
      expect(canReviewUpgrade({ ...pro, ...delta }, true)).toBe(false);
    }
  });
  it("does not claim Max from an existing Pro subscription or browser return", () => {
    for (const phase of ["opening", "waiting", "confirming", "confirmed"] as const) {
      const upgrade = { ...attempt(phase, "plan_change"), requested_plan: "max" as const };
      expect(billingCopy(upgrade, "pro").success).toBe(false);
      expect(billingCopy(upgrade, null).success).toBe(false);
      expect(billingCopy(upgrade, "max").success).toBe(true);
    }
  });
  it("settles an unconfirmed review honestly and still recognizes a later exact-plan update", () => {
    const review = { ...attempt("unconfirmed", "plan_change"), requested_plan: "max" as const };
    expect(billingCopy(review, "pro")).toMatchObject({ pending: false, success: false, check: true });
    expect(billingCopy(review, null)).toMatchObject({ success: false });
    expect(billingCopy(review, "pro").title).not.toBe(billingCopy(review, null).title);
    expect(billingCopy(review, "max")).toMatchObject({ success: true });
  });
  it("does not pretend stopping a local wait closed the browser", () => {
    const copy = billingCopy(attempt("canceled", "portal"), "pro");
    expect(copy).toMatchObject({ pending: false, check: true, success: false });
    const opening = billingCopy(attempt("opening", "portal"), "pro");
    const waiting = billingCopy(attempt("waiting", "portal"), "pro");
    expect(new Set([copy.title, opening.title, waiting.title]).size).toBe(3);
    // A portal attempt is never described as a checkout.
    expect(waiting.title).not.toBe(billingCopy(attempt("waiting"), "none").title);
  });
});

describe("plan prices", () => {
  const plans = [
    { plan: "pro" as const, interval: "month" as const, amount_cents: 1250, currency: "usd" },
    { plan: "pro" as const, interval: "year" as const, amount_cents: 12000, currency: "USD" },
    { plan: "max" as const, interval: "month" as const, amount_cents: 1500, currency: "JPY" },
  ];
  it("shows an amount only when the account supplies one", () => {
    expect(planPrice(plans, "pro", "month", "en-US")).toBe("$12.50");
    expect(planPrice(plans, "pro", "year", "en-US")).toBe("$120");
    for (const absent of [undefined, null, []]) expect(planPrice(absent, "pro", "month", "en-US")).toBeNull();
    expect(planPrice(plans, "max", "year", "en-US")).toBeNull();
  });
  it("reads minor units per currency", () => {
    expect(planPrice(plans, "max", "month", "en-US")).toBe("¥1,500");
  });
  it("shows all four prices or none", () => {
    const complete = [...plans, { plan: "max" as const, interval: "year" as const, amount_cents: 15000, currency: "JPY" }];
    expect(planPrices(complete, "en-US")).toEqual({
      pro: { month: planPrice(complete, "pro", "month", "en-US"), year: planPrice(complete, "pro", "year", "en-US") },
      max: { month: planPrice(complete, "max", "month", "en-US"), year: planPrice(complete, "max", "year", "en-US") },
    });
    // One missing, malformed or unknown-currency entry withholds every amount.
    expect(planPrices(plans, "en-US")).toBeNull();
    for (const bad of [{ amount_cents: -1 }, { currency: "ZZZ" }]) {
      const value = complete.map(entry => entry.plan === "max" && entry.interval === "year" ? { ...entry, ...bad } : entry);
      expect(planPrices(value, "en-US")).toBeNull();
    }
    for (const absent of [undefined, null, []]) expect(planPrices(absent, "en-US")).toBeNull();
  });
  it("shows nothing for a malformed entry rather than a guessed amount", () => {
    for (const entry of [
      { amount_cents: -1, currency: "USD" },
      { amount_cents: 1.5, currency: "USD" },
      { amount_cents: 100, currency: "US" },
      { amount_cents: 100, currency: "ZZZ" },
      { amount_cents: 100, currency: 5 },
    ]) {
      const value = [{ plan: "pro", interval: "month", ...entry }] as unknown as Parameters<typeof planPrice>[0];
      expect(planPrice(value, "pro", "month", "en-US")).toBeNull();
    }
  });
});

describe("billing snapshot order", () => {
  it("keeps a new portal when an older canceled snapshot arrives", () => {
    const current = { ...attempt("waiting", "portal"), id: 9 };
    expect(latestBilling(current, attempt("canceled", "portal"))).toBe(current);
    const newer = { ...attempt("opening", "portal"), id: 10 };
    expect(latestBilling(current, newer)).toBe(newer);
  });
  it("does not regress confirming or confirmed, but accepts explicit dismissal", () => {
    const confirming = attempt("confirming", "portal");
    const confirmed = attempt("confirmed", "portal");
    expect(latestBilling(confirming, attempt("waiting", "portal"))).toBe(confirming);
    expect(latestBilling(confirmed, confirming)).toBe(confirmed);
    expect(latestBilling(confirmed, null)).toBeNull();
  });
});
