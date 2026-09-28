import { describe, expect, it } from "vitest";
import type { ProBillingAttempt, ProStatus } from "../net/native";
import { billingCopy, billingPending, billingNeedsReview, explicitCheckoutChoice } from "./billing";
const attempt = (phase: ProBillingAttempt["phase"], kind: ProBillingAttempt["kind"] = "checkout"): ProBillingAttempt => ({ id: 7, kind, phase, expires_at: 123, error: "private failure token=never-render-this" });

describe("native billing presentation", () => {
  it("does not promote a return or confirmed attempt to a paid account", () => {
    for (const phase of ["waiting", "confirming", "confirmed"] as const) expect(billingCopy(attempt(phase), false).success).toBe(false);
    expect(billingCopy(attempt("confirmed"), false).check).toBe(true);
    expect(billingCopy(attempt("confirming"), true)).toMatchObject({ title: "Your plan is active", pending: false, success: true });
  });
  it("only keeps the native waiting and confirming phases active", () => {
    expect(billingPending(undefined)).toBe(false); expect(billingPending(null)).toBe(false);
    for (const phase of ["waiting", "confirming"] as const) expect(billingPending(attempt(phase))).toBe(true);
    for (const phase of ["confirmed", "canceled", "expired", "failed"] as const) expect(billingPending(attempt(phase))).toBe(false);
  });
  it("explains timeout and cancellation without claiming payment was canceled or exposing errors", () => {
    for (const phase of ["canceled", "expired", "failed"] as const) {
      const copy = billingCopy(attempt(phase), false);
      expect(copy.check).toBe(true); expect(copy.pending).toBe(false);
      expect(JSON.stringify(copy)).not.toContain("never-render");
    }
    expect(billingCopy(attempt("canceled"), false).detail).toContain("doesn't cancel a payment");
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
    expect(billingCopy(attempt("confirmed", "portal"), false).title).toBe("Account updated");
    expect(billingCopy(attempt("waiting", "portal"), true).title).toContain("Billing");
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
  it("does not start a second checkout while a browser attempt or uncertain result needs attention", () => {
    for (const phase of ["waiting", "confirming", "expired", "failed", "confirmed"] as const) {
      expect(explicitCheckoutChoice({ ...free, billing: attempt(phase) }, true, choice)).toBeNull();
    }
    expect(explicitCheckoutChoice({ ...free, billing: attempt("canceled") }, true, choice)).toEqual(choice);
  });
});
