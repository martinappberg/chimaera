import { afterEach, describe, expect, it, vi } from "vitest";
import * as net from "../net/api";
import { acknowledgeSetupResult, getAgentSetup, pendingSetupId, recoverSetupResult, rememberSetupId, startAgentSetup } from "./agentSetupRequests";
import type { SetupProgress, SetupRequest } from "./agentSetup";
import type { AgentInfo } from "./launcher";

const operation = { id: "install-1", phase: "succeeded" } as SetupProgress;
const request = (agent: Partial<AgentInfo>, action: SetupRequest["action"] = "install"): SetupRequest => ({
  agent: { id: "codex", installed: false, ...agent } as AgentInfo,
  workspaceId: "workspace", action, showResult: false,
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  for (const agent of ["claude", "codex", "agy", "grok"]) {
    const id = pendingSetupId(agent);
    if (id) acknowledgeSetupResult(agent, id);
  }
});

describe("completed background setup", () => {
  it("recovers the result when the launcher still thinks the same job is running", () => {
    const stale = request({ setup: { id: operation.id, running: true, phase: "running" } });
    expect(recoverSetupResult(stale, operation, true)).toBe(true);
    expect(recoverSetupResult(stale, { ...operation, phase: "failed" }, false)).toBe(true);
  });
  it("recovers a successful install even when the launcher's progress poll was missed", () => {
    expect(recoverSetupResult(request({}), operation, true)).toBe(true);
  });
  it("allows a fresh install after removal and an explicit repair of an installed agent", () => {
    expect(recoverSetupResult(request({}), operation, false)).toBe(false);
    expect(recoverSetupResult(request({ installed: true }, "reinstall"), operation, true)).toBe(false);
  });
  it("keeps an explicitly requested historical result", () => {
    expect(recoverSetupResult({ ...request({}), showResult: true }, operation, false)).toBe(true);
  });
});

describe("setup recovery without a catalog refresh", () => {
  it.each(["succeeded", "failed"] as const)("recovers a completed %s update until its result is shown", async phase => {
    const update = request({ installed: true }, "update");
    vi.spyOn(net, "api").mockResolvedValue(Response.json({ ...operation, action: "update", phase: "running" }));
    await startAgentSetup(update, operation.id);
    // The catalog never learned this operation was running before the dialog closed.
    expect(recoverSetupResult(update, { ...operation, phase }, true)).toBe(true);
    acknowledgeSetupResult("codex", operation.id);
    expect(recoverSetupResult(update, { ...operation, phase }, true)).toBe(false);
  });

  it("remembers an ambiguous POST before waiting for its reply", async () => {
    vi.spyOn(net, "api").mockImplementation(async () => {
      expect(pendingSetupId("codex")).toBe(operation.id);
      throw new TypeError("Connection lost");
    });
    await expect(startAgentSetup(request({}), operation.id)).rejects.toThrow("Connection lost");
    expect(recoverSetupResult(request({}), { ...operation, phase: "failed" }, false)).toBe(true);
  });

  it("uses the returned ID when POST joins an existing operation", async () => {
    vi.spyOn(net, "api").mockResolvedValue(Response.json({ ...operation, phase: "running" }));
    await startAgentSetup(request({}), "other-request");
    acknowledgeSetupResult("codex", "other-request");
    expect(pendingSetupId("codex")).toBe(operation.id);
  });

  it("does not retain a request the daemon explicitly rejected", async () => {
    vi.spyOn(net, "api").mockResolvedValue(Response.json({ error: "Another workspace is installing" }, { status: 409 }));
    await expect(startAgentSetup(request({}), operation.id)).rejects.toBeInstanceOf(net.ApiError);
    expect(pendingSetupId("codex")).toBeNull();
  });

  it("keeps a discovered running operation through a page reload", async () => {
    vi.spyOn(net, "api").mockResolvedValue(Response.json({ host: "local", root: "/agents", operation: { ...operation, phase: "running" } }));
    await getAgentSetup("codex");
    vi.resetModules();
    const reloaded = await import("./agentSetupRequests");
    expect(reloaded.recoverSetupResult(request({ installed: true }, "update"), operation, true)).toBe(true);
    reloaded.acknowledgeSetupResult("codex", operation.id);
    expect(sessionStorage.getItem("chimaera.agentSetup.codex")).toBeNull();
  });

  it("retains in-tab recovery when browser storage is unavailable", () => {
    vi.stubGlobal("sessionStorage", {
      getItem() { throw new Error("Storage denied"); },
      setItem() { throw new Error("Storage denied"); },
      removeItem() { throw new Error("Storage denied"); },
    });
    rememberSetupId("codex", operation.id);
    expect(recoverSetupResult(request({}), { ...operation, phase: "failed" }, false)).toBe(true);
    acknowledgeSetupResult("codex", operation.id);
    expect(pendingSetupId("codex")).toBeNull();
  });
});
