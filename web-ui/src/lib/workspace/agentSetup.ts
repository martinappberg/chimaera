import { writable } from "svelte/store";
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
