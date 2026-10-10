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

/**
 * How an account-gateway answer says this browser view's job route is gone,
 * as the end the view shows, or null when it does not. The account answers
 * 503 with the keeper's own word: `job_ended` (the job ended: the plain
 * ended sentence), `workspace_closed` (closed while its job runs on),
 * `not_kept` (the cluster host is no longer kept connected) or
 * `host_unavailable` (gone, but it cannot tell which). A restarting keeper
 * or a stalled link answers `temporarily_unavailable` instead, which is only
 * a connection problem.
 */
export async function routeGoneAnswer(res: Response): Promise<JobEnd | null> {
  if (res.status !== 503) return null;
  let error: unknown;
  try {
    const body = (await res.json()) as unknown;
    error = typeof body === "object" && body !== null ? (body as { error?: unknown }).error : undefined;
  } catch {
    return null;
  }
  switch (error) {
    case "job_ended": return { reason: null };
    case "workspace_closed": return { reason: "closed" };
    case "not_kept": return { reason: "not-kept" };
    case "host_unavailable": return { reason: "gone" };
    default: return null;
  }
}

/** First look after the link drops, then the pace while it stays down. */
const FIRST_PROBE_MS = 4_000;
const PROBE_MS = 15_000;
/** One answer may come from a listing read mid-transition (the account
 *  caches a cluster's list for under a minute), so it takes two in a row. */
const GONE_STRIKES = 2;

/**
 * Watches a browser view of a cluster job workspace for its job's end. The
 * Mac app hears that from its shell; a browser has only the account's answer,
 * so while this view's link is down it asks the account, gently: only while
 * the page is visible, and never once the link is back or the end is known.
 */
export function createGatewayJobWatch(probe: () => Promise<Response>): {
  link(up: boolean): void;
  stop(): void;
} {
  let down = false;
  let strikes = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let onVisible: (() => void) | null = null;

  function clear(): void {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    if (onVisible !== null) document.removeEventListener("visibilitychange", onVisible);
    onVisible = null;
  }

  function schedule(ms: number): void {
    clear();
    timer = setTimeout(() => {
      timer = null;
      if (typeof document !== "undefined" && document.visibilityState !== "visible") {
        // Hidden: no asking at all; the return asks at once.
        onVisible = () => {
          if (document.visibilityState !== "visible") return;
          clear();
          void ask();
        };
        document.addEventListener("visibilitychange", onVisible);
        return;
      }
      void ask();
    }, ms);
  }

  async function ask(): Promise<void> {
    if (!down) return;
    let gone: JobEnd | null = null;
    try {
      gone = await routeGoneAnswer(await probe());
    } catch {
      gone = null;
    }
    if (!down) return;
    strikes = gone !== null ? strikes + 1 : 0;
    if (gone !== null && strikes >= GONE_STRIKES) {
      down = false;
      // The latest answer says why.
      endJobWindow(gone);
      return;
    }
    schedule(PROBE_MS);
  }

  return {
    link(up: boolean): void {
      if (up) {
        down = false;
        strikes = 0;
        clear();
        return;
      }
      if (down) return;
      down = true;
      strikes = 0;
      schedule(FIRST_PROBE_MS);
    },
    stop(): void {
      down = false;
      clear();
    },
  };
}
