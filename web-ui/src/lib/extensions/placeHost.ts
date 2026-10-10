/** The two daemon operations behind the host indicator's menu, on the
 * window's existing API owner: "Run here" (`POST /pro/projects/{id}/here`)
 * and "Run in the cloud" (`POST /pro/projects/{id}/cloud`). Both answer at
 * once (202) and finish on their own; the status row's `place` says when.
 * No retries: a refusal is the answer. */
import { api } from "../net/api";
import type { PlaceMoveResult } from "./application";

const ID = /^[A-Za-z0-9_-]{1,128}$/;
const CODE = /^[a-z_]{1,64}$/;
const TIMEOUT_MS = 15_000;

async function post(workspaceId: string, to: "here" | "cloud"): Promise<PlaceMoveResult> {
  if (!ID.test(workspaceId)) return { started: false, error: "unavailable" };
  let response: Response;
  try {
    response = await api(`/pro/projects/${encodeURIComponent(workspaceId)}/${to}`, { method: "POST", signal: AbortSignal.timeout(TIMEOUT_MS) });
  } catch { return { started: false, error: "unavailable" }; }
  if (response.status === 202) return { started: true };
  if (response.status !== 409) return { started: false, error: "unavailable" };
  const reply = await response.json().catch(() => null) as { error?: unknown } | null;
  const error = reply?.error;
  return { started: false, error: typeof error === "string" && CODE.test(error) ? error : "unavailable" };
}

export function runHere(workspaceId: string): Promise<PlaceMoveResult> { return post(workspaceId, "here"); }
export function runInCloud(workspaceId: string): Promise<PlaceMoveResult> { return post(workspaceId, "cloud"); }
