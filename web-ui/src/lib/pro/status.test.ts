import { describe, expect, it } from "vitest";
import { accountFailure, connectionWarningCode, rechecksItself } from "./status";

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
  it("recognizes only the unsupported service as rechecked by the app", () => {
    expect(rechecksItself("service_unsupported")).toBe(true);
    for (const error of [null, undefined, "", "account_restore_unavailable", "sign in required", "service_unsupported extra"]) {
      expect(rechecksItself(error)).toBe(false);
    }
  });
});
