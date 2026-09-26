/**
 * Client half of the opt-in "teach agents the document dialect" installs
 * (`/api/v1/agent-docs`): a documents section in the workspace's AGENTS.md
 * and a Claude Code skill, for agents launched outside Chimaera. The daemon
 * owns the text; GET returns exactly what an install writes, so the confirm
 * dialog can show it verbatim.
 */

import { api, ApiError } from "../net/api";

export type AgentDocsTarget = "agents_md" | "claude_skill";

/**
 * `installed` (current), `outdated` (present, different), `absent` (no
 * section / no skill), `no_file` (no AGENTS.md yet), `broken` (an unmatched
 * marker in AGENTS.md), `unreadable`.
 */
export type AgentDocsState =
  | "installed"
  | "outdated"
  | "absent"
  | "no_file"
  | "broken"
  | "unreadable";

export interface AgentDocsEntry {
  target: AgentDocsTarget;
  /** Absolute path on the daemon host. */
  path: string;
  state: AgentDocsState;
  /** Exactly what an install writes (the AGENTS.md block, or SKILL.md). */
  text: string;
}

export interface AgentDocsInstall {
  target: AgentDocsTarget;
  path: string;
  changed: boolean;
  created: boolean;
}

async function errorFrom(res: Response): Promise<ApiError> {
  let message = `request failed with status ${res.status}`;
  try {
    const body = (await res.json()) as { error?: string };
    if (body.error) message = body.error;
  } catch {
    // non-JSON error body; keep the generic message
  }
  return new ApiError(res.status, message);
}

/** GET /api/v1/agent-docs — each target's path, state and text. */
export async function getAgentDocs(workspaceId: string | null): Promise<AgentDocsEntry[]> {
  const q = workspaceId !== null ? `?${new URLSearchParams({ workspace_id: workspaceId })}` : "";
  const res = await api(`/agent-docs${q}`);
  if (!res.ok) throw await errorFrom(res);
  const body = (await res.json()) as { targets?: unknown };
  return Array.isArray(body.targets) ? (body.targets as AgentDocsEntry[]) : [];
}

/** POST /api/v1/agent-docs/install — idempotent (`changed: false` when current). */
export async function installAgentDocs(
  target: AgentDocsTarget,
  workspaceId: string | null,
): Promise<AgentDocsInstall> {
  const res = await api("/agent-docs/install", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(
      workspaceId !== null ? { target, workspace_id: workspaceId } : { target },
    ),
  });
  if (!res.ok) throw await errorFrom(res);
  return (await res.json()) as AgentDocsInstall;
}
