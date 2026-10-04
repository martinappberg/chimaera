import { describe, expect, it } from "vitest";
import { pausedConnect, providerLabel } from "./providers";

describe("provider names", () => {
  it("names a required provider from the catalog even when the daemon did not list it", () => {
    expect(providerLabel("claude")).not.toBe("claude");
    expect(providerLabel("future-provider")).toBe("future-provider");
  });
});

describe("paused sessions waiting for a sign-in", () => {
  const paused = { id: "s-1", workspace_id: "w-1", suspended: true };
  it("names the provider from the catalog and keeps the project for the connection flow", () => {
    expect(pausedConnect({ ...paused, blocked_provider: "claude" })).toEqual({ providerId: "claude", label: providerLabel("claude"), workspaceId: "w-1" });
    expect(pausedConnect({ ...paused, blocked_provider: "codex" })?.label).toBe(providerLabel("codex"));
    // An id the catalog does not know still opens the flow, named as sent.
    expect(pausedConnect({ ...paused, blocked_provider: "future-provider" })?.label).toBe("future-provider");
  });
  it("offers nothing to a row without the field, a live row or a malformed id", () => {
    for (const row of [paused, { ...paused, blocked_provider: null }, { ...paused, suspended: false, blocked_provider: "claude" }, { id: "s-1", blocked_provider: "claude" }, { ...paused, blocked_provider: "../claude" }, { ...paused, blocked_provider: "" }, null, "claude"]) {
      expect(pausedConnect(row)).toBeNull();
    }
  });
  it("never forwards an unsafe project id", () => {
    expect(pausedConnect({ ...paused, workspace_id: "../other", blocked_provider: "claude" })).toEqual({ providerId: "claude", label: providerLabel("claude") });
  });
});
