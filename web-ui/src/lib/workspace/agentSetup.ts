import { writable } from "svelte/store";
import { ApiError } from "../net/api";
import type { AgentInfo } from "./launcher";
import { AGENT_PRODUCT_NAMES } from "../shared/agentNames";
import { agentOperationPath, operationRunning, postJson, request, type OperationProgress } from "./operations";

/** Built-in agents whose pending setup id survives a reload. */
const SETUP_AGENTS: ReadonlySet<string> = new Set(Object.keys(AGENT_PRODUCT_NAMES));

export type SetupAction = "install" | "update" | "reinstall";
export interface SetupRequest { agent: AgentInfo; workspaceId: string; action: SetupAction; showResult: boolean }
/** What the daemon found when it looked for the executable after a clean
 *  installer exit; null while running, or when the installer failed. Only
 *  `found` is a success: `unchecked` means the login shell couldn't answer. */
export type SetupVerified =
  | { state: "found"; path: string; version: string | null }
  | { state: "unchecked"; reason: string }
  | { state: "missing" };
export interface SetupProgress extends OperationProgress {
  action: SetupAction;
  exit_status: number | null;
  output: string;
  truncated: boolean;
  verified?: SetupVerified | null;
}
export interface SetupDetails { host: string; root: string; operation: SetupProgress | null }
export const agentSetup = writable<SetupRequest | null>(null);
export function openAgentSetup(agent: AgentInfo, workspaceId: string, action: SetupAction, showResult = false): void {
  agentSetup.set({ agent, workspaceId, action, showResult });
}

// Fixed per-agent keys, scoped to this daemon origin. Remember before POST so an
// ambiguous response also survives closing/reloading; acknowledge only once
// the completed result is displayed. Storage denial still keeps in-tab state.
const pendingIds = new Map<string, string | null>();
const storageKey = (agent: string) => `chimaera.agentSetup.${agent}`;
export function pendingSetupId(agent: string): string | null {
  if (pendingIds.has(agent)) return pendingIds.get(agent) ?? null;
  try { return sessionStorage.getItem(storageKey(agent)); } catch { return null; }
}
export function rememberSetupId(agent: string, id: string): void {
  if (!SETUP_AGENTS.has(agent) || !/^[A-Za-z0-9-]{1,128}$/.test(id)) return;
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

const path = (agent: string) => agentOperationPath(agent, "setup");
export async function getAgentSetup(agent: string): Promise<SetupDetails> {
  const details = await request<SetupDetails>(path(agent));
  if (details.operation && operationRunning(details.operation)) rememberSetupId(agent, details.operation.id);
  return details;
}
export async function startAgentSetup(r: SetupRequest, id: string): Promise<SetupProgress> {
  rememberSetupId(r.agent.id, id);
  try {
    const operation = await request<SetupProgress>(path(r.agent.id), postJson({ workspace_id: r.workspaceId, action: r.action, request_id: id }));
    rememberSetupId(r.agent.id, operation.id);
    return operation;
  } catch (error) {
    if (error instanceof ApiError) acknowledgeSetupResult(r.agent.id, id);
    throw error;
  }
}
export const cancelAgentSetup = (agent: string, id: string) => request<SetupProgress>(`${path(agent)}/${encodeURIComponent(id)}`, { method: "DELETE" });
