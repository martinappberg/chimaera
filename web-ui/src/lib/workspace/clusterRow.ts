/**
 * The few cluster words the local home's host row needs — kept apart from
 * `cluster.ts` so the always-loaded home bundle carries only these, while
 * the cluster page, start sheet and job notices load theirs on demand.
 */
import type { ClusterOverview } from "../net/native";

const MINUTE = 60;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** The host row's scheduler tag: "Slurm cluster", "PBS cluster", "LSF cluster". */
export function schedulerLabel(s: string | null | undefined): string {
  switch (s) {
    case "pbs":
      return "PBS cluster";
    case "lsf":
      return "LSF cluster";
    default:
      return "Slurm cluster";
  }
}

/** Compact time left: "5d 22h", "3h 12m", "58 min", "under a minute". */
export function shortDuration(totalSecs: number): string {
  const secs = Math.max(0, Math.floor(totalSecs));
  if (secs < MINUTE) return "under a minute";
  const d = Math.floor(secs / DAY);
  const h = Math.floor((secs % DAY) / HOUR);
  const m = Math.floor((secs % HOUR) / MINUTE);
  if (d > 0) return h > 0 ? `${d}d ${h}h` : `${d}d`;
  if (h > 0) return m > 0 ? `${h}h ${m}m` : `${h}h`;
  return `${m} min`;
}

/** "5d 22h left" until `endsAtMs`; "time's up" once it passed. */
export function timeLeftWords(endsAtMs: number, nowMs: number): string {
  const secs = Math.floor((endsAtMs - nowMs) / 1000);
  if (secs <= 0) return "time's up";
  return `${shortDuration(secs)} left`;
}

/** A cancelled job may remain in Slurm's live queue while its tasks shut down. */
export function isJobStopping(j: ClusterOverview["jobs"][number]): boolean {
  return j.state !== "ended" && (j.stopping === true || j.stopped_by_user);
}

/** The host row's job counts, with stopping jobs kept out of running totals. */
export function hostSummary(ov: ClusterOverview, nowMs: number): string {
  const live = ov.jobs.filter((j) => j.state !== "ended");
  const stopping = live.filter(isJobStopping);
  const running = live.filter((j) => !isJobStopping(j) && (j.state === "running" || j.state === "starting"));
  const waiting = live.filter((j) => !isJobStopping(j) && j.state === "waiting");
  const jobs = (n: number) => (n === 1 ? "1 job" : `${n} jobs`);
  if (live.length === 0) return "no jobs running";
  if (stopping.length > 0) {
    return [
      running.length > 0 ? `${jobs(running.length)} running` : "",
      waiting.length > 0 ? `${jobs(waiting.length)} waiting` : "",
      `${jobs(stopping.length)} stopping`,
    ].filter(Boolean).join(" · ");
  }
  if (running.length === 0) return `${jobs(waiting.length)} waiting for a node`;
  if (waiting.length > 0) return `${jobs(running.length)} running · ${waiting.length} waiting`;
  if (running.length === 1 && running[0].state === "starting") return "1 job starting";
  const ends = running
    .map((j) => j.ends_at_ms)
    .filter((e): e is number => e !== undefined)
    .sort((a, b) => a - b);
  if (ends.length === 0) return `${jobs(running.length)} running`;
  const secs = Math.floor((ends[0] - nowMs) / 1000);
  const when = secs <= 0 ? "time's up" : `${running.length > 1 ? "next ends" : "ends"} in ${shortDuration(secs)}`;
  return `${jobs(running.length)} running · ${when}`;
}

/** Whether any job on the cluster is running (the row's dot). */
export function anyJobRunning(ov: ClusterOverview): boolean {
  return ov.jobs.some((j) => j.state === "running" && !isJobStopping(j));
}
