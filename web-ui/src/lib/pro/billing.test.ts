import { describe, expect, it } from "vitest";
import type { ProBillingAttempt, ProStatus } from "../net/native";
import { billingCopy, billingPending, billingNeedsReview, explicitCheckoutChoice, canReviewUpgrade, latestBilling } from "./billing";
const attempt = (phase: ProBillingAttempt["phase"], kind: ProBillingAttempt["kind"] = "checkout"): ProBillingAttempt => ({ id: 7, kind, phase, expires_at: 123, error: "private failure token=never-render-this" });

describe("native billing presentation", () => {
  it("does not promote a return or confirmed attempt to a paid account", () => {
    for (const phase of ["waiting", "confirming", "confirmed"] as const) expect(billingCopy(attempt(phase), "none").success).toBe(false);
    expect(billingCopy(attempt("confirmed"), "none").check).toBe(true);
    expect(billingCopy(attempt("confirming"), "pro")).toMatchObject({ title: "Your plan is active", pending: false, success: true });
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
    expect(billingCopy(attempt("canceled"), "none").detail).toContain("cancel a payment");
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
    expect(billingCopy(attempt("confirmed", "portal"), "none").title).toBe("You're back from billing");
    expect(billingCopy(attempt("waiting", "portal"), "pro").title).toContain("Billing");
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
    expect(billingCopy(review, "pro")).toMatchObject({ title: "Your plan is still Pro", pending: false, success: false, check: true });
    expect(billingCopy(review, null)).toMatchObject({ title: "No plan change confirmed", success: false });
    expect(billingCopy(review, "max")).toMatchObject({ title: "You're on Chimaera Max", success: true });
    expect(billingCopy(review, "pro").detail).not.toContain("canceled");
  });
  it("does not pretend stopping a local wait closed the browser", () => {
    const copy = billingCopy(attempt("canceled", "portal"), "pro");
    expect(copy.title).toBe("Stopped waiting for billing");
    expect(copy.detail).toContain("doesn't close the billing page");
    expect(billingCopy(attempt("opening", "portal"), "pro").title).toBe("Opening billing");
    expect(billingCopy(attempt("waiting", "portal"), "pro").title).toBe("Billing is open in your browser");
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
