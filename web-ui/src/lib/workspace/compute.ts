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

export interface ComputeSnapshot {
  scheduler: "slurm" | "none";
  /** The current user's jobs only, already capped server-side. */
  jobs: ComputeJob[];
  partitions: ComputePartition[];
  fetched_at_ms: number;
  /** True when the server-side cap dropped rows. */
  truncated: boolean;
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

function parseSnapshot(body: unknown): ComputeSnapshot {
  const none: ComputeSnapshot = {
    scheduler: "none",
    jobs: [],
    partitions: [],
    fetched_at_ms: 0,
    truncated: false,
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

