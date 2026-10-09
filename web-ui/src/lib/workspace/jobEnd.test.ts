import { get } from "svelte/store";
import { describe, expect, it } from "vitest";

import type { ClusterJob, ClusterWorkspaceView } from "../net/native";
import { reconnectsHalted } from "../net/reconnect";
import { endedScreenWords } from "./cluster";
import { endJobWindow, jobWindowEnd, windowJobEnd } from "./jobEnd";

function job(over: Partial<ClusterJob> = {}): ClusterJob {
  return {
    id: "j-0000aaaa",
    name: "Dev",
    state: "running",
    slurm_job_id: "4242",
    attached: true,
    stopped_by_user: false,
    open: ["w-0000aaaa"],
    spec: { time: "00:10:00" },
    startup: "",
    submitted_ms: 1_000,
    ...over,
  };
}

function ws(over: Partial<ClusterWorkspaceView> = {}): ClusterWorkspaceView {
  return { id: "w-0000aaaa", name: "proj", path: "/scratch/u/proj", state: "open", job: "j-0000aaaa", ...over };
}

describe("jobWindowEnd", () => {
  it("reads a job that hit its time limit as ended, in the cluster page's words", () => {
    const end = jobWindowEnd(
      { jobs: [job({ state: "ended", ended: "TIMEOUT" })], workspaces: [ws({ state: "closed", job: undefined })] },
      "4242",
      "w-0000aaaa",
    );
    expect(end).toEqual({ reason: "TIMEOUT" });
    expect(endedScreenWords(end?.reason)).toBe("This job ended — it hit its time limit. Your chats are saved.");
  });

  it("says the user stopped it, and an ended job with no reason yet still ended", () => {
    expect(jobWindowEnd({ jobs: [job({ state: "ended", stopped_by_user: true })], workspaces: [] }, "4242", null)).toEqual({
      reason: "stopped",
    });
    expect(jobWindowEnd({ jobs: [job({ state: "ended" })], workspaces: [] }, "4242", null)).toEqual({ reason: null });
  });

  it("a running job with the workspace still open is a connection problem, not an end", () => {
    expect(jobWindowEnd({ jobs: [job()], workspaces: [ws()] }, "4242", "w-0000aaaa")).toBeNull();
  });

  it("a running job whose workspace closed or stopped says which", () => {
    expect(jobWindowEnd({ jobs: [job()], workspaces: [ws({ state: "closed" })] }, "4242", "w-0000aaaa")).toEqual({
      reason: "closed",
    });
    expect(
      jobWindowEnd({ jobs: [job()], workspaces: [ws({ state: "closed", failed: "boom" })] }, "4242", "w-0000aaaa"),
    ).toEqual({ reason: "workspace-failed" });
  });

  it("an attached job that ended without learning its Slurm id is found by its workspace", () => {
    const ended = job({ state: "ended", slurm_job_id: undefined, ended: "TIMEOUT", ended_at_ms: 5_000 });
    const older = job({ id: "j-0000bbbb", state: "ended", slurm_job_id: undefined, ended: "CANCELLED", ended_at_ms: 1_000 });
    expect(
      jobWindowEnd({ jobs: [older, ended], workspaces: [ws({ state: "closed", job: undefined })] }, "4242", "w-0000aaaa"),
    ).toEqual({ reason: "TIMEOUT" });
  });

  it("no job carrying the id and the workspace open elsewhere or unknown: not an end", () => {
    expect(
      jobWindowEnd({ jobs: [job({ slurm_job_id: "9" })], workspaces: [ws({ job: "j-0000cccc" })] }, "4242", "w-0000aaaa"),
    ).toBeNull();
    expect(jobWindowEnd({ jobs: [], workspaces: [] }, "4242", "w-0000aaaa")).toBeNull();
    expect(jobWindowEnd({ jobs: [], workspaces: [] }, "4242", null)).toBeNull();
  });
});

describe("endJobWindow", () => {
  it("a move keeps the sockets; an end publishes and stops every retry", () => {
    endJobWindow({ reason: "moving" });
    expect(get(windowJobEnd)).toEqual({ reason: "moving" });
    expect(reconnectsHalted()).toBe(false);
    endJobWindow({ reason: "TIMEOUT" });
    expect(get(windowJobEnd)).toEqual({ reason: "TIMEOUT" });
    expect(reconnectsHalted()).toBe(true);
  });
});
