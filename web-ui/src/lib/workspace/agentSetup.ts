import { writable } from "svelte/store";
import { api, ApiError } from "../net/api";
import type { AgentInfo } from "./launcher";

export type SetupAction = "install" | "update" | "reinstall";
export interface SetupRequest { agent: AgentInfo; workspaceId: string; action: SetupAction; showResult: boolean }
export interface SetupProgress {
  id: string;
  action: SetupAction;
  phase: "running" | "cancelling" | "succeeded" | "failed" | "cancelled";
  workspace_id: string;
  started_at: number;
  exit_status: number | null;
  message: string;
  output: string;
  truncated: boolean;
}
export interface SetupDetails { host: string; root: string; operation: SetupProgress | null }
export const agentSetup = writable<SetupRequest | null>(null);
export function openAgentSetup(agent: AgentInfo, workspaceId: string, action: SetupAction, showResult = false): void {
  agentSetup.set({ agent, workspaceId, action, showResult });
}
export const setupRunning = (p: SetupProgress | null) => p?.phase === "running" || p?.phase === "cancelling";

/** A launcher may still show Install after a background operation finished. */
export function recoverSetupResult(request: SetupRequest, operation: SetupProgress, installed: boolean): boolean {
  return request.showResult
    || (request.agent.setup?.running === true && request.agent.setup.id === operation.id)
    || (request.action === "install" && !request.agent.installed && installed && operation.phase === "succeeded");
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await api(path, { ...init, signal: AbortSignal.timeout(15_000) });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new ApiError(res.status, body.error ?? `Request failed (${res.status})`);
  }
  return res.json() as Promise<T>;
}
const path = (agent: string) => `/agents/${encodeURIComponent(agent)}/setup`;
export const getAgentSetup = (agent: string) => request<SetupDetails>(path(agent));
export const startAgentSetup = (r: SetupRequest, id: string) => request<SetupProgress>(path(r.agent.id), {
  method: "POST", headers: { "Content-Type": "application/json" },
  body: JSON.stringify({ workspace_id: r.workspaceId, action: r.action, request_id: id }),
});
export const cancelAgentSetup = (agent: string, id: string) => request<SetupProgress>(`${path(agent)}/${encodeURIComponent(id)}`, { method: "DELETE" });
