/**
 * Sign in to an agent CLI from the app — the client half of `agent_auth`.
 * One flow for every runtime: the daemon runs the CLI's own login, the UI
 * shows the page (and code) it printed, feeds back a pasted code, and the
 * CLI's own status command is the only thing that ever says "signed in".
 */
import { patchAgentSignedIn, type AgentInfo } from "./launcher";
import { agentOperationPath, postJson, request, type OperationProgress } from "./operations";

export interface SignInProgress extends OperationProgress {
  how: "browser";
  /** The page to sign in on, once the CLI printed it. */
  url: string | null;
  /** A device code to confirm on that page (Codex, Grok). */
  code: string | null;
  /** The page will show a code to paste back (Claude). */
  accepts_code: boolean;
  /** What the CLI itself printed (plain text, its newest part) — shown when
   *  its page isn't one the daemon recognizes, so that is never a dead end. */
  printed?: string;
}

export interface SignInStatus {
  installed: boolean;
  signed_in: boolean | null;
  how: "browser";
  command: string;
  accepts_code: boolean;
  relogin: boolean;
  operation: SignInProgress | null;
}

/** Installed and the CLI says it is not signed in: the one state every
 *  surface answers with a Sign in. Unknown stays quiet. */
export function needsSignIn(a: AgentInfo | null | undefined): boolean {
  return a !== null && a !== undefined && a.installed && a.signedIn === false;
}

const path = (agent: string) => agentOperationPath(agent, "sign-in");
/** Longer than an install call: `?check=true` runs the CLI's own status
 *  command inline, bounded at 10 s on the daemon. */
const TIMEOUT = 20_000;

/** The agent's status and its sign-in in progress. `check` asks the CLI
 *  again (a chat just failed to start; the user asked). The verdict is
 *  folded into the shared catalog. */
export async function getSignIn(agent: string, check = false): Promise<SignInStatus> {
  const status = await request<SignInStatus>(`${path(agent)}${check ? "?check=true" : ""}`, undefined, TIMEOUT);
  patchAgentSignedIn(agent, status.signed_in);
  return status;
}

export function startSignIn(agent: string, workspaceId: string, requestId: string, theme: "light" | "dark", sessionId: string | null = null): Promise<SignInProgress> {
  // `session_id`: the chat the sign-in was asked from, relaunched by the
  // daemon once the login lands.
  return request<SignInProgress>(path(agent), postJson({ workspace_id: workspaceId, request_id: requestId, theme, ...(sessionId ? { session_id: sessionId } : {}) }), TIMEOUT);
}

export const cancelSignIn = (agent: string, id: string) =>
  request<SignInProgress>(`${path(agent)}/${encodeURIComponent(id)}`, { method: "DELETE" }, TIMEOUT);

export const sendSignInCode = (agent: string, id: string, code: string) =>
  request<SignInProgress>(`${path(agent)}/${encodeURIComponent(id)}/code`, postJson({ code }), TIMEOUT);
