import { describe, expect, it } from "vitest";
import { accountSurfaceCurrent } from "./accountOwner";
import type { CloudOnboardingContext } from "../pro/onboarding.svelte";

describe("original account presentation incarnation", () => {
  it("refuses close/reopen of the same visible tuple", () => {
    const tab = {}, original = { incarnation: tab, workspaceId: "project", intent: null };
    let value: typeof original | null = original;
    const current = accountSurfaceCurrent(original, () => value, () => true);
    expect(current()).toBe(true);
    value = null; expect(current()).toBe(false);
    value = { incarnation: {}, workspaceId: "project", intent: null };
    expect(current()).toBe(false);
  });
  it("cannot complete a successor intent with identical provider/project fields", () => {
    const intent: CloudOnboardingContext = { providerIds: ["github"], workspaceId: "project" };
    const original = { incarnation: {}, workspaceId: "project", intent };
    let value = original, host = true;
    const current = accountSurfaceCurrent(original, () => value, () => host);
    expect(current()).toBe(true);
    value = { ...original, intent: { providerIds: ["github"], workspaceId: "project" } };
    expect(current()).toBe(false);
    value = original; host = false; expect(current()).toBe(false);
  });
});
