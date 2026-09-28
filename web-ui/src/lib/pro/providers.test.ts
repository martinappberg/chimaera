import { describe, expect, it } from "vitest";
import { handoffKey, nextReadyHandoff, providerLoginUrl, providersReady, type ProviderHandoff } from "./providers";
import type { CloudProviderStatus } from "../net/native";
const row = (id: string, state: CloudProviderStatus["state"], category: CloudProviderStatus["category"] = "agent"): CloudProviderStatus => ({ id, label: id, state, category, installed: true, reason: null, checked_at: 1, methods: ["device_code"] });
describe("provider readiness", () => {
  it("accepts one agent initially but requires every provider used by a handoff", () => {
    const rows = [row("claude", "signed_in"), row("codex", "needs_sign_in")];
    expect(providersReady(rows)).toBe(true);
    expect(providersReady(rows, ["claude", "codex"])).toBe(false);
    expect(providersReady(rows, ["future-provider"])).toBe(false);
    expect(providersReady([...rows, row("future-provider", "signed_in")], ["future-provider"])).toBe(true);
  });
  it("never treats installation, unknown state or repository login as agent readiness", () => {
    expect(providersReady([row("claude", "unknown"), row("codex", "missing"), row("github", "signed_in", "repository")])).toBe(false);
  });
  it("rejects credential-bearing and deceptive provider links", () => {
    expect(providerLoginUrl("codex", "https://auth.openai.com/codex/device")).toBe("https://auth.openai.com/codex/device");
    for (const url of ["http://auth.openai.com/codex/device", "https://auth.openai.com.evil.test", "https://x@auth.openai.com", "https://auth.openai.com:444/", "javascript:alert(1)", "https://auth.openai.com/#fragment", "https://auth.openai.com/" + "x".repeat(4096)]) expect(providerLoginUrl("codex", url)).toBeNull();
    expect(providerLoginUrl("claude", "https://claude.com/cai/oauth/authorize?state=fixture")).toBe("https://claude.com/cai/oauth/authorize?state=fixture");
    expect(providerLoginUrl("claude", "https://claude.com.evil.test/cai/oauth/authorize")).toBeNull();
    expect(providerLoginUrl("future-provider", "https://auth.openai.com")).toBeNull();
  });
});

describe("automatic provider-blocked continuation", () => {
  const handoff: ProviderHandoff = { workspace_id: "project", name: "Project", expected_epoch: 7, blocked_providers: [{id: "claude", state: "needs_sign_in", reason: null}, {id: "codex", state: "needs_sign_in", reason: null}] };
  const connected = [row("claude", "signed_in"), row("codex", "signed_in")];
  it("requires a fresh complete provider confirmation for an existing scoped transfer", () => {
    expect(nextReadyHandoff(connected, [handoff], [], true)).toBe(handoff);
    expect(nextReadyHandoff(connected, [handoff], [], false)).toBeUndefined();
    expect(nextReadyHandoff([row("claude", "signed_in"), row("codex", "unknown")], [handoff], [], true)).toBeUndefined();
    for (const invalid of [{...handoff, blocked_providers: []}, {...handoff, workspace_id: ""}, {...handoff, expected_epoch: 0}, {...handoff, expected_epoch: 1.5}]) {
      expect(nextReadyHandoff(connected, [invalid], [], true)).toBeUndefined();
    }
  });
  it("attempts each epoch once and serially advances to other eligible projects", () => {
    const second = {...handoff, workspace_id: "second"};
    expect(nextReadyHandoff(connected, [handoff], [handoffKey(handoff)], true)).toBeUndefined();
    expect(nextReadyHandoff(connected, [handoff, second], [handoffKey(handoff)], true)).toBe(second);
    const later = {...handoff, expected_epoch: 8};
    expect(nextReadyHandoff(connected, [later], [handoffKey(handoff)], true)).toBe(later);
  });
});
