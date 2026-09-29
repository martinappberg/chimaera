import { describe, it, expect } from "vitest";
import type { MirrorStatus, MirrorWorkspace } from "../net/native";
import { paid, readIntent, cloudCopy, cloudPollDelay, cloudProjectStatus, connectionWarningCopy, copyIssue, friendlyError, projectCopiesSetupLine, projectCopyError, alreadySubscribed, recoverableAccountRestore } from "./presentation";

// Copy is free to change; these tests pin which states read alike or apart,
// what takes precedence, and that nothing from a raw error reaches the page.

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
  it("distinguishes a disabled service, preparation and an unknown answer; idle reads as ready", () => {
    const disabled = cloudCopy("unavailable", "provisioning_disabled");
    const preparing = cloudCopy("preparing", null);
    const unknown = cloudCopy("unknown", null);
    expect(new Set([disabled.title, preparing.title, unknown.title, cloudCopy("ready", null).title]).size).toBe(4);
    expect(cloudCopy("ready", null)).toEqual(cloudCopy("sleeping", null));
    for (const copy of [disabled, preparing, unknown]) { expect(copy.title).not.toBe(""); expect(copy.detail).not.toBe(""); }
  });
  it("explains each limit separately instead of as a generic pause or a setup task", () => {
    const generic = cloudCopy("limited", null);
    const hours = cloudCopy("limited", "hours_exhausted");
    const storage = cloudCopy("limited", "storage_exhausted");
    expect(new Set([generic.detail, hours.detail, storage.detail]).size).toBe(3);
    expect(hours).not.toEqual(cloudCopy("preparing", null));
  });
  it("explains a credential save failure apart from an expired sign-in", () => {
    const unsaved = friendlyError("account_credentials_unsaved", "fallback");
    expect(unsaved).not.toBe("fallback");
    expect(unsaved).not.toBe(friendlyError("sign in required", "fallback"));
    expect(friendlyError("account_credentials_unsaved private-details", "fallback")).toBe("fallback");
  });
  it("never renders arbitrary service errors", () => {
    expect(friendlyError("account request rejected (503) secret=example", "Please retry.")).toBe("Please retry.");
    expect(friendlyError("account request rejected (409)", "Please retry.")).not.toBe("Please retry.");
    expect(alreadySubscribed("account request rejected (409)")).toBe(true);
    expect(alreadySubscribed(new Error("use_billing_portal"))).toBe(true);
    expect(alreadySubscribed("account request rejected (503)")).toBe(false);
  });
  it("explains known copy blockers without raw diagnostics", () => {
    const unknown = projectCopyError("private path=/sensitive token=example");
    expect(unknown).not.toMatch(/private|sensitive|token|retry/i);
    for (const known of ["workspace exceeds mirror storage quota", "Claude transcript is unavailable", "project setup needs attention in its terminal", "session archive exceeds mirror file limit"]) {
      expect(projectCopyError(known)).not.toBe(unknown);
    }
    expect(projectCopyError("Claude transcript is unavailable")).toBe(projectCopyError("Codex rollout is unavailable"));
  });
});


describe("cloud preparation progress", () => {
  it("keeps infrastructure phases out of user tasks and lets blockers take precedence", () => {
    for (const phase of ["keeper", "worker", "connecting"] as const) {
      expect(cloudCopy("preparing", null, phase)).toEqual(cloudCopy("preparing", null));
    }
    expect(cloudCopy("sleeping", null, "worker")).toEqual(cloudCopy("ready", null));
    expect(cloudCopy("preparing", "hours_exhausted", "worker")).toEqual(cloudCopy("limited", "hours_exhausted"));
  });
  const mirror = (workspaces: MirrorWorkspace[]): MirrorStatus => ({ configured: true, projects_root: "", projects_root_confirmed: false, workspaces, sessions: [] });
  const row = (id: string, at: number | null): MirrorWorkspace => ({ workspace_id: id, name: id, root: "/fixture", never_mirror: false, ownership: {state: "local", epoch: 1}, profile: null, mirror: { files: 4, bytes: 4096, excluded: 0, too_large: 0, last_mirrored_at: at, storage_limit_bytes: 10000, error: null } });
  it("does not invent syncing, completion or a task when no project status exists", () => {
    expect(cloudProjectStatus(null)).toBeNull();
    expect(cloudProjectStatus(mirror([]))).toBeNull();
    expect(cloudProjectStatus(mirror([{...row("private", 123), never_mirror: true}]))).toBeNull();
    expect(cloudProjectStatus(mirror([row("saved", 123)]), "other")).toBeNull();
  });
  it("reports completed copies as history, excludes private projects and scopes handoffs", () => {
    const saved = cloudProjectStatus(mirror([row("saved", 123)]));
    const unknown = cloudProjectStatus(mirror([row("new", null)]));
    expect(saved?.state).toBe("quiet");
    expect(unknown?.state).toBe("quiet");
    expect(unknown?.title).not.toBe(saved?.title);
    expect(cloudProjectStatus(mirror([{...row("remote", null), checkpoint_id:"c-confirmed"}]))).toEqual(saved);
    const privateProject = {...row("private", null), never_mirror: true};
    expect(cloudProjectStatus(mirror([row("saved", 123), privateProject]))).toEqual(saved);
    // Projects without a recorded copy yet add nothing to the saved summary.
    expect(cloudProjectStatus(mirror([row("saved", 123), row("new", null)]))).toEqual(saved);
    expect(cloudProjectStatus(mirror([row("saved", 123), row("new", null)]), "new")).toEqual(unknown);
    expect(cloudProjectStatus(mirror([row("a", 1), row("b", 2)]))?.detail).not.toBe(saved?.detail);
  });
  it("uses actual ownership for active progress and keeps privacy/configuration failures ahead of saved counts", () => {
    const starting = cloudProjectStatus({...mirror([row("saved", 123)]), configured: false});
    expect(starting?.state).toBe("active");
    const checking = {...row("saved", 123), ownership: {state: "awaiting_verification" as const, epoch: 2}};
    expect(cloudProjectStatus(mirror([checking]))?.state).toBe("active");
    const moving = {...row("saved", 123), ownership: {state: "transferring" as const, epoch: 2}};
    expect(cloudProjectStatus(mirror([moving]))?.state).toBe("active");
    const restoring = {...row("restore", 123), ownership: {state: "hydrating" as const, epoch: 2}};
    const restoringStatus = cloudProjectStatus(mirror([restoring]));
    expect(restoringStatus?.state).toBe("active");
    expect(new Set([starting?.title, restoringStatus?.title, cloudProjectStatus(mirror([moving]))?.title, cloudProjectStatus(mirror([checking]))?.title]).size).toBe(4);
    expect(cloudProjectStatus({...mirror([restoring]), configured: false})).toEqual(starting);
    const privacy = {...row("saved", 123), privacy_pending: true};
    // Turning copies off is progress the system finishes, never a problem to fix.
    expect(cloudProjectStatus(mirror([privacy]))?.state).toBe("active");
    const failed = row("saved", 123); failed.mirror!.error = "private diagnostic";
    const detail = cloudProjectStatus(mirror([failed]));
    expect(detail?.state).toBe("attention"); expect(detail?.detail).not.toContain("private diagnostic");
  });
  it("reads copy work still under way as quiet progress, never as a project needing attention", () => {
    const coded = (code: string | null): MirrorWorkspace => { const r = row("saved", 123); r.mirror!.error = "diagnostic"; r.mirror!.error_code = code; return r; };
    const saved = cloudProjectStatus(mirror([row("saved", 123)]));
    for (const code of ["pending", "checkpoint_pending", "ownership_unverified"]) {
      expect(copyIssue(coded(code).mirror)).toBe("progress");
      expect(cloudProjectStatus(mirror([coded(code)]))).toEqual(saved);
      // A real problem elsewhere still takes precedence.
      expect(cloudProjectStatus(mirror([coded(code), { ...coded("git_too_old"), workspace_id: "other" }]))?.state).toBe("attention");
    }
    for (const code of ["git_too_old", "credential_in_history", "conversation_not_saved", "other", null]) {
      expect(copyIssue(coded(code).mirror)).toBe("problem");
      expect(cloudProjectStatus(mirror([coded(code)]))?.state).toBe("attention");
    }
    expect(copyIssue(row("saved", 123).mirror)).toBe("none");
    expect(copyIssue(null)).toBe("none");
    expect(copyIssue(undefined)).toBe("none");
  });
  it("explains a failed cloud setup on its own, as a problem", () => {
    const setup = (): MirrorWorkspace => { const r = row("saved", 123); r.mirror!.error = "project setup failed"; r.mirror!.error_code = "cloud_setup_failed"; return r; };
    expect(copyIssue(setup().mirror)).toBe("problem");
    expect(cloudProjectStatus(mirror([setup()]))?.state).toBe("attention");
    const text = projectCopyError("project setup failed", "cloud_setup_failed");
    expect(text).not.toBe(projectCopyError("project setup failed"));
    expect(text).not.toBe(projectCopyError("x", "root_setup_required"));
  });
  it("reads a lapsed account connection as quiet progress, apart from first setup", () => {
    const renewing = { ...mirror([row("saved", 123)]), configured: false, renewal_failed: true };
    const starting = { ...mirror([row("saved", 123)]), configured: false };
    expect(cloudProjectStatus(renewing)?.state).toBe("active");
    expect(cloudProjectStatus(renewing)?.title).not.toBe(cloudProjectStatus(starting)?.title);
    expect(projectCopiesSetupLine(renewing)).not.toBeNull();
    expect(projectCopiesSetupLine(renewing)).not.toBe(projectCopiesSetupLine(starting));
    expect(projectCopiesSetupLine(mirror([]))).toBeNull();
    expect(projectCopiesSetupLine({ ...mirror([]), renewal_failed: true })).toBeNull();
    expect(projectCopiesSetupLine(null)).toBeNull();
  });
  it("gives each connection state its own quiet line", () => {
    const lines = ["connection_preparing", "connection_retrying", "account_unreachable"].map(connectionWarningCopy);
    expect(new Set(lines).size).toBe(3);
    for (const line of lines) expect(line).not.toBe("");
    expect(connectionWarningCopy("a_newer_code")).toBe(connectionWarningCopy("connection_preparing"));
  });
  it("limits rapid visible preparation checks to five minutes and keeps passive states slow", () => {
    expect(cloudPollDelay(true, 0)).toBe(5000);
    expect(cloudPollDelay(true, 299999)).toBe(5000);
    expect(cloudPollDelay(true, 300000)).toBe(30000);
    expect(cloudPollDelay(false, 0)).toBe(30000);
  });
});

describe("saved account recovery", () => {
  it("offers retry only for the two native restore outcomes", () => {
    expect(recoverableAccountRestore("account_restore_locked")).toBe(true);
    expect(recoverableAccountRestore("account_restore_unavailable")).toBe(true);
    for (const value of [null, "sign in required", "authorization revoked", "some upstream error"]) expect(recoverableAccountRestore(value)).toBe(false);
  });
});
