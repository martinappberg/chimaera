/** The one daemon operation behind the host indicator's "Run in the cloud",
 * on the window's existing API owner: the quit hand-off (`POST /pro/sleep`
 * with `park`) for exactly one project. A parked project stays in the cloud
 * until this computer leaves and comes back (`/pro/wake`), so the laptop does
 * not pull it straight home again. No retries: a refusal is the answer. */
import { api } from "../net/api";
import type { PlaceMoveResult } from "./application";

/** The daemon's own budget for the hand-off; past it the move finishes by
 *  itself and the reply says so (`deadline`). */
const BUDGET_MS = 60_000;
const ID = /^[A-Za-z0-9_-]{1,128}$/;
const CODE = /^[a-z_]{1,64}$/;

export async function runInCloud(workspaceId: string): Promise<PlaceMoveResult> {
  if (!ID.test(workspaceId)) return { moved: false, reason: "unavailable" };
  let response: Response;
  try {
    response = await api("/pro/sleep", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ park: true, workspace_ids: [workspaceId], deadline_ms: BUDGET_MS }),
      signal: AbortSignal.timeout(BUDGET_MS + 15_000),
    });
  } catch { return { moved: false, reason: "unavailable" }; }
  // 204: this daemon has no account configured, so nothing could move.
  if (!response.ok || response.status === 204) return { moved: false, reason: "unavailable" };
  const reply = await response.json().catch(() => null) as { handoff?: unknown; reason?: unknown; pending?: unknown; failed?: unknown } | null;
  if (reply?.handoff === true) return { moved: true };
  // Still publishing at the budget: the daemon finishes (or undoes) it alone.
  if (reply?.reason === "deadline" && Array.isArray(reply.pending) && reply.pending.includes(workspaceId)) return { moved: true };
  const failed = Array.isArray(reply?.failed)
    ? (reply.failed as { workspace_id?: unknown; error?: unknown }[]).find(row => row?.workspace_id === workspaceId)
    : undefined;
  const reason = typeof failed?.error === "string" ? failed.error : reply?.reason;
  return { moved: false, reason: typeof reason === "string" && CODE.test(reason) ? reason : "unavailable" };
}
