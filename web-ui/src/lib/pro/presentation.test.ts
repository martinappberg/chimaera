import { describe, it, expect } from "vitest";
import { paid, readIntent, cloudCopy, cloudPollDelay, cloudProjectStatus, friendlyError, projectCopyError } from "./presentation";

describe("purchase intent", () => {
  const intent = { plan: "max", interval: "year", stage: "sign_in", created: 1000 };
  it("preserves the explicitly selected plan across sign-in without trusting malformed or stale storage", () => {
    expect(readIntent(JSON.stringify(intent), 2000)).toEqual(intent);
    for (const invalid of [null, "broken", JSON.stringify({ ...intent, plan: "free" }), JSON.stringify({ ...intent, stage: "complete" }), JSON.stringify({ ...intent, stage: "checkout" }), JSON.stringify({ ...intent, created: 3000 })]) expect(readIntent(invalid, 2000)).toBeNull();
    expect(readIntent(JSON.stringify(intent), 1000 + 86400001)).toBeNull();
  });
  it("retains signup versus signin for retry without turning either draft into a checkout request", () => {
    for (const screenHint of ["sign-up", "sign-in"] as const) {
      expect(readIntent(JSON.stringify({ ...intent, screenHint }), 2000)).toEqual({ ...intent, screenHint });
    }
    expect(readIntent(JSON.stringify({ ...intent, screenHint: "admin" }), 2000)).toBeNull();
    expect(readIntent(JSON.stringify({ ...intent, stage: "checkout", screenHint: "sign-up" }), 2000)).toBeNull();
  });
  it("never treats no plan, absent status or an unknown future plan as entitlement", () => {
    expect(paid("pro")).toBe(true); expect(paid("max")).toBe(true);
    for (const value of [null, undefined, "none", "trial", "preparing"]) expect(paid(value)).toBe(false);
  });
});
describe("honest cloud state", () => {
  it("distinguishes disabled service from preparing and sleeping", () => {
    expect(cloudCopy("unavailable", "provisioning_disabled").title).toContain("unavailable");
    expect(cloudCopy("preparing", null).title).toBe("Getting things ready");
    expect(cloudCopy("sleeping", null).title).toBe("Available when you need it");
    expect(cloudCopy("ready", null)).toEqual(cloudCopy("sleeping", null));
    expect(cloudCopy("unknown", null).title).toContain("unavailable");
  });
  it("explains quota recovery without converting it into an infrastructure setup task", () => {
    expect(cloudCopy("limited", "hours_exhausted").detail).toContain("resets next month");
    expect(cloudCopy("limited", "storage_exhausted").detail).toContain("local projects remain available");
  });
  it("explains a credential save failure without claiming authentication expired", () => {
    const copy = friendlyError("account_credentials_unsaved", "fallback");
    expect(copy).toContain("You’re signed in");
    expect(copy).toContain("Check again");
    expect(copy).toContain("after restarting");
    expect(copy).not.toContain("expired");
    expect(friendlyError("account_credentials_unsaved private-details", "fallback")).toBe("fallback");
  });
  it("never renders arbitrary service errors", () => {
    expect(friendlyError("account request rejected (503) secret=example", "Please retry.")).toBe("Please retry.");
    expect(friendlyError("account request rejected (409)", "Please retry.")).toContain("Manage billing");
  });
  it("explains known copy blockers without raw diagnostics or false retry promises", () => {
    expect(projectCopyError("workspace exceeds mirror storage quota")).toContain("cloud allowance");
    expect(projectCopyError("Claude transcript is unavailable")).toContain("conversation");
    expect(projectCopyError("project setup needs attention in its terminal")).toContain("setup didn’t finish");
    const unknown = projectCopyError("private path=/sensitive token=example");
    expect(unknown).toContain("incomplete");
    expect(unknown).not.toMatch(/private|sensitive|token|retry/i);
  });
});


describe("cloud preparation progress", () => {
  it("keeps infrastructure phases out of user tasks and lets blockers take precedence", () => {
    for (const phase of ["keeper", "worker", "connecting"] as const) {
      expect(cloudCopy("preparing", null, phase)).toEqual(cloudCopy("preparing", null));
    }
    expect(cloudCopy("sleeping", null, "worker").title).toBe("Available when you need it");
    expect(cloudCopy("preparing", "hours_exhausted", "worker").title).toContain("allowance used");
  });
  const mirror = (workspaces: import("../net/native").MirrorWorkspace[]): import("../net/native").MirrorStatus => ({ configured: true, projects_root: "", projects_root_confirmed: false, workspaces, sessions: [] });
  const row = (id: string, at: number | null): import("../net/native").MirrorWorkspace => ({ workspace_id: id, name: id, root: "/fixture", never_mirror: false, ownership: {state: "local", epoch: 1}, profile: null, mirror: { files: 4, bytes: 4096, excluded: 0, too_large: 0, last_mirrored_at: at, storage_limit_bytes: 10000, error: null } });
  it("does not invent syncing, completion or a task when no project status exists", () => {
    expect(cloudProjectStatus(null)).toBeNull();
    expect(cloudProjectStatus(mirror([]))).toBeNull();
    expect(cloudProjectStatus(mirror([{...row("private", 123), never_mirror: true}]))).toBeNull();
    expect(cloudProjectStatus(mirror([row("saved", 123)]), "other")).toBeNull();
  });
  it("reports completed copies as history, excludes private projects and scopes handoffs", () => {
    expect(cloudProjectStatus(mirror([row("new", null)]))).toMatchObject({title: "Waiting for a project copy", state: "quiet"});
    expect(cloudProjectStatus(mirror([row("saved", 123)]))).toMatchObject({ title: "Cloud copies saved", state: "quiet" });
    const privateProject = {...row("private", null), never_mirror: true};
    expect(cloudProjectStatus(mirror([row("saved", 123), privateProject]))?.detail).toContain("1 project has");
    const both = mirror([row("saved", 123), row("new", null)]);
    expect(cloudProjectStatus(both)?.detail).toContain("Other projects have not reported");
    expect(cloudProjectStatus(both, "new")?.title).toBe("Waiting for a project copy");
  });
  it("uses actual ownership for active progress and keeps privacy/configuration failures ahead of saved counts", () => {
    const paused = {...mirror([row("saved", 123)]), configured: false};
    expect(cloudProjectStatus(paused)).toMatchObject({title: "Project connection pending", state: "attention"});
    const checking = {...row("saved", 123), ownership: {state: "awaiting_verification" as const, epoch: 2}};
    expect(cloudProjectStatus(mirror([checking]))).toMatchObject({title: "Checking your project", state: "active"});
    const moving = {...row("saved", 123), ownership: {state: "transferring" as const, epoch: 2}};
    expect(cloudProjectStatus(mirror([moving]))?.detail).toContain("files and conversation");
    const restoring = {...row("restore", 123), ownership: {state: "hydrating" as const, epoch: 2}};
    expect(cloudProjectStatus(mirror([restoring]))).toMatchObject({title: "Restoring your project", state: "active"});
    expect(cloudProjectStatus({...mirror([restoring]), configured: false})?.title).toBe("Project connection pending");
    const privacy = {...row("saved", 123), privacy_pending: true};
    expect(cloudProjectStatus(mirror([privacy]))?.state).toBe("attention");
    const failed = row("saved", 123); failed.mirror!.error = "private diagnostic";
    const detail = cloudProjectStatus(mirror([failed]));
    expect(detail?.state).toBe("attention"); expect(detail?.detail).not.toContain("private diagnostic");
  });
  it("limits rapid visible preparation checks to five minutes and keeps passive states slow", () => {
    expect(cloudPollDelay(true, 0)).toBe(5000);
    expect(cloudPollDelay(true, 299999)).toBe(5000);
    expect(cloudPollDelay(true, 300000)).toBe(30000);
    expect(cloudPollDelay(false, 0)).toBe(30000);
  });
});
