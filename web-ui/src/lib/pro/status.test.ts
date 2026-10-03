import { describe, expect, it } from "vitest";
import { accountFailure, connectionWarningCode, grantedPlan, keeperRestartAt, planEnded, rechecksItself, returningUntil, signInNote } from "./status";

const connectionWarning = (status: Parameters<typeof connectionWarningCode>[0]) => connectionWarningCode(status) !== null;

const preparing = "You're signed in. Your Pro connection is preparing; Chimaera will reconnect automatically.";
const retrying = "Your account is up to date. The Pro connection is not ready yet; Chimaera will retry automatically.";

describe("account status interpretation", () => {
  it("treats the connection coming up as informational, not an account failure", () => {
    for (const status of [{ error: preparing }, { error: retrying }, { error: null, connection_warning: "starting" }]) {
      expect(accountFailure(status)).toBeNull();
      expect(connectionWarning(status)).toBe(true);
    }
  });
  it("keeps every other error a failure", () => {
    for (const error of ["account_restore_locked", "sign in required", `${preparing} extra`]) {
      expect(accountFailure({ error })).toBe(error);
      expect(connectionWarning({ error })).toBe(false);
    }
    expect(accountFailure({ error: "sign in required", connection_warning: "starting" })).toBe("sign in required");
  });
  it("is quiet for a healthy or absent status", () => {
    for (const status of [null, undefined, { error: null }, { error: null, connection_warning: null }, { error: null, connection_warning: "" }]) {
      expect(accountFailure(status)).toBeNull();
      expect(connectionWarning(status)).toBe(false);
    }
  });
  it("names each informational state, older shells' messages included", () => {
    expect(connectionWarningCode({ error: preparing })).toBe(connectionWarningCode({ error: null, connection_warning: "connection_preparing" }));
    expect(connectionWarningCode({ error: retrying })).toBe(connectionWarningCode({ error: null, connection_warning: "connection_retrying" }));
    expect(connectionWarningCode({ error: preparing })).not.toBe(connectionWarningCode({ error: retrying }));
    expect(connectionWarningCode({ error: null, connection_warning: "account_unreachable" })).toBe("account_unreachable");
    expect(connectionWarningCode({ error: "sign in required" })).toBeNull();
  });
  it("reads a sign-in attempt that ended as a note while signed out, never an account failure", () => {
    for (const error of ["sign_in_timed_out", "sign_in_incomplete", "browser_unavailable"]) {
      expect(accountFailure({ error })).toBeNull();
      expect(signInNote({ error, signed_in: false })).toBe(error);
      expect(signInNote({ error, signed_in: true })).toBeNull();
    }
    for (const error of [null, "sign in required", "account_restore_locked", "sign_in_timed_out extra"]) expect(signInNote({ error, signed_in: false })).toBeNull();
  });
  it("recognizes only the unsupported service as rechecked by the app", () => {
    expect(rechecksItself("service_unsupported")).toBe(true);
    for (const error of [null, undefined, "", "account_restore_unavailable", "sign in required", "service_unsupported extra"]) {
      expect(rechecksItself(error)).toBe(false);
    }
  });
});

describe("an ended plan inside its return window", () => {
  const until = "2026-11-03T09:30:00Z";
  it("reads the account's time only while signed in, and only when it is a time", () => {
    expect(returningUntil({ signed_in: true, returning_until: until })).toBe(until);
    for (const status of [null, undefined, { signed_in: true }, { signed_in: true, returning_until: null }, { signed_in: false, returning_until: until }, { signed_in: true, returning_until: "soon" }, { signed_in: true, returning_until: "" }]) {
      expect(returningUntil(status)).toBeNull();
      expect(planEnded(status)).toBe(false);
    }
    expect(planEnded({ signed_in: true, returning_until: until })).toBe(true);
  });
  it("grants no plan once it has ended, even while the account still names it", () => {
    for (const plan of ["pro", "max", "none"] as const) {
      expect(grantedPlan({ signed_in: true, plan, returning_until: until })).toBe("none");
      // Unchanged for every account without a return window.
      expect(grantedPlan({ signed_in: true, plan })).toBe(plan);
      expect(grantedPlan({ signed_in: true, plan, returning_until: null })).toBe(plan);
    }
    expect(grantedPlan({ signed_in: true, plan: null })).toBeNull();
    expect(grantedPlan(null)).toBeUndefined();
  });
});

describe("a planned restart of the cloud connection", () => {
  it("reads the account's time, future or past, only while signed in", () => {
    for (const at of ["2026-10-01T02:00:00Z", "2020-01-01T00:00:00+02:00"]) {
      expect(keeperRestartAt({ signed_in: true, keeper_restart_at: at })).toBe(at);
      expect(keeperRestartAt({ signed_in: false, keeper_restart_at: at })).toBeNull();
    }
  });
  it("is absent when the account omits it, sends null, or sends something that is not a time", () => {
    expect(keeperRestartAt({ signed_in: true })).toBeNull();
    expect(keeperRestartAt({ signed_in: true, keeper_restart_at: null })).toBeNull();
    for (const bad of ["tonight", "", "soon-ish", 1_790_000_000, true, { at: "2026-10-01T02:00:00Z" }]) {
      expect(keeperRestartAt({ signed_in: true, keeper_restart_at: bad as unknown as string })).toBeNull();
    }
    expect(keeperRestartAt(null)).toBeNull();
    expect(keeperRestartAt(undefined)).toBeNull();
  });
});
