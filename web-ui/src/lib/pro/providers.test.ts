import { describe, expect, it } from "vitest";
import { agentsConnected, awaitingCloudUpdate, catalogRows, canDisconnect, canStartConnection, cloudUpdateLine, connectingLabel, connectionError, connectionSuccessCurrent, disconnectConnection, handoffKey, nextReadyHandoff, olderCloudSignIn, panelRows, pausedConnect, pendingConnection, providerLabel, providerLoginUrl, providersReady, recoverDisconnect, rememberedRows, sameConnection, signInGuided, stillAwaitingUpdate, type ProviderHandoff } from "./providers";
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
    expect(providerLoginUrl("github", "https://github.com/login/device")).toBe("https://github.com/login/device");
    for (const url of ["https://github.com.evil.test/login/device", "https://gist.github.com/login/device", "https://auth.openai.com/codex/device"]) expect(providerLoginUrl("github", url)).toBeNull();
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
  it("tells a half-pasted code apart from a failed sign-in", () => {
    expect(connectionError("authorization_code_incomplete")).not.toBe(connectionError("unknown_code"));
    expect(connectionError("authorization_code_incomplete")).not.toBe(connectionError("sign_in_not_confirmed"));
    expect(connectionError("device_auth_disabled")).toBe(connectionError("unknown_code"));
  });
  it("names each way a one-time-code sign-in can end apart from the generic line, in plain words", () => {
    const generic = connectionError("unknown_code");
    const codes = ["sign_in_unavailable", "sign_in_failed", "git_setup_failed", "sign_in_not_confirmed", "expired", "canceled"];
    for (const code of codes) expect(connectionError(code)).not.toBe(generic);
    expect(new Set(codes.map(code => connectionError(code))).size).toBe(codes.length);
    for (const code of [...codes, "provider_busy", "device_login_unavailable", "installation_failed", "installation_unavailable", "probe_timeout", "browser_login_unavailable", "unsupported", "SECRET raw stderr"]) {
      expect(connectionError(code)).not.toMatch(/provider|worker|keeper|terminal/i);
      expect(connectionError(code)).not.toContain("SECRET");
    }
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

describe("the last known connections", () => {
  const remembered = [
    { id: "claude", label: "Claude Code", category: "agent", state: "signed_in", methods: ["browser"], disconnect_supported: true },
    { id: "codex", label: "Codex", category: "agent", state: "needs_sign_in", methods: ["device_code"] },
    { id: "github", label: "GitHub", category: "repository", state: "missing", methods: ["device_code"] },
  ];
  it("keeps only well-formed remembered rows, each once, in the catalog's shape", () => {
    const rows = rememberedRows([...remembered,
      { id: "claude", label: "Again", category: "agent", state: "missing" },
      { id: "../x", label: "Bad", category: "agent", state: "missing" },
      { id: "future", label: "Future", category: "unknown_kind", state: "missing" },
      { id: "newer", label: "Newer", category: "agent", state: "a_newer_state", methods: ["ok", "bad method", 3] },
      { id: "nameless", label: "", category: "repository", state: "missing" },
      null, "text"]);
    expect(rows?.map(row => [row.id, row.label, row.state, row.methods])).toEqual([
      ["claude", "Claude Code", "signed_in", ["browser"]], ["codex", "Codex", "needs_sign_in", ["device_code"]], ["github", "GitHub", "missing", ["device_code"]],
      ["newer", "Newer", "unknown", ["ok"]], ["nameless", "nameless", "missing", []],
    ]);
    expect(rows?.[0]).toMatchObject({ installed: null, reason: null, checked_at: null, disconnect_supported: true });
    expect("disconnect_supported" in rows![1]).toBe(false);
    for (const nothing of [null, undefined, [], [null], "rows", {}]) expect(rememberedRows(nothing)).toBeNull();
    expect(rememberedRows(Array.from({ length: 40 }, (_, index) => ({ id: `p${index}`, label: "P", category: "agent", state: "missing" })))?.length).toBe(16);
  });
  it("shows the remembered rows at once and settled while the live read is pending or finds the cloud asleep", () => {
    const rows = rememberedRows(remembered)!;
    const live = [row("claude", "signed_in"), row("codex", "signed_in")];
    const base = { providers: [] as CloudProviderStatus[], remembered: rows, liveAnswered: false, loaded: false, current: false, asleep: false };
    // Pending: nothing live yet.
    expect(panelRows(base)).toEqual({ rows, fromMemory: true, unchecked: false, known: true, settled: true });
    // The live read found the cloud asleep or starting: still the remembered rows.
    expect(panelRows({ ...base, asleep: true })).toMatchObject({ rows, fromMemory: true, settled: true });
    // A live answer replaces them, and they never come back.
    expect(panelRows({ ...base, providers: live, liveAnswered: true, loaded: true, current: true })).toEqual({ rows: live, fromMemory: false, unchecked: false, known: true, settled: true });
    expect(panelRows({ ...base, providers: live, liveAnswered: true, loaded: true, asleep: true })).toMatchObject({ rows: live, settled: true });
    // Right after the user's own change, a live panel says it is checking.
    expect(panelRows({ ...base, providers: live, liveAnswered: true, loaded: true }).settled).toBe(false);
    // Nothing remembered and no answer yet: the neutral loading state.
    expect(panelRows({ ...base, remembered: null })).toMatchObject({ rows: [], fromMemory: false, unchecked: false, known: false });
    // A look that found the cloud idle names the catalog's providers, claiming no state.
    expect(panelRows({ ...base, remembered: null, asleep: true })).toEqual({ rows: catalogRows(), fromMemory: false, unchecked: true, known: true, settled: true });
    expect(panelRows({ ...base, asleep: true }).unchecked).toBe(false);
    expect(panelRows({ ...base, remembered: null, asleep: true, providers: live, loaded: true, liveAnswered: true }).unchecked).toBe(false);
  });
  it("names only the shared catalog's providers when nothing is known, with Connect still possible", () => {
    const rows = catalogRows();
    expect(rows.map(row => [row.id, row.category])).toEqual([["claude", "agent"], ["codex", "agent"], ["github", "repository"]]);
    for (const row of rows) {
      expect(row.label).toBe(providerLabel(row.id));
      expect(row).toMatchObject({ state: "unknown", installed: null, checked_at: null });
      // Never offered for disconnection, never counted as connected.
      expect(canDisconnect(row)).toBe(false);
    }
    expect(agentsConnected(rows)).toBeNull();
    expect(providersReady(rows)).toBe(false);
  });
  it("tells a connected agent from none and from unknown", () => {
    expect(agentsConnected([row("claude", "signed_in"), row("codex", "unknown")])).toBe(true);
    expect(agentsConnected([row("claude", "needs_sign_in"), row("codex", "unknown")])).toBeNull();
    expect(agentsConnected([row("claude", "needs_sign_in"), row("github", "signed_in", "repository")])).toBe(false);
  });
  it("names the action a pressed Connect is taking, never the cloud's machinery", () => {
    const agent = connectingLabel({ label: "Claude Code", category: "agent" });
    const repository = connectingLabel({ label: "GitHub", category: "repository" });
    expect(agent).toContain("Claude Code");
    expect(repository).toContain("GitHub");
    expect(agent).not.toBe(connectingLabel({ label: "Claude Code", category: "repository" }));
    for (const line of [agent, repository]) expect(line).not.toMatch(/machine|wak|asleep|start/i);
  });
});

describe("an older cloud", () => {
  const attempt = (phase: CloudProviderConnection["phase"], action: CloudProviderConnection["action"]): CloudProviderConnection => ({ id: "attempt", provider_id: "github", phase, expires_at: 1, action, error_code: null });
  it("recognizes only its sign-in step, never the cloud setting up an agent", () => {
    expect(olderCloudSignIn(attempt("waiting", { type: "terminal" }))).toBe(true);
    expect(olderCloudSignIn(attempt("preparing", { type: "terminal" }))).toBe(false);
    expect(olderCloudSignIn(attempt("waiting", { type: "device_code", verification_url: "https://github.com/login/device", user_code: "ABCD-1234" }))).toBe(false);
    expect(olderCloudSignIn(attempt("waiting", null))).toBe(false);
    expect(olderCloudSignIn(null)).toBe(false);
  });
  it("reads a catalog row offering only the older sign-in as waiting for the update", () => {
    const github = (methods: string[]): CloudProviderStatus => ({ ...row("github", "needs_sign_in", "repository"), methods });
    expect(awaitingCloudUpdate(github(["terminal"]))).toBe(true);
    expect(awaitingCloudUpdate(github(["device_code"]))).toBe(false);
    expect(awaitingCloudUpdate(github(["terminal", "device_code"]))).toBe(false);
    expect(awaitingCloudUpdate(github([]))).toBe(false);
    expect(signInGuided(github(["device_code"]))).toBe(true);
    expect(signInGuided({ ...row("claude", "needs_sign_in"), methods: ["browser_code"] })).toBe(true);
    expect(signInGuided(github(["terminal"]))).toBe(false);
  });
  it("brings Connect back only once a fresh catalog offers the one-time code", () => {
    const github = (methods: string[]): CloudProviderStatus => ({ ...row("github", "needs_sign_in", "repository"), methods });
    expect(stillAwaitingUpdate(["github"], [github(["terminal"])])).toEqual(["github"]);
    expect(stillAwaitingUpdate(["github"], [])).toEqual(["github"]);
    expect(stillAwaitingUpdate(["github"], [github(["device_code"])])).toEqual([]);
    expect(stillAwaitingUpdate(["github", "codex"], [github(["device_code"])])).toEqual(["codex"]);
  });
  it("says so in the row, in plain words", () => {
    expect(cloudUpdateLine("GitHub")).toBe("Your cloud is being updated. GitHub sign-in is available again in a few minutes.");
  });
});
