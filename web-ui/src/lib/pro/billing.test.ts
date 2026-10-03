import { describe, expect, it } from "vitest";
import type { ProBillingAttempt, ProStatus } from "../net/native";
import { billingCopy, billingPending, billingNeedsReview, explicitCheckoutChoice, canReviewUpgrade, latestBilling, maxCapacityNote, planMultiples, planPrice, planPrices } from "./billing";
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

describe("plan multiples", () => {
  // Synthetic multiples: only their relations matter. The list carries no
  // absolute allowance, so none appears here or anywhere in the app.
  type Row = Parameters<typeof planMultiples>[0];
  const offer = (plan: "pro" | "max", cloud?: unknown, storage?: unknown, interval: "month" | "year" = "month") => ({
    plan, interval, amount_cents: 100, currency: "usd", cloud_time_multiple: cloud, storage_multiple: storage,
  });
  const catalog = (cloud?: unknown, storage?: unknown): Row => [
    offer("pro", 1, 1), offer("pro", 1, 1, "year"), offer("max", cloud, storage), offer("max", cloud, storage, "year"),
  ] as unknown as Row;
  const note = (plans: Row) => maxCapacityNote(plans, "en-US");

  it("reads Max's whole-number multiples of Pro", () => {
    expect(planMultiples(catalog(5, 3))).toEqual({ cloud: 5, storage: 3 });
    expect(planMultiples(catalog(4, undefined))).toEqual({ cloud: 4, storage: null });
    expect(planMultiples(catalog(undefined, 3))).toEqual({ cloud: null, storage: 3 });
    // Any Max entry will do: the yearly one carries it when the monthly does not.
    const yearOnly = [offer("max", undefined, undefined), offer("max", 5, 3, "year")] as unknown as Row;
    expect(planMultiples(yearOnly)).toEqual({ cloud: 5, storage: 3 });
    // Each number is read on its own, from whichever entry states it.
    const split = [offer("max", 5, undefined), offer("max", undefined, 3, "year")] as unknown as Row;
    expect(planMultiples(split)).toEqual({ cloud: 5, storage: 3 });
    // Pro's entries never stand in for Max's.
    expect(planMultiples([offer("pro", 5, 3)] as unknown as Row)).toEqual({ cloud: null, storage: null });
  });

  it("counts only a safe integer of at least 2; 1, 0 and malformed values are not a multiple", () => {
    expect(planMultiples(catalog(1, 1))).toEqual({ cloud: null, storage: null });
    for (const bad of [0, -3, 1.5, 2.5, "5", "lots", null, undefined, true, [5], {}, Number.NaN, Number.POSITIVE_INFINITY, Number.MAX_SAFE_INTEGER + 1]) {
      expect(planMultiples(catalog(bad, bad)), String(bad)).toEqual({ cloud: null, storage: null });
    }
    // One bad number leaves the other intact.
    expect(planMultiples(catalog("5", 3))).toEqual({ cloud: null, storage: 3 });
    expect(planMultiples(catalog(5, 0))).toEqual({ cloud: 5, storage: null });
    for (const absent of [undefined, null, []]) expect(planMultiples(absent)).toEqual({ cloud: null, storage: null });
    expect(planMultiples([null, 3, "max"] as unknown as Row)).toEqual({ cloud: null, storage: null });
  });

  it("says one shared multiple once, and two different ones each", () => {
    expect(note(catalog(5, 5))).toBe("5× the cloud time and storage of Pro");
    expect(note(catalog(5, 3))).toBe("5× the cloud time and 3× the storage of Pro");
    expect(note(catalog(3, 5))).toBe("3× the cloud time and 5× the storage of Pro");
  });

  it("names only the one number the service states", () => {
    expect(note(catalog(4, undefined))).toBe("4× the cloud time of Pro");
    expect(note(catalog(4, null))).toBe("4× the cloud time of Pro");
    expect(note(catalog(undefined, 3))).toBe("3× the storage of Pro");
    // A number that is not a multiple is left out, not shown as 1× or 0×.
    expect(note(catalog(4, 1))).toBe("4× the cloud time of Pro");
    expect(note(catalog(0, 3))).toBe("3× the storage of Pro");
    expect(note(catalog("5", 3))).toBe("3× the storage of Pro");
  });

  it("states nothing when there is nothing to compare, so the generic line stays", () => {
    expect(note(catalog(1, 1))).toBeNull();
    expect(note(catalog(0, 0))).toBeNull();
    expect(note(catalog("5", "3"))).toBeNull();
    expect(note(catalog())).toBeNull();
    for (const absent of [undefined, null, []]) expect(note(absent)).toBeNull();
    // An older service: prices only, no multiples anywhere.
    const older = [offer("pro"), offer("pro", undefined, undefined, "year"), offer("max"), offer("max", undefined, undefined, "year")] as unknown as Row;
    expect(note(older)).toBeNull();
    // Only Pro's entries state anything: Max is not compared with itself.
    expect(note([offer("pro", 5, 3)] as unknown as Row)).toBeNull();
  });

  it("does not depend on the billing interval", () => {
    const rows = catalog(5, 3) as unknown as Array<{ interval: string }>;
    const month = rows.filter(row => row.interval === "month") as unknown as Row;
    const year = rows.filter(row => row.interval === "year") as unknown as Row;
    expect(note(month)).toBe(note(year));
    expect(note(month)).not.toBeNull();
  });

  it("writes the number the way the reader's locale does, with no decimals", () => {
    expect(maxCapacityNote(catalog(1200, 1200), "en-US")).toBe("1,200× the cloud time and storage of Pro");
    expect(maxCapacityNote(catalog(5, 5), "de-DE")).toBe("5× the cloud time and storage of Pro");
    expect(maxCapacityNote(catalog(5, 3), "not a locale")).toBeNull();
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
