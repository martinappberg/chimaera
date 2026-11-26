/**
 * Who a chat can reference as `@agent:` and how one is opened: the half of
 * `agentMentions.ts` that App itself imports. App is in the always-loaded
 * entry (a size budget the build enforces), so this file imports nothing of
 * the chat's: the parsing and the composer's short forms stay in
 * `agentMentions.ts`, which loads with the chat.
 */

import { writable } from "svelte/store";

export interface AgentOption {
  id: string;
  /** As its tab and rail row name it. */
  name: string;
  /** The server's `agent_kind`, for its glyph. */
  agentKind: string | null;
  workspaceId: string;
  alive: boolean;
  /** What it is doing, in the rail's words ("agent working", "finished"). */
  status?: string;
  cwd?: string;
}

/** The agent sessions of this window's workspace, ended ones included (a
 *  sent message keeps its chip's name after the agent it names has ended). */
export const mentionableAgents = writable<AgentOption[]>([]);

/**
 * The panes a dragged session is referenced in, as `@agent:`: each one
 * showing another live agent of its workspace. Never its own pane, never a
 * shell (a reference to an agent is for an agent to act on). Undefined when
 * the dragged session is not a live agent here: the drag is a plain tab move.
 */
export function agentRefPanes(
  draggedId: string,
  shown: readonly { paneId: string; sessionId: string }[],
  agents: readonly AgentOption[],
): Set<string> | undefined {
  const dragged = agents.find((a) => a.id === draggedId);
  if (dragged === undefined || !dragged.alive) return undefined;
  const out = new Set<string>();
  for (const pane of shown) {
    if (pane.sessionId === draggedId) continue;
    const peer = agents.find((a) => a.id === pane.sessionId);
    if (peer !== undefined && peer.alive && peer.workspaceId === dragged.workspaceId) out.add(pane.paneId);
  }
  return out;
}

type AgentOpener = (id: string, fromPane: string | null) => void;
let opener: AgentOpener | null = null;

/** App-level wiring (null unregisters). */
export function setAgentOpener(fn: AgentOpener | null): void {
  opener = fn;
}

/** Show the agent `id`, opened from the element `from` (its pane decides
 *  where a not-yet-open agent lands: beside, never over what is being
 *  read). False when nothing can. */
export function openAgent(id: string, from: Element | null = null): boolean {
  return openAgentFrom(id, from?.closest(".pane")?.getAttribute("data-pane-id") ?? null);
}

/** `openAgent` for a caller that knows the pane (or only that there is none). */
export function openAgentFrom(id: string, paneId: string | null): boolean {
  if (opener === null) return false;
  opener(id, paneId);
  return true;
}

/** A picked or dropped agent always travels by id: names collide and change. */
export function agentReference(sessionId: string): string {
  return `@agent:${sessionId}`;
}
