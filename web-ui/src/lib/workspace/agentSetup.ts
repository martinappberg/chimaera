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

// Four fixed keys, scoped to this daemon origin. Remember before POST so an
// ambiguous response also survives closing/reloading; acknowledge only once
// the completed result is displayed. Storage denial still keeps in-tab state.
const pendingIds = new Map<string, string | null>();
const storageKey = (agent: string) => `chimaera.agentSetup.${agent}`;
export function pendingSetupId(agent: string): string | null {
  if (pendingIds.has(agent)) return pendingIds.get(agent) ?? null;
  try { return sessionStorage.getItem(storageKey(agent)); } catch { return null; }
}
export function rememberSetupId(agent: string, id: string): void {
  if (!["claude", "codex", "agy", "grok"].includes(agent) || !/^[A-Za-z0-9-]{1,128}$/.test(id)) return;
  if (pendingIds.get(agent) === id) return;
  pendingIds.set(agent, id);
  try { sessionStorage.setItem(storageKey(agent), id); } catch { /* in-tab fallback */ }
}
export function acknowledgeSetupResult(agent: string, id: string): void {
  if (pendingSetupId(agent) !== id) return;
  pendingIds.set(agent, null);
  try { sessionStorage.removeItem(storageKey(agent)); } catch { /* in-tab fallback */ }
}

/** A launcher may still show Install after a background operation finished. */
export function recoverSetupResult(request: SetupRequest, operation: SetupProgress, installed: boolean): boolean {
  return request.showResult
    || pendingSetupId(request.agent.id) === operation.id
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
export async function getAgentSetup(agent: string): Promise<SetupDetails> {
  const details = await request<SetupDetails>(path(agent));
  if (details.operation && setupRunning(details.operation)) rememberSetupId(agent, details.operation.id);
  return details;
}
export async function startAgentSetup(r: SetupRequest, id: string): Promise<SetupProgress> {
  rememberSetupId(r.agent.id, id);
  try {
    const operation = await request<SetupProgress>(path(r.agent.id), {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ workspace_id: r.workspaceId, action: r.action, request_id: id }),
    });
    rememberSetupId(r.agent.id, operation.id);
    return operation;
  } catch (error) {
    if (error instanceof ApiError) acknowledgeSetupResult(r.agent.id, id);
    throw error;
  }
}
export const cancelAgentSetup = (agent: string, id: string) => request<SetupProgress>(`${path(agent)}/${encodeURIComponent(id)}`, { method: "DELETE" });
