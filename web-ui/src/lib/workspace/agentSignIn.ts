/**
 * Sign in to an agent CLI from the app — the client half of `agent_auth`.
 * One flow for every runtime: the daemon runs the CLI's own login, the UI
 * shows the page (and code) it printed, feeds back a pasted code, and the
 * CLI's own status command is the only thing that ever says "signed in".
 */
import { patchAgentSignedIn, type AgentInfo } from "./launcher";
import { agentOperationPath, operationRunning, postJson, request, type OperationProgress } from "./operations";

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

/** What a card's first read of the agent's `operation` makes history: a
 *  finished attempt. The daemon keeps an agent's last result until the next
 *  attempt, so that one is another card's, an hour ago. A running one is
 *  the card's to show (a repeated click, a second window), and so is every
 *  attempt after it. */
export function historyAttempt(first: OperationProgress | null | undefined): string | null {
  return first && !operationRunning(first) ? first.id : null;
}

/** Whether a card shows `operation`: anything but its history. */
export function attemptShown(history: string | null, operation: OperationProgress | null | undefined): boolean {
  return operation !== null && operation !== undefined && operation.id !== history;
}

/** Whether `operation` is a sign-in that landed since the card opened: its
 *  own, or one finished elsewhere meanwhile (Settings, another window).
 *  Never the history: read as this card's success, that one closed a chat's
 *  `/login` card before it could start a login, so nobody could sign in a
 *  second time. */
export function attemptLanded(history: string | null, operation: OperationProgress | null | undefined): boolean {
  return attemptShown(history, operation) && operation?.phase === "succeeded";
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
