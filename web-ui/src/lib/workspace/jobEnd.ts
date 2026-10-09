/**
 * A cluster job window's end: its job left the queue, or its workspace was
 * closed or stopped, so the daemon this window was served from is gone.
 * Whatever learns it first — the shell's `host-status` "ended" on this
 * window's key, or the cluster's own overview read after a reconnect fails —
 * sets it once; the window's overlay, its chats and its sockets all read it.
 */
import { writable } from "svelte/store";
import type { ClusterJob, ClusterOverview } from "../net/native";
import { haltReconnects } from "../net/reconnect";

export interface JobEnd {
  /** Slurm's terminal state, or `stopped` / `closed` / `workspace-failed` /
   *  `moving`; null when nothing says why. */
  reason: string | null;
}

/** Set once this window's job or workspace ended (null while it runs). */
export const windowJobEnd = writable<JobEnd | null>(null);

/** This window's daemon is gone for good: say so everywhere, and stop every
 *  socket's retries against an endpoint nothing will answer again. */
export function endJobWindow(end: JobEnd): void {
  windowJobEnd.set(end);
  if (end.reason !== "moving") haltReconnects();
}

function reasonOf(job: ClusterJob): string | null {
  return job.stopped_by_user ? "stopped" : (job.ended ?? null);
}

/**
 * Whether the cluster's overview says this job window's daemon is gone, and
 * why — or null when it still runs there (then a failed reconnect is a
 * connection problem, and the window says that instead).
 *
 * The window knows its Slurm job id and its cluster workspace (`cws`). An
 * attached job's record can end without ever learning its Slurm id, so when
 * no job carries this window's id, a workspace that is no longer open
 * anywhere means its job ended: the reason is then the latest such ended
 * job's, when one names this workspace.
 */
export function jobWindowEnd(
  ov: Pick<ClusterOverview, "jobs" | "workspaces">,
  slurmJobId: string,
  cws: string | null,
): JobEnd | null {
  const ws = cws === null ? undefined : ov.workspaces.find((w) => w.id === cws);
  const mine = ov.jobs.find((j) => j.slurm_job_id === slurmJobId);
  if (mine !== undefined) {
    if (mine.state === "ended") return { reason: reasonOf(mine) };
    if (ws === undefined || ws.state !== "closed") return null;
    // Its job still runs; the workspace itself closed or stopped there.
    return { reason: ws.failed !== undefined ? "workspace-failed" : "closed" };
  }
  if (ws === undefined || ws.state !== "closed") return null;
  const unnamed = ov.jobs
    .filter((j) => j.state === "ended" && j.slurm_job_id === undefined && j.open.includes(ws.id))
    .sort((a, b) => (b.ended_at_ms ?? b.submitted_ms) - (a.ended_at_ms ?? a.submitted_ms));
  return { reason: unnamed.length > 0 ? reasonOf(unnamed[0]) : null };
}
