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

/**
 * The host row's one-line summary: "crc running · 5d 22h left",
 * "2 running · 1 waiting", "2 workspaces · none running", "no workspaces yet".
 */
export function hostSummary(ov: ClusterOverview, nowMs: number): string {
  const ws = ov.workspaces;
  if (ws.length === 0) return "no workspaces yet";
  const running = ws.filter((w) => w.state === "running" || w.state === "starting");
  const waiting = ws.filter((w) => w.state === "waiting");
  if (running.length === 1 && waiting.length === 0) {
    const w = running[0];
    if (w.state === "starting") return `${w.name} starting`;
    return w.ends_at_ms !== undefined
      ? `${w.name} running · ${timeLeftWords(w.ends_at_ms, nowMs)}`
      : `${w.name} running`;
  }
  if (running.length === 0 && waiting.length === 1) return `${waiting[0].name} waiting for a node`;
  if (running.length > 0 || waiting.length > 0) {
    const parts: string[] = [];
    if (running.length > 0) parts.push(`${running.length} running`);
    if (waiting.length > 0) parts.push(`${waiting.length} waiting`);
    return parts.join(" · ");
  }
  return ws.length === 1 ? "1 workspace · not running" : `${ws.length} workspaces · none running`;
}
