/**
 * The client half of the daemon's bounded agent operations (`operation.rs`):
 * what a managed install (`agentSetup.ts`) and a sign-in (`agentSignIn.ts`)
 * share — the phase vocabulary, the fields every progress record carries,
 * "is it still running", and the one JSON request helper that times out and
 * throws the daemon's `{error}` as the message.
 */
import { api, ApiError } from "../net/api";

export type OperationPhase = "running" | "cancelling" | "succeeded" | "failed" | "cancelled";

/** The fields every operation's progress carries; each kind adds its own. */
export interface OperationProgress {
  id: string;
  phase: OperationPhase;
  workspace_id: string;
  started_at: number;
  message: string;
}

/** Still going: a cancel in flight counts (the record changes again). */
export const operationRunning = (p: { phase: OperationPhase } | null | undefined): boolean =>
  p?.phase === "running" || p?.phase === "cancelling";

/** `/agents/{id}/<operation>`, the agent id escaped. */
export const agentOperationPath = (agent: string, operation: "setup" | "sign-in"): string =>
  `/agents/${encodeURIComponent(agent)}/${operation}`;

export const postJson = (body: unknown): RequestInit => ({
  method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
});

export async function request<T>(path: string, init?: RequestInit, timeoutMs = 15_000): Promise<T> {
  const res = await api(path, { ...init, signal: AbortSignal.timeout(timeoutMs) });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new ApiError(res.status, body.error ?? `Request failed (${res.status})`);
  }
  return res.json() as Promise<T>;
}
