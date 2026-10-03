import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../net/api";
import { dotState, dotTitle, type Session } from "./sessions";
import { manualResumeNote, resumeParkedSession } from "./manualResume";

vi.mock("../net/api", async (original) => ({ ...await original<typeof import("../net/api")>(), api: vi.fn() }));

const parked = (): Session => ({
  id: "s-original", workspace_id: "w-one", kind: "agent", agent_kind: "claude", ui: "chat",
  manual_resume_reason: "project_secrets_idle", suspended: true, alive: false,
  name: "Conversation", cwd: "/fixture", cols: 80, rows: 24, created_at: 1,
  exit_status: null, title: null, agent_state: null, agent_title: null,
});
const resumed = () => ({ ...parked(), alive: true, suspended: false, manual_resume_reason: null });
const request = vi.mocked(api);
afterEach(() => vi.resetAllMocks());

describe("explicit resume of a parked conversation", () => {
  it("opens passively and does not present parking as an applied update", () => {
    expect(manualResumeNote(parked())).toContain("Paused for");
    expect(manualResumeNote(parked())).not.toMatch(/applied|updated|complete/i);
    expect(dotState(parked())).toBe("idle");
    expect(dotTitle(parked())).toContain("resume required");
    expect(request).not.toHaveBeenCalled();
  });

  it("sends only the original session ID with no spawn or native resume body", async () => {
    request.mockResolvedValue(new Response(JSON.stringify(resumed())));
    await resumeParkedSession(parked());
    expect(request).toHaveBeenCalledTimes(1);
    expect(request).toHaveBeenCalledWith("/sessions/s-original/resume", {
      method: "POST", signal: expect.any(AbortSignal),
    });
  });

  it("keeps every unknown reason manual and refuses the known action without a request", async () => {
    for (const reason of ["future_reason", ""]) {
      const row = { ...parked(), manual_resume_reason: reason };
      expect(manualResumeNote(row)).toContain("paused");
      expect(dotTitle(row)).toContain("resume required");
      await expect(resumeParkedSession(row)).rejects.toThrow("cannot be resumed here");
    }
    expect(request).not.toHaveBeenCalled();
    expect(manualResumeNote({})).toBeNull();
    expect(manualResumeNote({ manual_resume_reason: null })).toBeNull();
  });

  it("refuses changed identity, incomplete or still-manual acknowledgment", async () => {
    for (const change of [
      { id: "s-new" }, { workspace_id: "w-other" }, { kind: "shell" }, { agent_kind: "codex" },
      { ui: "term" }, { alive: false }, { suspended: true }, { manual_resume_reason: "unknown" },
    ]) {
      request.mockResolvedValueOnce(new Response(JSON.stringify({ ...resumed(), ...change })));
      await expect(resumeParkedSession(parked())).rejects.toThrow("Couldn’t confirm");
    }
    expect(request).toHaveBeenCalledTimes(8);
  });

  it("does not resend after an ambiguous transport or malformed successful reply", async () => {
    request.mockRejectedValueOnce(new Error("lost reply"));
    await expect(resumeParkedSession(parked())).rejects.toThrow("Check this conversation’s status");
    expect(request).toHaveBeenCalledTimes(1);
    request.mockResolvedValueOnce(new Response("not-json"));
    await expect(resumeParkedSession(parked())).rejects.toThrow("Couldn’t confirm");
    expect(request).toHaveBeenCalledTimes(2);
  });

  it("keeps a maintenance refusal visible and never claims resume succeeded", async () => {
    request.mockResolvedValue(new Response('{"error":"maintenance_pending"}', { status: 409 }));
    await expect(resumeParkedSession(parked())).rejects.toThrow("isn’t ready to resume");
    expect(request).toHaveBeenCalledTimes(1);
  });

  it("captures identity before waiting for the reply", async () => {
    let finish!: (response: Response) => void;
    request.mockReturnValue(new Promise<Response>((resolve) => { finish = resolve; }));
    const row = parked();
    const result = resumeParkedSession(row);
    row.id = "s-replacement";
    row.workspace_id = "w-replacement";
    finish(new Response(JSON.stringify(resumed())));
    await result;
    expect(request).toHaveBeenCalledWith("/sessions/s-original/resume", expect.anything());
  });
});
