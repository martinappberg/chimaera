import { describe, it, expect } from "vitest";
import { paid, readIntent, cloudCopy, cloudPollDelay, cloudStages, friendlyError } from "./presentation";

describe("purchase intent", () => {
  const intent = { plan: "max", interval: "year", stage: "sign_in", created: 1000 };
  it("preserves the explicitly selected plan across sign-in without trusting malformed or stale storage", () => {
    expect(readIntent(JSON.stringify(intent), 2000)).toEqual(intent);
    for (const invalid of [null, "broken", JSON.stringify({ ...intent, plan: "free" }), JSON.stringify({ ...intent, stage: "complete" }), JSON.stringify({ ...intent, created: 3000 })]) expect(readIntent(invalid, 2000)).toBeNull();
    expect(readIntent(JSON.stringify(intent), 1000 + 86400001)).toBeNull();
  });
  it("never treats no plan, absent status or an unknown future plan as entitlement", () => {
    expect(paid("pro")).toBe(true); expect(paid("max")).toBe(true);
    for (const value of [null, undefined, "none", "trial", "preparing"]) expect(paid(value)).toBe(false);
  });
});
describe("honest cloud state", () => {
  it("distinguishes disabled service from preparing and sleeping", () => {
    expect(cloudCopy("unavailable", "provisioning_disabled").title).toContain("paused");
    expect(cloudCopy("preparing", null).title).toContain("Preparing");
    expect(cloudCopy("sleeping", null).title).toContain("resting");
    expect(cloudCopy("unknown", null).title).toContain("unavailable");
  });
  it("explains quota recovery without converting it into an infrastructure setup task", () => {
    expect(cloudCopy("limited", "hours_exhausted").detail).toContain("resets next month");
    expect(cloudCopy("limited", "storage_exhausted").detail).toContain("local projects remain available");
  });
  it("never renders arbitrary service errors", () => {
    expect(friendlyError("account request rejected (503) secret=example", "Please retry.")).toBe("Please retry.");
    expect(friendlyError("account request rejected (409)", "Please retry.")).toContain("Manage billing");
  });
});


describe("cloud preparation progress", () => {
  it("uses a reported phase only during preparation and lets blockers take precedence", () => {
    expect(cloudCopy("preparing", null, "keeper").title).toBe("Preparing your cloud connection");
    expect(cloudCopy("preparing", null, "worker").title).toBe("Starting your cloud machine");
    expect(cloudCopy("preparing", null, "connecting").title).toBe("Connecting your cloud workbench");
    expect(cloudCopy("preparing", null).title).toBe("Preparing your cloud");
    expect(cloudCopy("sleeping", null, "worker").title).toContain("resting");
    expect(cloudCopy("preparing", "hours_exhausted", "worker").title).toContain("hours used");
  });
  it("does not infer agent or project readiness from an available machine", () => {
    expect(cloudStages(false, true, null, false).map(s => s.state)).toEqual(["current", "pending", "pending"]);
    expect(cloudStages(true, null, null, false).map(s => s.state)).toEqual(["complete", "current", "pending"]);
    const agent = cloudStages(true, true, null, false);
    expect(agent[1].state).toBe("complete");
    expect(agent[2].state).toBe("current");
    expect(agent[2].title).toBe("Continue your projects");
    expect(cloudStages(true, true, null, true)[2].detail).toContain("on this cloud machine");
  });
  it("reports only completed copies, excludes private projects, and scopes project handoffs", () => {
    const mirror = (workspaces: import("../net/native").MirrorWorkspace[]): import("../net/native").MirrorStatus => ({ configured: true, projects_root: "", projects_root_confirmed: false, workspaces, sessions: [] });
    const row = (id: string, at: number | null): import("../net/native").MirrorWorkspace => ({ workspace_id: id, name: id, root: "/fixture", never_mirror: false, ownership: {state: "local", epoch: 1}, profile: null, mirror: { files: 4, bytes: 4096, excluded: 0, too_large: 0, last_mirrored_at: at, storage_limit_bytes: 10000, error: null } });
    expect(cloudStages(true, true, mirror([row("new", null)]), false)[2].title).toBe("Waiting for a project copy");
    expect(cloudStages(true, true, mirror([row("saved", 123)]), false)[2]).toMatchObject({ title: "Project copies saved", state: "complete" });
    const privateProject = {...row("private", null), never_mirror: true};
    expect(cloudStages(true, true, mirror([row("saved", 123), privateProject]), false)[2].detail).toContain("1 project has");
    const both = mirror([row("saved", 123), row("new", null)]);
    expect(cloudStages(true, true, both, false)[2].state).toBe("current");
    expect(cloudStages(true, true, both, false, "new")[2].title).toBe("Waiting for a project copy");
    const paused = {...mirror([row("saved", 123)]), configured: false};
    expect(cloudStages(true, true, paused, false)[2]).toMatchObject({title: "Project connection pending", state: "current"});
    const checking = {...row("saved", 123), ownership: {state: "awaiting_verification" as const, epoch: 2}};
    expect(cloudStages(true, true, mirror([checking]), false)[2].title).toBe("Checking project ownership");
    const moving = {...row("saved", 123), ownership: {state: "transferring" as const, epoch: 2}};
    expect(cloudStages(true, true, mirror([moving]), true)[2].detail).toContain("other machine");
    const restoring = {...row("restore", 123), ownership: {state: "hydrating" as const, epoch: 2}};
    expect(cloudStages(true, true, mirror([restoring]), false)[2].title).toBe("Restoring your project");
    expect(cloudStages(true, true, {...mirror([restoring]), configured: false}, false)[2].title).toBe("Project connection pending");
    const failed = row("saved", 123); failed.mirror!.error = "private diagnostic";
    const detail = cloudStages(true, true, mirror([failed]), false)[2];
    expect(detail.state).toBe("current"); expect(detail.detail).not.toContain("private diagnostic");
  });
  it("limits rapid visible preparation checks to five minutes and keeps passive states slow", () => {
    expect(cloudPollDelay(true, 0)).toBe(5000);
    expect(cloudPollDelay(true, 299999)).toBe(5000);
    expect(cloudPollDelay(true, 300000)).toBe(30000);
    expect(cloudPollDelay(false, 0)).toBe(30000);
  });
});
