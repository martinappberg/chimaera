import { describe, it, expect } from "vitest";
import { paid, readIntent, cloudCopy, friendlyError } from "./presentation";

describe("purchase intent", () => {
  const intent = { plan: "max", interval: "year", stage: "sign_in", created: 1000 };
  it("preserves the explicitly selected plan across sign-in without trusting malformed or stale storage", () => {
    expect(readIntent(JSON.stringify(intent), 2000)).toEqual(intent);
    for (const invalid of [null, "broken", JSON.stringify({ ...intent, plan: "free" }), JSON.stringify({ ...intent, stage: "complete" }), JSON.stringify({ ...intent, created: 3000 })]) expect(readIntent(invalid, 2000)).toBeNull();
    expect(readIntent(JSON.stringify(intent), 1000 + 86400001)).toBeNull();
  });
  it("never treats no plan, absent status or an unknown future plan as entitlement", () => {
    expect(paid("pro")).toBe(true); expect(paid("max")).toBe(true);
    for (const value of [null, undefined, "none", "trial", "preparing"]) expect(paid(value)).toBe(false);
  });
});
describe("honest cloud state", () => {
  it("distinguishes disabled service from preparing and sleeping", () => {
    expect(cloudCopy("unavailable", "provisioning_disabled").title).toContain("isn't available");
    expect(cloudCopy("preparing", null).title).toContain("Preparing");
    expect(cloudCopy("sleeping", null).title).toContain("resting");
    expect(cloudCopy("unknown", null).title).toContain("unavailable");
  });
  it("explains quota recovery without converting it into an infrastructure setup task", () => {
    expect(cloudCopy("limited", "hours_exhausted").detail).toContain("resets next month");
    expect(cloudCopy("limited", "storage_exhausted").detail).toContain("local projects remain available");
  });
  it("never renders arbitrary service errors", () => {
    expect(friendlyError("account request rejected (503) secret=example", "Please retry.")).toBe("Please retry.");
    expect(friendlyError("account request rejected (409)", "Please retry.")).toContain("Manage billing");
  });
});
