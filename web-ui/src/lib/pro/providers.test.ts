import { describe, expect, it } from "vitest";
import { canDisconnect, canStartConnection, connectionError, connectionSuccessCurrent, disconnectConnection, handoffKey, nextReadyHandoff, pausedConnect, pendingConnection, providerLabel, providerLoginUrl, providersReady, recoverDisconnect, sameConnection, type ProviderHandoff } from "./providers";
import type { CloudProviderConnection, CloudProviderStatus } from "../net/native";
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

describe("cloud disconnection", () => {
  const attempt = (phase: CloudProviderConnection["phase"], operation?: CloudProviderConnection["operation"]): CloudProviderConnection => ({ id: "attempt", provider_id: "codex", phase, operation, expires_at: 1, action: null, error_code: null });
  it("requires an explicit daemon capability and permits clearing installed but unconfirmed credentials", () => {
    expect(canDisconnect(row("codex", "signed_in"))).toBe(false);
    expect(canDisconnect({...row("codex", "signed_in"), disconnect_supported: true})).toBe(true);
    expect(canDisconnect({...row("codex", "unknown"), disconnect_supported: true})).toBe(true);
    expect(canDisconnect({...row("codex", "unknown"), installed: null, disconnect_supported: true})).toBe(false);
    for (const state of ["needs_sign_in", "missing", "unavailable"] as const) expect(canDisconnect({...row("codex", state), disconnect_supported: true})).toBe(false);
  });
  it("keeps older attempts as sign-in and recognizes disconnect jobs without auth actions", () => {
    expect(disconnectConnection(attempt("preparing"))).toBe(false);
    expect(disconnectConnection(attempt("verifying", "disconnect"))).toBe(true);
    expect(pendingConnection(attempt("disconnected", "disconnect"))).toBe(false);
  });
  it("cannot turn a disconnect status reply into a login flow or another provider's attempt", () => {
    const disconnect = attempt("preparing", "disconnect");
    expect(sameConnection(disconnect, attempt("verifying", "disconnect"))).toBe(true);
    expect(sameConnection(disconnect, attempt("waiting"))).toBe(false);
    expect(sameConnection(disconnect, {...disconnect, provider_id: "claude"})).toBe(false);
    expect(sameConnection(disconnect, {...disconnect, id: "other"})).toBe(false);
    expect(sameConnection(disconnect, null)).toBe(false);
    expect(sameConnection(attempt("preparing"), attempt("waiting", "connect"))).toBe(true);
  });
  it("never retries an unfinished job even when its local polling deadline has passed", () => {
    for (const phase of ["preparing", "waiting", "verifying"] as const) expect(canStartConnection(attempt(phase, "disconnect"), true)).toBe(false);
    for (const phase of ["disconnected", "failed", "expired"] as const) {
      expect(canStartConnection(attempt(phase, "disconnect"), false)).toBe(false);
      expect(canStartConnection(attempt(phase, "disconnect"), true)).toBe(true);
    }
  });
  it("gives bounded disconnection recovery without directing a sign-in or exposing raw errors", () => {
    for (const code of ["provider_busy", "disconnect_failed", "disconnect_not_confirmed", "external_auth_unverified", "invalid_status", "expired", "SECRET raw stderr"]) {
      const text = connectionError(code, "disconnect");
      expect(text).not.toContain("SECRET");
      expect(text).not.toContain("Start again for a fresh request");
      expect(text).not.toContain("Open its sign-in flow");
    }
    expect(connectionError("external_auth_unverified", "disconnect")).toContain("managed outside Chimaera");
    expect(connectionError("provider_busy", "disconnect")).toContain("in progress");
  });
  it("does not keep old success claims after an external connection change or a failed refresh", () => {
    expect(connectionSuccessCurrent(attempt("disconnected", "disconnect"), [row("codex", "needs_sign_in")], true)).toBe(true);
    expect(connectionSuccessCurrent(attempt("disconnected", "disconnect"), [row("codex", "signed_in")], true)).toBe(false);
    expect(connectionSuccessCurrent(attempt("connected"), [row("codex", "needs_sign_in")], true)).toBe(false);
    expect(connectionSuccessCurrent(attempt("connected"), [row("codex", "signed_in")], false)).toBe(false);
    expect(connectionSuccessCurrent(attempt("disconnected", "disconnect"), [row("codex", "unknown")], true)).toBe(false);
  });
  it("recovers lost logout replies without replacing another live job or rolling back completion", () => {
    const pending = attempt("verifying", "disconnect");
    expect(recoverDisconnect(null, pending)).toBe(pending);
    const done = attempt("disconnected", "disconnect");
    expect(recoverDisconnect(done, pending)).toBe(done);
    const other = {...pending, id: "other", provider_id: "claude"};
    expect(recoverDisconnect(pending, other)).toBe(pending);
    expect(recoverDisconnect(done, other)).toBe(other);
    expect(recoverDisconnect(done, undefined)).toBe(done);
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
