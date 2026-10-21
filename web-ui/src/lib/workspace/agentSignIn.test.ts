import { describe, expect, it } from "vitest";
import { accountLimited } from "./agentSignIn";

describe("accountLimited", () => {
  it("reads each agent's limit and plan words", () => {
    for (const text of [
      "You've hit your limit · resets 5pm (America/Los_Angeles)",
      "Claude AI usage limit reached|1760000000",
      "Credit balance is too low",
      "You've hit your usage limit. Upgrade to Pro or try again in 2 days.",
      "Your account does not have access to Claude Code. Please run /login.",
      "429 RESOURCE_EXHAUSTED: Quota exceeded for this account",
      "insufficient_quota",
    ]) {
      expect(accountLimited(text), text).toBe(true);
    }
  });

  it("leaves other failures alone", () => {
    for (const text of ["turn failed", "GLIBC_2.28 not found", "Not logged in · Please run /login", "", null, undefined]) {
      expect(accountLimited(text), String(text)).toBe(false);
    }
  });
});
