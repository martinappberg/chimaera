import { describe, expect, it } from "vitest";
import { agentUpdateStatus, installationResult } from "./agentStatus";
import { releaseCore, versionNumber, type AgentInfo } from "../workspace/launcher";

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
  it("reads a dated build by its date and calls only the same build current", () => {
    const cursor = { version: "2026.10.14-941492e", latestVersion: "2026.10.14-941492e", updateAvailable: false } as AgentInfo;
    expect(versionNumber(cursor.version!)).toBe("2026.10.14");
    expect(versionNumber("1.2.3-beta")).toBe("1.2.3-beta");
    // Pinned in step with the daemon's release_core rule.
    for (const [token, core] of [
      ["2026.10.14-941492e", "2026.10.14"], ["2026.10.14-941492E", "2026.10.14"],
      ["1.2.3-20261014", "1.2.3"], ["1.2.3-beta", "1.2.3-beta"],
      ["1.2.3-abcdef", "1.2.3-abcdef"], ["1.2-abcdef0", "1.2-abcdef0"], ["1.2.3", "1.2.3"],
    ]) expect(releaseCore(token)).toBe(core);
    expect(agentUpdateStatus(cursor)).toEqual({ text: "Up to date", current: true });
    expect(agentUpdateStatus({ ...cursor, latestVersion: "2026.10.14-0a1b2c3" }).current).toBe(false);
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
