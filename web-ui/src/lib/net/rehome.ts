/**
 * The re-home URL: where a window navigates when the daemon it talks to has
 * moved to another loopback origin (port) or minted another token, carrying
 * the window's identity so the reloaded document restores the same layout
 * (`win`), workspace (`ws`), Home hub flag (`hub`) and appearance.
 *
 * Two callers build it, and they deliberately differ (kept as parameters,
 * never "fixed" to match):
 *
 * - **A remote window's tunnel heal** (`host-status: connected`, App's
 *   `Connection.handleHostStatus`) sends `host` and, for a job window, the
 *   `job` / `node` / `cws` context; it carries `token` only when one is known
 *   (a compute-status event carries none and the window's own token may be
 *   absent), and falls back to the current port when the event has none.
 * - **The local daemon's self-update** (`onLocalDaemonUpdated`) sends no
 *   `host` and no job context (`host: null`, `job: null`), and always has a
 *   token and a port.
 *
 * The parameter order in the fragment is part of what the callers produced
 * before this module existed; the tests pin it.
 */
import type { JobContext } from "./api";

export interface RehomeParams {
  /** The daemon's loopback port. */
  port: number | string;
  /** The daemon token; null omits `token` (a remote heal without one). */
  token: string | null;
  /** The window key (`layout/viewState.windowKey()`). */
  win: string;
  /** The window's workspace, or null for Home. */
  ws: string | null;
  /** Carry the Home hub identity across the origin move. */
  hub: boolean;
  /** The remote host alias; null for the local daemon (omits `host`). */
  host: string | null;
  /** The bootstrap appearance, serialized as JSON into `appearance`. */
  appearance: unknown;
  /** The job this window serves; null omits `job` / `node` / `cws`. */
  job: JobContext | null;
}

export function rehomeUrl(p: RehomeParams): string {
  const params = new URLSearchParams();
  if (p.token !== null) params.set("token", p.token);
  params.set("win", p.win);
  if (p.ws !== null) params.set("ws", p.ws);
  if (p.hub) params.set("hub", "1");
  if (p.host !== null) params.set("host", p.host);
  params.set("appearance", JSON.stringify(p.appearance));
  if (p.job !== null) {
    params.set("job", p.job.jobId);
    if (p.job.node !== null) params.set("node", p.job.node);
    if (p.job.cws !== null) params.set("cws", p.job.cws);
  }
  return `http://127.0.0.1:${p.port}/#${params.toString()}`;
}
