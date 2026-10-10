import { get } from "svelte/store";
import { describe, expect, it, vi } from "vitest";

import type { ClusterJob, ClusterWorkspaceView } from "../net/native";
import { reconnectsHalted } from "../net/reconnect";
import { endedScreenWords } from "./cluster";
import { createGatewayJobWatch, endJobWindow, jobWindowEnd, routeGoneAnswer, windowJobEnd } from "./jobEnd";

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

describe("a browser job view's watch", () => {
  const gone = () => new Response(JSON.stringify({ error: "host_unavailable" }), { status: 503 });
  const busy = () => new Response(JSON.stringify({ error: "temporarily_unavailable" }), { status: 503 });

  it("reads the account's words for a gone route, and nothing else", async () => {
    expect(await routeGoneAnswer(gone())).toEqual({ reason: "gone" });
    expect(await routeGoneAnswer(busy())).toBeNull();
    expect(await routeGoneAnswer(new Response("bad gateway", { status: 503 }))).toBeNull();
    expect(await routeGoneAnswer(new Response("{}", { status: 200 }))).toBeNull();
    expect(await routeGoneAnswer(new Response(JSON.stringify({ error: "job_ended" }), { status: 409 }))).toBeNull();
  });

  it("says what happened for each of the account's words", async () => {
    const said = async (error: string) => {
      const end = await routeGoneAnswer(new Response(JSON.stringify({ error }), { status: 503 }));
      return end === null ? null : endedScreenWords(end.reason);
    };
    expect(await said("job_ended")).toBe("This job ended. Your chats are saved.");
    expect(await said("workspace_closed")).toBe("This workspace was closed. Its chats are saved.");
    expect(await said("not_kept")).toBe("Turn on Keep connected for this host to open it here.");
    expect(await said("host_unavailable")).toBe("This workspace ended. Your chats are saved.");
    expect(await said("route_pending")).toBeNull();
  });

  it("ends with the latest answer's reason", async () => {
    vi.useFakeTimers();
    try {
      windowJobEnd.set(null);
      const answers = [gone, () => new Response(JSON.stringify({ error: "workspace_closed" }), { status: 503 })];
      let asked = 0;
      const watch = createGatewayJobWatch(async () => answers[asked++]());
      watch.link(false);
      await vi.advanceTimersByTimeAsync(4_000);
      await vi.advanceTimersByTimeAsync(15_000);
      expect(asked).toBe(2);
      expect(get(windowJobEnd)).toEqual({ reason: "closed" });
      watch.stop();
    } finally {
      vi.useRealTimers();
    }
  });

  it("ends the view after two gone answers in a row, and not after one", async () => {
    vi.useFakeTimers();
    try {
      windowJobEnd.set(null);
      const answers = [gone, busy, gone, gone];
      let asked = 0;
      const watch = createGatewayJobWatch(async () => answers[asked++]());
      watch.link(false);
      await vi.advanceTimersByTimeAsync(4_000);
      expect(asked).toBe(1);
      await vi.advanceTimersByTimeAsync(15_000);
      await vi.advanceTimersByTimeAsync(15_000);
      expect(asked).toBe(3);
      expect(get(windowJobEnd)).toBeNull();
      await vi.advanceTimersByTimeAsync(15_000);
      expect(asked).toBe(4);
      expect(get(windowJobEnd)).toEqual({ reason: "gone" });
      await vi.advanceTimersByTimeAsync(60_000);
      expect(asked).toBe(4);
      watch.stop();
    } finally {
      vi.useRealTimers();
    }
  });

  it("stops asking once the link is back", async () => {
    vi.useFakeTimers();
    try {
      let asked = 0;
      const watch = createGatewayJobWatch(async () => {
        asked++;
        return gone();
      });
      watch.link(false);
      await vi.advanceTimersByTimeAsync(4_000);
      watch.link(true);
      await vi.advanceTimersByTimeAsync(60_000);
      expect(asked).toBe(1);
      watch.stop();
    } finally {
      vi.useRealTimers();
    }
  });

  it("names the browser's unknown end plainly", () => {
    expect(endedScreenWords("gone")).toBe(
      "This workspace ended. Your chats are saved.",
    );
  });
});
