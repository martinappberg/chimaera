import { describe, it, expect } from "vitest";
import type { MirrorStatus, MirrorWorkspace } from "../net/native";
import { paid, readIntent, cloudAsleep, CLOUD_ASLEEP, afterFailedRead, MISSES_REPORTED, cloudCopy, cloudReadyOnce, cloudPollDelay, cloudProjectStatus, connectionWarningCopy, copyIssue, friendlyError, projectCopiesSetupLine, projectCopyError, projectPlace, returningLine, RETURN_WINDOW_ENDED_COPY, signInNoteCopy, alreadySubscribed, recoverableAccountRestore } from "./presentation";

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
  it("claims connected agents only when the app remembers one, and names the step when none", () => {
    const unknown = cloudCopy("sleeping", null);
    const connected = cloudCopy("sleeping", null, undefined, true);
    const none = cloudCopy("sleeping", null, undefined, false);
    expect(new Set([unknown.detail, connected.detail, none.detail]).size).toBe(3);
    // The connections section always shows, so the heading stays about availability.
    expect(new Set([unknown.title, connected.title, none.title]).size).toBe(1);
    // Idle and awake read alike for every agent fact.
    for (const agents of [null, true, false]) expect(cloudCopy("ready", null, undefined, agents)).toEqual(cloudCopy("sleeping", null, undefined, agents));
    // A limit still takes precedence over the agent step.
    expect(cloudCopy("sleeping", "hours_exhausted", undefined, false)).toEqual(cloudCopy("limited", "hours_exhausted"));
  });
  it("reads as setup only while an account's cloud has never been ready", () => {
    const first = cloudCopy("preparing", null, "worker", null, false);
    for (const agents of [null, true, false]) {
      // A later `preparing` (a service update, say) is the calm available state.
      expect(cloudCopy("preparing", null, "worker", agents, true)).toEqual(cloudCopy("ready", null, undefined, agents));
      expect(cloudCopy("preparing", null, undefined, agents, false)).toEqual(first);
    }
    expect(first.title).not.toBe(cloudCopy("ready", null).title);
    // Ready and idle never read as setup, whatever the memory says.
    expect(cloudCopy("ready", null, undefined, null, false)).toEqual(cloudCopy("ready", null, undefined, null, true));
    expect(cloudCopy("preparing", "hours_exhausted", undefined, null, false)).toEqual(cloudCopy("limited", "hours_exhausted"));
  });
  it("remembers a ready cloud from the app, or from an older app's catalog fact", () => {
    expect(cloudReadyOnce(null)).toBe(false);
    expect(cloudReadyOnce({ cloud_ready_once: true })).toBe(true);
    expect(cloudReadyOnce({ cloud_ready_once: false, agents_connected: true })).toBe(false);
    expect(cloudReadyOnce({})).toBe(false);
    expect(cloudReadyOnce({ agents_connected: false })).toBe(true);
    expect(cloudReadyOnce({ agents_connected: null, remembered_providers: [{ id: "claude", label: "Claude Code", category: "agent", state: "signed_in" }] })).toBe(true);
  });
  it("gives each ended sign-in its own quiet line and a signed-out sign-out its own sentence", () => {
    const notes = ["sign_in_timed_out", "sign_in_incomplete", "browser_unavailable"].map(signInNoteCopy);
    expect(new Set(notes).size).toBe(3);
    for (const note of notes) expect(note).not.toBeNull();
    expect(signInNoteCopy("sign in required")).toBeNull();
    expect(signInNoteCopy(null)).toBeNull();
    expect(friendlyError("sign_in_timed_out", "fallback")).toBe(signInNoteCopy("sign_in_timed_out"));
    const pending = friendlyError("sign_out_pending", "fallback");
    expect(pending).not.toBe("fallback");
    expect(pending).not.toBe(friendlyError("sign in required", "fallback"));
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
  it("reads an ended return window as one quiet note, never progress or a problem", () => {
    const ended = (): MirrorWorkspace => { const r = row("saved", 123); r.mirror!.error = "return_window_ended"; r.mirror!.error_code = "return_window_ended"; return r; };
    expect(copyIssue(ended().mirror)).toBe("note");
    expect(projectCopyError("return_window_ended", "return_window_ended")).toBe(RETURN_WINDOW_ENDED_COPY);
    // It never makes a project "need attention".
    expect(cloudProjectStatus(mirror([ended()]))?.state).not.toBe("attention");
  });
  it("explains a failed cloud setup on its own, as a problem", () => {
    const setup = (): MirrorWorkspace => { const r = row("saved", 123); r.mirror!.error = "project setup failed"; r.mirror!.error_code = "cloud_setup_failed"; return r; };
    expect(copyIssue(setup().mirror)).toBe("problem");
    expect(cloudProjectStatus(mirror([setup()]))?.state).toBe("attention");
    const text = projectCopyError("project setup failed", "cloud_setup_failed");
    expect(text).not.toBe(projectCopyError("project setup failed"));
    expect(text).not.toBe(projectCopyError("x", "root_setup_required"));
  });
  it("reads project setup as progress unless it waits on an agent or failed", () => {
    const setup = (patch: Partial<MirrorWorkspace> = {}, code: string | null = null): MirrorWorkspace => {
      const r = { ...row("saved", 123), ownership: { state: "setting_up" as const, epoch: 2 }, ...patch };
      if (code) { r.mirror = { ...r.mirror!, error: "diagnostic", error_code: code }; }
      return r;
    };
    const running = cloudProjectStatus(mirror([setup()]));
    expect(running?.state).toBe("active");
    expect(cloudProjectStatus(mirror([setup()]), undefined, "cloud")?.state).toBe("active");
    expect(cloudProjectStatus(mirror([setup()]), undefined, "cloud")?.title).not.toBe(running?.title);
    const failed = cloudProjectStatus(mirror([setup({}, "cloud_setup_failed")]));
    const waiting = cloudProjectStatus(mirror([setup({ blocked_providers: [{ id: "claude", state: "needs_sign_in", reason: null }] })]));
    expect(failed?.state).toBe("attention");
    expect(waiting?.state).toBe("attention");
    expect(failed?.title).not.toBe(waiting?.title);
    // A problem in one project outranks another project's normal setup.
    expect(cloudProjectStatus(mirror([setup(), { ...setup({}, "cloud_setup_failed"), workspace_id: "other" }]))).toEqual(failed);
    const broken = row("other", 123); broken.mirror!.error = "diagnostic"; broken.mirror!.error_code = "git_too_old";
    expect(cloudProjectStatus(mirror([setup(), broken]))?.state).toBe("attention");
    expect(projectPlace(setup())).not.toBe(projectPlace(setup({}, "cloud_setup_failed")));
    expect(projectPlace(setup({}, "cloud_setup_failed"))).not.toBe(projectPlace(setup({ blocked_providers: [{ id: "codex", state: "missing", reason: null }] })));
  });
  it("names who runs a project, and reads a pending privacy change as progress", () => {
    const local = projectPlace(row("a", 1));
    const remote = projectPlace({ ...row("a", 1), ownership: { state: "remote", epoch: 3 } });
    expect(local).not.toBe(remote);
    const kept = projectPlace({ ...row("a", 1), never_mirror: true });
    const keeping = projectPlace({ ...row("a", 1), never_mirror: true, privacy_pending: true });
    expect(new Set([local, remote, kept, keeping]).size).toBe(4);
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

describe("an ended plan's return window", () => {
  const until = "2026-11-03T09:30:00Z";
  const before = Date.parse("2026-10-20T12:00:00Z");
  it("says when the cloud work can still be brought home, in the person's own date format", () => {
    const line = returningLine(until, before, "en-US");
    expect(line).toMatch(/^Your plan has ended\. Bring your work home from the cloud by .*2026.*\.$/);
    expect(returningLine(until, before, "en-US")).not.toBe(returningLine(until, before, "de-DE"));
  });
  it("is absent without a window and reads as the code once the time has passed", () => {
    expect(returningLine(null, before)).toBeNull();
    expect(returningLine("soon", before)).toBeNull();
    expect(returningLine(until, Date.parse(until))).toBe(RETURN_WINDOW_ENDED_COPY);
    expect(returningLine(until, Date.parse(until) + 86_400_000)).toBe(RETURN_WINDOW_ENDED_COPY);
  });
  it("renders the return_window_ended code as one plain, quiet sentence wherever it arrives", () => {
    expect(RETURN_WINDOW_ENDED_COPY).toBe("The time to bring this work home has passed. Contact support.");
    expect(friendlyError(new Error("return_window_ended"), "fallback")).toBe(RETURN_WINDOW_ENDED_COPY);
    expect(friendlyError("return_window_ended", "fallback")).toBe(RETURN_WINDOW_ENDED_COPY);
    expect(friendlyError(new Error("return_window_ended and more"), "fallback")).toBe("fallback");
    expect(RETURN_WINDOW_ENDED_COPY).not.toMatch(/return_window_ended|epoch|window/i);
  });
});

describe("a failed account read", () => {
  it("keeps the last confirmed state and its calm copy after one miss, and reports only a second in a row", () => {
    const ready = { state: "ready", reason: null, agents_connected: true, cloud_ready_once: true } as const;
    const first = afterFailedRead(ready, 0);
    expect(first).toEqual({ status: ready, misses: 1, report: false });
    expect(cloudCopy(first.status!.state, null, undefined, true, true)).toEqual(cloudCopy("ready", null, undefined, true, true));
    // A later `preparing` (the service being updated) keeps its calm copy too.
    const updating = { state: "preparing", reason: null, cloud_ready_once: true } as const;
    expect(afterFailedRead(updating, 0).status).toBe(updating);
    const second = afterFailedRead(first.status, first.misses);
    expect(second.report).toBe(true);
    expect(second.misses).toBe(MISSES_REPORTED);
    expect(second.status?.state).toBe("error");
    expect(cloudCopy(second.status!.state, null).title).not.toBe(cloudCopy("ready", null).title);
    // Nothing confirmed yet: the first miss still waits for a second.
    expect(afterFailedRead(null, 0)).toEqual({ status: null, misses: 1, report: false });
    expect(afterFailedRead(null, 1).report).toBe(true);
  });
});

describe("a sleeping or starting cloud", () => {
  it("recognizes only the fixed code, from the native shell (a string) or the browser transport (an Error)", () => {
    expect(cloudAsleep(CLOUD_ASLEEP)).toBe(true);
    expect(cloudAsleep(new Error(CLOUD_ASLEEP))).toBe(true);
    for (const other of ["worker_asleep", "Couldn't complete cloud setup. Try again shortly.", new Error("provider_busy"), null, undefined]) expect(cloudAsleep(other)).toBe(false);
  });
  it("answers an action the user took quietly, never with the failure copy or machine words", () => {
    const failure = "This repository couldn’t open in the cloud. Check the URL and your Git access, then try again.";
    const line = friendlyError(CLOUD_ASLEEP, failure);
    expect(line).not.toBe(failure);
    expect(line).not.toMatch(/machine|waking|asleep|couldn|error|failed|worker|keeper/i);
    expect(friendlyError(new Error(CLOUD_ASLEEP), failure)).toBe(line);
    expect(friendlyError("something else", failure)).toBe(failure);
  });
});
