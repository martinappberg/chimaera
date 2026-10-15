/**
 * Sign in to an agent CLI from the app — the client half of `agent_auth`.
 * One flow for every runtime: the daemon runs the CLI's own login, the UI
 * shows the page (and code) it printed, feeds back a pasted code, and the
 * CLI's own status command is the only thing that ever says "signed in".
 */
import { api, ApiError } from "../net/api";
import { patchAgentSignedIn, type AgentInfo } from "./launcher";

export interface SignInProgress {
  id: string;
  how: "browser" | "terminal";
  phase: "running" | "cancelling" | "succeeded" | "failed" | "cancelled";
  workspace_id: string;
  started_at: number;
  /** The page to sign in on, once the CLI printed it. */
  url: string | null;
  /** A device code to confirm on that page (Codex, Grok). */
  code: string | null;
  /** The page will show a code to paste back (Claude). */
  accepts_code: boolean;
  /** The managed terminal running the CLI (the terminal flow). */
  terminal_session: string | null;
  message: string;
}

export interface SignInStatus {
  installed: boolean;
  signed_in: boolean | null;
  how: "browser" | "terminal";
  command: string;
  accepts_code: boolean;
  operation: SignInProgress | null;
}

export const signInRunning = (p: SignInProgress | null | undefined): boolean =>
  p?.phase === "running" || p?.phase === "cancelling";

/** Installed and the CLI says it is not signed in: the one state every
 *  surface answers with a Sign in. Unknown stays quiet. */
export function needsSignIn(a: AgentInfo | null | undefined): boolean {
  return a !== null && a !== undefined && a.installed && a.signedIn === false;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await api(path, { ...init, signal: AbortSignal.timeout(20_000) });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new ApiError(res.status, body.error ?? `Request failed (${res.status})`);
  }
  return res.json() as Promise<T>;
}
const path = (agent: string) => `/agents/${encodeURIComponent(agent)}/sign-in`;

/** The agent's status and its sign-in in progress. `check` asks the CLI
 *  again (a chat just failed to start; the user asked). The verdict is
 *  folded into the shared catalog. */
export async function getSignIn(agent: string, check = false): Promise<SignInStatus> {
  const status = await request<SignInStatus>(`${path(agent)}${check ? "?check=true" : ""}`);
  patchAgentSignedIn(agent, status.signed_in);
  return status;
}

export function startSignIn(agent: string, workspaceId: string, requestId: string, theme: "light" | "dark"): Promise<SignInProgress> {
  return request<SignInProgress>(path(agent), {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ workspace_id: workspaceId, request_id: requestId, theme }),
  });
}

export const cancelSignIn = (agent: string, id: string) =>
  request<SignInProgress>(`${path(agent)}/${encodeURIComponent(id)}`, { method: "DELETE" });

export const sendSignInCode = (agent: string, id: string, code: string) =>
  request<SignInProgress>(`${path(agent)}/${encodeURIComponent(id)}/code`, {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code }),
  });
