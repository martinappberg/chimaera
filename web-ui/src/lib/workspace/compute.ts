/**
 * Client + reactive store for the daemon's compute service:
 *   GET /api/v1/compute[?refresh=true]   scheduler detection + the user's queue
 *
 * Detection is daemon-side (the git-service model): the cluster daemon probes
 * for Slurm locally and this store just mirrors the answer. Off-cluster the
 * first fetch says `scheduler:"none"` and the store goes quiet — one probe per
 * page load on a laptop. On-cluster it refetches every 60s, but ONLY while the
 * window is visible; a hidden tab pauses and catches up on the next
 * visibilitychange. `?refresh=true` (the popover's refresh button) forces the
 * daemon to re-detect — the "I just module-loaded slurm" path — and a "slurm"
 * answer there restarts polling.
 */
import { writable, type Readable } from "svelte/store";

import { api } from "../net/api";

export interface ComputeJob {
  id: string;
  name: string;
  partition: string;
  /** Raw Slurm state (RUNNING, PENDING, COMPLETING, …) — never relabeled. */
  state: string;
  time_left: string;
  /** Node list for running jobs; Slurm's pending reason otherwise. */
  nodes: string;
}

export interface ComputePartition {
  name: string;
  default: boolean;
  avail: boolean;
  nodes: number;
}

/**
 * The allocation THIS daemon runs inside (Mode 2 compute-node session).
 * Present only when the daemon detects it was launched under a Slurm job —
 * the walltime here is the window's lifetime, so the rail's allocation
 * strip wears it (as a live countdown) and the host label carries the node.
 */
export interface ComputeSelf {
  job_id: string;
  node: string;
  partition: string;
  /** Raw Slurm state — never relabeled. */
  state: string;
  time_left: string;
  /** Allocated resources (squeue %C/%m/%b), "" when the wire lacked them. */
  cpus: string;
  mem: string;
  gres: string;
}

export interface ComputeSnapshot {
  scheduler: "slurm" | "none";
  /** The current user's jobs only, already capped server-side. */
  jobs: ComputeJob[];
  partitions: ComputePartition[];
  fetched_at_ms: number;
  /** True when the server-side cap dropped rows. */
  truncated: boolean;
  /** The allocation this daemon runs inside, when it IS a compute-node
   *  session (`null` = the wire didn't carry a usable `self` block). */
  self: ComputeSelf | null;
  /** CLIENT clock at parse time — the countdown's baseline. `time_left`
   *  only moves per fetch; the strip ticks against this locally. */
  received_at_ms: number;
}

// Defensive parsing (the environment-fetcher idiom): drop malformed entries
// rather than let one bad row blank the whole strip.
function parseJob(raw: unknown): ComputeJob | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.id !== "string" || typeof r.state !== "string") return null;
  return {
    id: r.id,
    name: typeof r.name === "string" ? r.name : "",
    partition: typeof r.partition === "string" ? r.partition : "",
    state: r.state,
    time_left: typeof r.time_left === "string" ? r.time_left : "",
    nodes: typeof r.nodes === "string" ? r.nodes : "",
  };
}

function parsePartition(raw: unknown): ComputePartition | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.name !== "string") return null;
  return {
    name: r.name,
    default: r.default === true,
    avail: r.avail === true,
    nodes: typeof r.nodes === "number" ? r.nodes : 0,
  };
}

// Absent, malformed, or missing its job id all read as "not in an allocation".
function parseSelf(raw: unknown): ComputeSelf | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.job_id !== "string" || r.job_id === "") return null;
  return {
    job_id: r.job_id,
    node: typeof r.node === "string" ? r.node : "",
    partition: typeof r.partition === "string" ? r.partition : "",
    state: typeof r.state === "string" ? r.state : "",
    time_left: typeof r.time_left === "string" ? r.time_left : "",
    cpus: typeof r.cpus === "string" ? r.cpus : "",
    mem: typeof r.mem === "string" ? r.mem : "",
    gres: typeof r.gres === "string" ? r.gres : "",
  };
}

/**
 * Slurm's TimeLeft rendering (`[days-]hours:minutes:seconds`, short forms
 * `MM:SS`) → seconds. `null` for the non-durations Slurm also emits here
 * (UNLIMITED, NOT_SET, INVALID) — callers show the raw string instead.
 */
export function parseSlurmTimeLeft(s: string): number | null {
  const m = s.trim().match(/^(?:(\d+)-)?(?:(\d+):)?(\d{1,2}):(\d{2})$/);
  if (m === null) return null;
  const days = m[1] !== undefined ? Number(m[1]) : 0;
  const hours = m[2] !== undefined ? Number(m[2]) : 0;
  return ((days * 24 + hours) * 60 + Number(m[3])) * 60 + Number(m[4]);
}

/** Seconds → Slurm's own duration style (`1-04:00:00`, `1:57:12`, `04:32`). */
export function formatSlurmDuration(totalSecs: number): string {
  const secs = Math.max(0, Math.floor(totalSecs));
  const days = Math.floor(secs / 86400);
  const hours = Math.floor((secs % 86400) / 3600);
  const mins = Math.floor((secs % 3600) / 60);
  const rest = secs % 60;
  const mmss = `${String(mins).padStart(2, "0")}:${String(rest).padStart(2, "0")}`;
  if (days > 0) return `${days}-${String(hours).padStart(2, "0")}:${mmss}`;
  if (hours > 0) return `${hours}:${mmss}`;
  return mmss;
}

function parseSnapshot(body: unknown): ComputeSnapshot {
  const none: ComputeSnapshot = {
    scheduler: "none",
    jobs: [],
    partitions: [],
    fetched_at_ms: 0,
    truncated: false,
    self: null,
    received_at_ms: Date.now(),
  };
  if (typeof body !== "object" || body === null) return none;
  const b = body as Record<string, unknown>;
  if (b.scheduler !== "slurm") return none;
  return {
    scheduler: "slurm",
    jobs: Array.isArray(b.jobs)
      ? b.jobs.map(parseJob).filter((j): j is ComputeJob => j !== null)
      : [],
    partitions: Array.isArray(b.partitions)
      ? b.partitions.map(parsePartition).filter((p): p is ComputePartition => p !== null)
      : [],
    fetched_at_ms: typeof b.fetched_at_ms === "number" ? b.fetched_at_ms : 0,
    truncated: b.truncated === true,
    self: parseSelf(b.self),
    received_at_ms: Date.now(),
  };
}

/** RUNNING + PENDING — the number the daemon-bar chip wears. */
export function queuedJobCount(snap: ComputeSnapshot): number {
  return snap.jobs.filter((j) => j.state === "RUNNING" || j.state === "PENDING").length;
}

// ---- reactive store ----------------------------------------------------------

const snapshotStore = writable<ComputeSnapshot | null>(null);
/** The last compute snapshot (`null` = not fetched yet / never reachable). */
export const computeStatus: Readable<ComputeSnapshot | null> = snapshotStore;

const POLL_MS = 60_000;

let started = false;
let timer: ReturnType<typeof setInterval> | null = null;
let fetchSeq = 0;
/** The last RESPONSE's scheduler; a failed fetch doesn't change the gate. */
let lastScheduler: "slurm" | "none" | null = null;

/** Reconcile the 60s timer with the polling gate (slurm + visible). */
function sync(): void {
  const shouldPoll =
    started && lastScheduler === "slurm" && document.visibilityState === "visible";
  if (shouldPoll && timer === null) {
    timer = setInterval(() => void fetchSnapshot(false), POLL_MS);
  } else if (!shouldPoll && timer !== null) {
    clearInterval(timer);
    timer = null;
  }
}

async function fetchSnapshot(force: boolean): Promise<void> {
  const seq = ++fetchSeq;
  try {
    const res = await api(`/compute${force ? "?refresh=true" : ""}`);
    // Transient failure: keep the shown snapshot; the timer (if any) retries.
    if (!res.ok) return;
    const snap = parseSnapshot((await res.json()) as unknown);
    // Drop stale responses (a manual refresh overtook us, or teardown ran).
    if (!started || seq !== fetchSeq) return;
    lastScheduler = snap.scheduler;
    snapshotStore.set(snap);
    sync();
  } catch {
    // unreachable daemon; the daemon dot already reflects reachability
  }
}

function onVisibility(): void {
  // Hidden paused the timer, so on return refetch NOW rather than waiting out
  // a full period with a stale queue.
  if (document.visibilityState === "visible" && started && lastScheduler === "slurm") {
    void fetchSnapshot(false);
  }
  sync();
}

/**
 * Boot the store: one probe now, then the gated 60s poll. Returns the
 * teardown (App.svelte runs this from an `$effect`).
 */
export function initCompute(): () => void {
  started = true;
  document.addEventListener("visibilitychange", onVisibility);
  void fetchSnapshot(false);
  return () => {
    started = false;
    fetchSeq += 1;
    document.removeEventListener("visibilitychange", onVisibility);
    if (timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  };
}

