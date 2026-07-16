/**
 * Client for the daemon's Mode 2 compute-session routes — used by the
 * HOST-DETAIL home screen (a remote window already talking to the login
 * daemon, so no native-bridge hop is needed):
 *   GET    /api/v1/compute/sessions            the stateless registry
 *   POST   /api/v1/compute/sessions            launch a new session (detached srun)
 *   DELETE /api/v1/compute/sessions/{job_id}   scancel
 *
 * The wire MAY carry each session's daemon `port`/`token` (the app shell's
 * trust boundary strips them before its JS; this direct path must uphold
 * the same rule) — the parser below deliberately never copies them, so
 * nothing downstream can render or store them. Opening a session still
 * goes through the native bridge (`connectComputeSession`): only the shell
 * can build the tunnel and the window.
 */
import { api, ApiError } from "../net/api";
import { parsePartition, type ComputePartition } from "./compute";

/** A compute-node session (a Slurm job owning a full chimaera daemon),
 *  minus its port/token — see the module header. */
export interface ComputeSessionView {
  job_id: string;
  name: string;
  /** Raw Slurm state (RUNNING, PENDING, …) — never relabeled. */
  state: string;
  /** Node name once allocated; empty while the job waits in the queue. */
  node: string;
  partition: string;
  /** Walltime remaining — the session's honest lifetime. */
  time_left: string;
  cpus: number | null;
  mem: string | null;
  gres: string | null;
  workspace_id: string | null;
  /** The daemon bound a cluster-routable address instead of loopback. */
  routable: boolean;
  /** Whether the node can reach the agent API. `null` = couldn't verify —
   *  which is NOT the same fact as blocked. */
  egress: boolean | null;
  /** The session's daemon is up and connectable (manifest written). */
  ready: boolean;
}

/** What one GET answers: scheduler tag + sessions + partitions (the one
 *  call feeds both the group and the launch dialog). */
export interface ComputeSessionList {
  /** "slurm" or "none" — anything but "slurm" hides the compute surface. */
  scheduler: string;
  sessions: ComputeSessionView[];
  partitions: ComputePartition[];
  /** The squeue round failed and `sessions` carries the last good rows
   *  forward stale — countdown baselines must NOT re-sync to this response.
   *  Missing on older daemons → false. */
  degraded: boolean;
}

/** A launch request for a new compute-node session (detached srun). */
export interface ComputeLaunchSpec {
  name: string;
  /** Slurm walltime, e.g. "2:00:00". */
  time: string;
  partition?: string;
  cpus?: number;
  mem?: string;
  gres?: string;
  workspace_id?: string;
  /** Launch-scope startup commands — host/workspace preludes still apply. */
  prelude?: string;
  /** Bind a routable address (rung A) — exposes the port on the cluster
   *  network, token-gated. Default (absent/false) is loopback + ssh forward. */
  routable?: boolean;
}

// Defensive parsing (the compute.ts idiom): drop malformed rows rather than
// blank the group. port/token are NOT copied — that omission is load-bearing.
function parseSession(raw: unknown): ComputeSessionView | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.job_id !== "string" || typeof r.state !== "string") return null;
  return {
    job_id: r.job_id,
    name: typeof r.name === "string" ? r.name : "",
    state: r.state,
    node: typeof r.node === "string" ? r.node : "",
    partition: typeof r.partition === "string" ? r.partition : "",
    time_left: typeof r.time_left === "string" ? r.time_left : "",
    cpus: typeof r.cpus === "number" ? r.cpus : null,
    mem: typeof r.mem === "string" ? r.mem : null,
    gres: typeof r.gres === "string" ? r.gres : null,
    workspace_id: typeof r.workspace_id === "string" ? r.workspace_id : null,
    routable: r.routable === true,
    egress: typeof r.egress === "boolean" ? r.egress : null,
    ready: r.ready === true,
  };
}

/** The daemon's error envelope is `{"error": msg}`; fall back to the status. */
async function errorMessage(res: Response, fallback: string): Promise<string> {
  try {
    const body = (await res.json()) as unknown;
    if (typeof body === "object" && body !== null) {
      const e = (body as Record<string, unknown>).error;
      if (typeof e === "string" && e !== "") return e;
    }
  } catch {
    // non-JSON error body; use the fallback
  }
  return `${fallback} (status ${res.status})`;
}

/** This window's daemon's compute sessions. Throws with a readable message. */
export async function listComputeSessions(): Promise<ComputeSessionList> {
  const res = await api("/compute/sessions");
  if (!res.ok) throw new Error(await errorMessage(res, "could not list compute sessions"));
  const body = (await res.json()) as unknown;
  const b = (typeof body === "object" && body !== null ? body : {}) as Record<string, unknown>;
  return {
    scheduler: typeof b.scheduler === "string" ? b.scheduler : "none",
    sessions: Array.isArray(b.sessions)
      ? b.sessions.map(parseSession).filter((s): s is ComputeSessionView => s !== null)
      : [],
    partitions: Array.isArray(b.partitions)
      ? b.partitions.map(parsePartition).filter((p): p is ComputePartition => p !== null)
      : [],
    degraded: b.degraded === true,
  };
}

/** Submit a compute-node session on this window's daemon → the Slurm job id. */
export async function launchComputeSession(spec: ComputeLaunchSpec): Promise<string> {
  const res = await api("/compute/sessions", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(spec),
  });
  if (!res.ok) throw new Error(await errorMessage(res, "launch failed"));
  const body = (await res.json()) as unknown;
  const jobId =
    typeof body === "object" && body !== null
      ? (body as Record<string, unknown>).job_id
      : undefined;
  return typeof jobId === "string" ? jobId : "";
}

/** scancel `jobId` — Slurm ends everything in the allocation. Idempotent.
 *  Error shape is the discriminant callers need: the daemon ANSWERING with an
 *  error status throws `ApiError` (the cancel definitively FAILED — e.g. 409
 *  "no scheduler detected" from a node without Slurm client tools), while a
 *  network-level rejection (fetch died mid-flight) propagates as-is — for a
 *  job window cancelling its OWN daemon, that death is the expected success
 *  signal. */
export async function cancelComputeSession(jobId: string): Promise<void> {
  const res = await api(`/compute/sessions/${encodeURIComponent(jobId)}`, {
    method: "DELETE",
  });
  if (!res.ok) throw new ApiError(res.status, await errorMessage(res, "cancel failed"));
}
