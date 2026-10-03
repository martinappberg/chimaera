import { describe, expect, it } from "vitest";
import type { ProBillingAttempt, ProStatus } from "../net/native";
import { accountErrorBar, accountPanel, completesReview, isConfirmedFree, nearLimit, offersCheck, reviewKey, type AccountPanel, type AccountRead } from "./account";

const free: ProStatus = { available: true, signed_in: true, email: "fixture@example.invalid", plan: "none", error: null };
const attempt = (phase: ProBillingAttempt["phase"], id = 7): ProBillingAttempt => ({ id, kind: "checkout", phase, expires_at: 123, error: null });
const reads: AccountRead[] = ["none", "background", "check"];

describe("Pro page panel", () => {
  it("keeps confirmed plans and subscriber views mounted through any read", () => {
    for (const read of reads) {
      expect(accountPanel(free, read)).toBe("plans");
      expect(accountPanel({ ...free, signed_in: false, email: null, plan: null }, read)).toBe("plans");
      expect(accountPanel({ ...free, plan: "pro" }, read)).toBe("subscriber");
      expect(accountPanel({ ...free, plan: "max" }, read)).toBe("subscriber");
      expect(accountPanel({ ...free, plan: "pro", error: "service_unsupported" }, read)).toBe("subscriber");
    }
  });
  it("holds an uncertain account's attention panel through background reads", () => {
    for (const uncertain of [{ ...free, plan: null }, { ...free, error: "account_restore_unavailable" }, { ...free, error: "service_unsupported" }]) {
      expect(accountPanel(uncertain, "none")).toBe("attention");
      expect(accountPanel(uncertain, "background")).toBe(accountPanel(uncertain, "none"));
      // Only a check the user asked for replaces it while it runs.
      expect(accountPanel(uncertain, "check")).toBe("checking");
    }
  });
  it("routes startup, endpoint-less builds and billing attempts ahead of plans", () => {
    for (const read of reads) expect(accountPanel(null, read)).toBe("loading");
    expect(accountPanel({ ...free, available: false }, "none")).toBe("unavailable");
    expect(accountPanel({ ...free, initializing: true }, "check")).toBe("initializing");
    for (const phase of ["opening", "waiting", "confirming", "expired", "failed", "confirmed"] as const) {
      expect(accountPanel({ ...free, billing: attempt(phase) }, "none")).toBe("billing");
    }
    expect(accountPanel({ ...free, billing: attempt("canceled") }, "none")).toBe("plans");
  });
  it("never treats a stale or failed account as confirmed free", () => {
    expect(isConfirmedFree(null)).toBe(false);
    for (const delta of [{ available: false }, { initializing: true }, { error: "failed" }, { plan: null }, { plan: "pro" as const }]) {
      expect(isConfirmedFree({ ...free, ...delta })).toBe(false);
    }
  });
});

describe("Max offer", () => {
  const limits = { cloud_hours: 40, storage_bytes: 1000 };
  it("appears only near or at a limit", () => {
    expect(nearLimit(null)).toBe(false);
    expect(nearLimit({ usage: { cloud_hours: 4, storage_bytes: 100 }, limits })).toBe(false);
    expect(nearLimit({ usage: null, limits })).toBe(false);
    expect(nearLimit({ usage: { cloud_hours: 32, storage_bytes: 0 }, limits })).toBe(true);
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
    for (const read of reads) expect(accountPanel(due, read)).toBe("payment");
    expect(accountPanel({ ...due, plan: "pro" }, "none")).toBe("subscriber");
    expect(accountPanel({ ...due, error: "sign in required" }, "background")).toBe("attention");
  });
  it("ignores the flag when absent or signed out", () => {
    expect(accountPanel({ ...free, payment_due: false }, "none")).toBe("plans");
    expect(accountPanel({ ...free, signed_in: false, email: null, plan: null, payment_due: true }, "none")).toBe("plans");
  });
});

describe("an ended plan inside its return window", () => {
  const ended = { ...free, returning_until: "2026-11-03T09:30:00Z" };
  it("offers plans, never the subscriber view, whatever plan the account still names", () => {
    for (const plan of ["none", "pro", "max"] as const) {
      expect(isConfirmedFree({ ...ended, plan })).toBe(true);
      for (const read of reads) expect(accountPanel({ ...ended, plan }, read)).toBe("plans");
    }
    // The same account without a window keeps its subscriber view.
    expect(accountPanel({ ...free, plan: "pro" }, "none")).toBe("subscriber");
  });
  it("still routes an overdue payment to billing and ignores the window while signed out", () => {
    expect(accountPanel({ ...ended, payment_due: true }, "none")).toBe("payment");
    expect(accountPanel({ ...ended, signed_in: false, email: null, plan: null }, "none")).toBe("plans");
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

describe("account problems and manual checks", () => {
  const panels: AccountPanel[] = ["loading", "initializing", "plans", "payment", "subscriber", "billing"];
  it("offers a check except for a failure the app rechecks on its own", () => {
    expect(offersCheck(null, null)).toBe(true);
    expect(offersCheck(null, "account_restore_unavailable")).toBe(true);
    expect(offersCheck(null, "service_unsupported")).toBe(false);
    // A read that failed here can always be retried.
    expect(offersCheck("read failed", "service_unsupported")).toBe(true);
  });
  it("hides the error bar while a check runs or a panel already explains the problem", () => {
    for (const panel of ["attention", "checking"] as const) {
      expect(accountErrorBar(panel, "read failed", "sign in required", false)).toBeNull();
    }
    expect(accountErrorBar("subscriber", "read failed", null, true)).toBeNull();
    for (const panel of panels) expect(accountErrorBar(panel, null, null, false)).toBeNull();
  });
  it("shows a self-rechecking failure without a retry and other problems with one", () => {
    for (const panel of panels) {
      expect(accountErrorBar(panel, null, "service_unsupported", false)).toEqual({ check: false });
      expect(accountErrorBar(panel, null, "account_credentials_unsaved", false)).toEqual({ check: true });
      expect(accountErrorBar(panel, "read failed", null, false)).toEqual({ check: true });
    }
  });
});
