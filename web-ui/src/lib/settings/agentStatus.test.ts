import { describe, expect, it } from "vitest";
import { agentUpdateStatus, installationResult } from "./agentStatus";
import type { AgentInfo } from "../workspace/launcher";

const agent = { version: "grok 1.0.46", latestVersion: "1.0.46", updateAvailable: false } as AgentInfo;
describe("honest agent update status", () => {
  it("reports current only after a successful, comparable check", () => {
    expect(agentUpdateStatus(agent).current).toBe(true);
    for (const change of [
      { version: "built from source" }, { version: null },
      { latestVersion: null }, { latestVersion: "1.0.46-beta" },
      { latestError: "offline" }, { updateAvailable: true },
    ]) expect(agentUpdateStatus({ ...agent, ...change }).current).toBe(false);
  });
  it("keeps an unchecked release quiet without claiming it is current", () => {
    expect(agentUpdateStatus({ ...agent, latestVersion: null })).toEqual({ text: null, current: false });
  });
  it("does not mistake an unchanged version for a finished installation", () => {
    expect(installationResult({ running: true, exitStatus: null })).toBe("running");
    expect(installationResult({ running: false, exitStatus: 0 })).toBe("done");
    expect(installationResult({ running: false, exitStatus: 1 })).toBe("failed");
    expect(installationResult({ running: false, exitStatus: null })).toBe("unknown");
    expect(installationResult(undefined)).toBe("unknown");
  });
});
