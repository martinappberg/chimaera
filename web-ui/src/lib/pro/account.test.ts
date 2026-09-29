import { describe, expect, it } from "vitest";
import type { ProBillingAttempt, ProStatus } from "../net/native";
import { accountPanel, completesReview, isConfirmedFree, nearLimit, reviewKey } from "./account";

const free: ProStatus = { available: true, signed_in: true, email: "fixture@example.invalid", plan: "none", error: null };
const attempt = (phase: ProBillingAttempt["phase"], id = 7): ProBillingAttempt => ({ id, kind: "checkout", phase, expires_at: 123, error: null });

describe("Pro page panel", () => {
  it("keeps confirmed plans and subscriber views mounted while a background read runs", () => {
    for (const refreshing of [false, true]) {
      expect(accountPanel(free, refreshing)).toBe("plans");
      expect(accountPanel({ ...free, signed_in: false, email: null, plan: null }, refreshing)).toBe("plans");
      expect(accountPanel({ ...free, plan: "pro" }, refreshing)).toBe("subscriber");
      expect(accountPanel({ ...free, plan: "max" }, refreshing)).toBe("subscriber");
    }
  });
  it("shows a checking state only for an uncertain account", () => {
    const uncertain = { ...free, plan: null };
    expect(accountPanel(uncertain, true)).toBe("checking");
    expect(accountPanel(uncertain, false)).toBe("attention");
    expect(accountPanel({ ...free, error: "account_restore_unavailable" }, false)).toBe("attention");
  });
  it("routes startup, endpoint-less builds and billing attempts ahead of plans", () => {
    expect(accountPanel(null, true)).toBe("loading");
    expect(accountPanel({ ...free, available: false }, false)).toBe("unavailable");
    expect(accountPanel({ ...free, initializing: true }, false)).toBe("initializing");
    for (const phase of ["opening", "waiting", "confirming", "expired", "failed", "confirmed"] as const) {
      expect(accountPanel({ ...free, billing: attempt(phase) }, false)).toBe("billing");
    }
    expect(accountPanel({ ...free, billing: attempt("canceled") }, false)).toBe("plans");
  });
  it("never treats a stale or failed account as confirmed free", () => {
    expect(isConfirmedFree(null)).toBe(false);
    for (const delta of [{ available: false }, { initializing: true }, { error: "failed" }, { plan: null }, { plan: "pro" as const }]) {
      expect(isConfirmedFree({ ...free, ...delta })).toBe(false);
    }
  });
});

describe("Max offer", () => {
  const limits = { cloud_hours: 100, storage_bytes: 1000 };
  it("appears only near or at a limit", () => {
    expect(nearLimit(null)).toBe(false);
    expect(nearLimit({ usage: { cloud_hours: 10, storage_bytes: 100 }, limits })).toBe(false);
    expect(nearLimit({ usage: null, limits })).toBe(false);
    expect(nearLimit({ usage: { cloud_hours: 80, storage_bytes: 0 }, limits })).toBe(true);
    expect(nearLimit({ usage: { cloud_hours: 0, storage_bytes: 950 }, limits })).toBe(true);
    expect(nearLimit({ usage: { cloud_hours: 0, storage_bytes: 0 }, limits, hours_exhausted: true })).toBe(true);
  });
  it("does not treat an unused account or an absent allowance as a limit", () => {
    expect(nearLimit({ usage: { cloud_hours: 0, storage_bytes: 0 }, limits: { cloud_hours: 0, storage_bytes: 0 } })).toBe(false);
    expect(nearLimit({ usage: { cloud_hours: 3, storage_bytes: 0 }, limits: { cloud_hours: 0, storage_bytes: 0 } })).toBe(true);
    expect(nearLimit({ usage: { cloud_hours: Number.NaN, storage_bytes: 0 }, limits })).toBe(false);
  });
});

describe("payment needs attention", () => {
  it("routes an overdue account to billing instead of plan choice", () => {
    const due = { ...free, payment_due: true };
    expect(isConfirmedFree(due)).toBe(false);
    for (const refreshing of [false, true]) expect(accountPanel(due, refreshing)).toBe("payment");
    expect(accountPanel({ ...due, plan: "pro" }, false)).toBe("subscriber");
    expect(accountPanel({ ...due, error: "sign in required" }, false)).toBe("attention");
  });
  it("ignores the flag when absent or signed out", () => {
    expect(accountPanel({ ...free, payment_due: false }, false)).toBe("plans");
    expect(accountPanel({ ...free, signed_in: false, email: null, plan: null, payment_due: true }, false)).toBe("plans");
  });
});

describe("billing review", () => {
  it("keys a review by attempt, phase and plan so unrelated events keep it", () => {
    const failed = { ...free, billing: attempt("failed") };
    expect(reviewKey(failed)).toBe(reviewKey({ ...failed, usage: null }));
    expect(reviewKey(failed)).not.toBe(reviewKey({ ...failed, billing: attempt("expired") }));
    expect(reviewKey(failed)).not.toBe(reviewKey({ ...failed, billing: attempt("failed", 8) }));
    expect(reviewKey(failed)).not.toBe(reviewKey({ ...failed, plan: "pro" }));
    expect(reviewKey(free)).toBeNull();
  });
  it("completes only for the requested unresolved attempt on a confirmed free account", () => {
    const failed = { ...free, billing: attempt("failed") };
    expect(completesReview(failed, 7)).toBe(true);
    expect(completesReview(failed, null)).toBe(false);
    expect(completesReview(failed, 8)).toBe(false);
    expect(completesReview({ ...failed, plan: "pro" }, 7)).toBe(false);
    expect(completesReview({ ...failed, error: "failed" }, 7)).toBe(false);
    expect(completesReview({ ...free, billing: attempt("waiting") }, 7)).toBe(false);
  });
});
