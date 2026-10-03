import { api } from "../net/api";
import type { Session } from "./sessions";

/** Parking is not proof that the secret update was applied. */
export function manualResumeNote(session: Pick<Session, "manual_resume_reason">): string | null {
  if (session.manual_resume_reason == null) return null;
  return session.manual_resume_reason === "project_secrets_idle"
    ? "Paused for a project secret update. Resume this conversation when the update is ready."
    : "This conversation is paused. Update chimaera to resume it.";
}

const UNCONFIRMED = "Couldn’t confirm the resume. Check this conversation’s status before trying again.";

/** Only a user click calls this. Never substitute a fresh Recents session or
 * retry a possibly accepted request automatically. */
export async function resumeParkedSession(session: Session): Promise<void> {
  const { id, workspace_id: workspace, kind, agent_kind: agent, manual_resume_reason: reason } = session;
  if (reason !== "project_secrets_idle" || kind !== "agent" || session.ui !== "chat"
    || !/^[A-Za-z0-9_-]{1,128}$/.test(id) || !/^[A-Za-z0-9_-]{1,128}$/.test(workspace)) {
    throw new Error("This conversation cannot be resumed here.");
  }
  let response: Response;
  try {
    response = await api(`/sessions/${encodeURIComponent(id)}/resume`, {
      method: "POST",
      signal: AbortSignal.timeout(30_000),
    });
  } catch {
    throw new Error(UNCONFIRMED);
  }
  if (!response.ok) {
    if (response.status === 401) throw new Error("Sign in again to resume this conversation.");
    if (response.status === 409) throw new Error("This conversation isn’t ready to resume. Wait for the project update, then try again.");
    if (response.status === 404) throw new Error("This conversation can’t be resumed here right now. Check its current project.");
    throw new Error(UNCONFIRMED);
  }
  let value: unknown;
  try { value = await response.json(); } catch { throw new Error(UNCONFIRMED); }
  if (typeof value !== "object" || value === null) throw new Error(UNCONFIRMED);
  const row = value as Partial<Session>;
  if (row.id !== id || row.workspace_id !== workspace || row.kind !== kind || row.ui !== "chat"
    || row.agent_kind !== agent || row.alive !== true || row.suspended === true
    || row.manual_resume_reason != null) throw new Error(UNCONFIRMED);
}
