import { describe, expect, it } from "vitest";
import { recoverSetupResult, type SetupProgress, type SetupRequest } from "./agentSetup";
import type { AgentInfo } from "./launcher";

const operation = { id: "install-1", phase: "succeeded" } as SetupProgress;
const request = (agent: Partial<AgentInfo>, action: SetupRequest["action"] = "install"): SetupRequest => ({
  agent: { id: "codex", installed: false, ...agent } as AgentInfo,
  workspaceId: "workspace", action, showResult: false,
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
