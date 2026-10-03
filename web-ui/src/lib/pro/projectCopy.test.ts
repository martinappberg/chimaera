import { describe, it, expect } from "vitest";
import { projectCopyError, stagingPresentation, takeoverEpoch } from "./projectCopy";
describe("local project copy presentation", () => {
  it("keeps missing or future staging evidence neutral", () => {
    expect(stagingPresentation(undefined)).toBeNull();
    expect(stagingPresentation({ state: "future" })).toBeNull();
    expect(stagingPresentation({ state: "uncaptured" })?.text).toContain("no Git staging snapshot");
    expect(stagingPresentation({ state: "synced" })?.text).toBe("Git staging copied.");
  });
  it("shows bounded conflicts and only an exact repository recovery path", () => {
    const result = stagingPresentation({ state: "conflicts", total: 18, paths: Array(20).fill("a.txt"), recovery: "chimaera-staging/copy-123" });
    expect(result?.paths).toHaveLength(16);
    expect(result?.text).toContain("18 paths");
    expect(result?.recovery).toBe("chimaera-staging/copy-123");
    expect(stagingPresentation({ state: "conflicts", total: 1, recovery: "https://secret.example" })?.recovery).toBeNull();
    expect(stagingPresentation({ state: "conflicts", total: -1 })).toBeNull();
  });
  it("requires exact ready copy role and a verified epoch for Take over", () => {
    const row = { local_copy: { state: "ready", ready: true }, ownership: { state: "remote", epoch: 4 } } as import("../net/native").MirrorWorkspace;
    expect(takeoverEpoch(row)).toBe(4);
    expect(takeoverEpoch({ ...row, local_copy: undefined })).toBeNull();
    expect(takeoverEpoch({ ...row, local_copy: { state: "pending", ready: true } })).toBeNull();
    expect(takeoverEpoch({ ...row, local_copy: { state: "ready", ready: false } })).toBeNull();
    expect(takeoverEpoch({ ...row, execution_allowed: true })).toBeNull();
    expect(takeoverEpoch({ ...row, ownership: { state: "awaiting_verification", epoch: 4 } })).toBeNull();
    expect(takeoverEpoch({ ...row, ownership: { state: "remote", epoch: -1 } })).toBeNull();
    const released = { ...row, local_copy: { state: "ready" as const, ready: true, owner_epoch: 7 }, ownership: null };
    expect(takeoverEpoch(released)).toBe(7);
    expect(takeoverEpoch({ ...released, local_copy: { ...released.local_copy, owner_epoch: -1 } })).toBeNull();
    expect(takeoverEpoch({ ...row, local_copy: { ...row.local_copy!, owner_epoch: Number.NaN } })).toBeNull();
  });
  it("never renders arbitrary daemon diagnostics and keeps upgrade refusal explicit", () => {
    expect(projectCopyError(new Error("private credential value"))).not.toContain("credential");
    expect(projectCopyError("project_copy_update_required")).toContain("without moving execution");
    expect(projectCopyError("unknown", "takeover")).toContain("Take over again");
  });
});
