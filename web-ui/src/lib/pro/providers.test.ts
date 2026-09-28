import { describe, expect, it } from "vitest";
import { providerLoginUrl, providersReady } from "./providers";
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
