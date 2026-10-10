import { describe, expect, it } from "vitest";
import { forgetFailure, forgetPlan } from "./hostForget";

describe("forgetPlan", () => {
  it("turns keep-connected off first for a kept host, and says so", () => {
    const plan = forgetPlan({ kept: true });
    expect(plan.turnOffKeep).toBe(true);
    expect(plan.question).toMatch(/Keep connected turns off too/);
  });

  it("only forgets a host that is not kept", () => {
    expect(forgetPlan({ kept: false })).toEqual({ turnOffKeep: false, question: "forget this host?" });
    expect(forgetPlan({})).toEqual({ turnOffKeep: false, question: "forget this host?" });
  });
});

describe("forgetFailure", () => {
  it("keeps the shell's own sentence", () => {
    expect(forgetFailure(new Error("Sign in first."))).toBe("Sign in first. This host is still here.");
  });

  it("never shows a code or an internal message", () => {
    expect(forgetFailure("keep_sign_in_again")).toMatch(/^Keep connected couldn’t be turned off/);
    expect(forgetFailure(new Error("Command pro_set_host_kept not found"))).toMatch(/^Keep connected couldn’t/);
  });
});
