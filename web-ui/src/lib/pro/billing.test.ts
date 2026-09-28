import { describe, expect, it } from "vitest";
import type { ProBillingAttempt } from "../net/native";
import { billingCopy, billingPending, billingNeedsReview } from "./billing";
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
