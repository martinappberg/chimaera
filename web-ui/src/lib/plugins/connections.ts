import { api, ApiError } from "../net/api";
import type { AgentId } from "./store";

export interface Connection {
  name: string;
  kind: "mcp" | "app";
  status: string;
  source: string;
  login: boolean;
}

export interface AgentConnections {
  agent: AgentId;
  available: boolean;
  version?: string;
  connections: Connection[];
  errors: string[];
  notice?: string;
  actions?: string[];
}

export interface ConnectionsReport { host: string; agents: AgentConnections[] }

export const CLAUDE_CONNECTIONS_URL = "https://claude.ai/customize/connectors/yours";

export function connectionStatus(status: string, source?: string): string {
  switch (status) {
    case "connected": return "Connected";
    case "authenticated": return "Signed in";
    case "configured": return "Configured";
    case "available": return "Available to Codex";
    case "needs_auth": return source === "claude.ai" ? "Setup needed in Claude" : "Sign-in needed";
    case "needs_approval": return "Approval needed in the agent";
    case "disabled": return "Disabled";
    case "failed": return "Couldn't connect";
    case "unavailable": return "Unavailable to Codex";
    default: return "Status not confirmed";
  }
}

export function managementUrl(connection: Connection): string | null {
  if (connection.kind === "app") return "https://chatgpt.com/apps";
  if (connection.source === "claude.ai") return CLAUDE_CONNECTIONS_URL;
  return null;
}

async function read<T>(response: Response): Promise<T> {
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new ApiError(response.status, body.error || `Request failed (${response.status})`);
  return body as T;
}

export async function fetchConnections(workspace: string, refresh: boolean): Promise<ConnectionsReport> {
  const body = await read<ConnectionsReport>(await api(`/workspaces/${encodeURIComponent(workspace)}/connections${refresh ? "?refresh=true" : ""}`, { signal: AbortSignal.timeout(65000) }));
  return {
    host: typeof body.host === "string" ? body.host : "",
    agents: (Array.isArray(body.agents) ? body.agents : []).filter(a => typeof a.agent === "string" && /^[a-z][a-z0-9_.:-]{0,127}$/.test(a.agent)).map(a => ({
      ...a,
      available: a.available === true,
      connections: (Array.isArray(a.connections) ? a.connections : []).filter(c => typeof c.name === "string" && (c.kind === "mcp" || c.kind === "app")),
      errors: (Array.isArray(a.errors) ? a.errors : []).filter(e => typeof e === "string"),
    })),
  };
}

export interface ConnectionAuth {
  id: string;
  agent: AgentId;
  name: string;
  state: "starting" | "awaiting_browser" | "awaiting_callback" | "verifying" | "succeeded" | "failed" | "cancelled";
  authorization_url: string | null;
  message: string | null;
}

function authPath(workspace: string, id?: string): string {
  return `/workspaces/${encodeURIComponent(workspace)}/connections/login${id ? `/${encodeURIComponent(id)}` : ""}`;
}

export async function loginConnection(workspace: string, agent: AgentId, name: string): Promise<ConnectionAuth> {
  return read<ConnectionAuth>(await api(authPath(workspace), {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ agent, name }),
    signal: AbortSignal.timeout(10000),
  }));
}

export async function connectionAuthStatus(workspace: string, id: string): Promise<ConnectionAuth> {
  return read<ConnectionAuth>(await api(authPath(workspace, id), { signal: AbortSignal.timeout(10000) }));
}

export async function connectionAuthAction(workspace: string, id: string, action: "cancel" | "check" | "callback", url?: string): Promise<void> {
  const response = await api(authPath(workspace, id) + (action === "cancel" ? "" : `/${action}`), {
    method: action === "cancel" ? "DELETE" : "POST",
    headers: { "Content-Type": "application/json" },
    body: action === "callback" ? JSON.stringify({ url }) : undefined,
    signal: AbortSignal.timeout(10000),
  });
  if (!response.ok) await read(response);
}
