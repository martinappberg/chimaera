/**
 * Linked terminals: user-granted edges between an agent session and the
 * terminal sessions it may reach (one agent per terminal — a re-link moves
 * the leash; an agent may hold many). The daemon owns the edges; the UI
 * mirrors them from /ws/events snapshots and mutates via PUT/DELETE.
 */

import { api, ApiError } from "./api";

export interface Link {
  terminal_id: string;
  agent_id: string;
}

/** Link mutations + reveal, implemented by App, used by pane top bars. */
export interface LinkCtrl {
  /**
   * Focus `sessionId`'s view, opening it as a split beside `besidePaneId`
   * when it isn't open anywhere (the auto-reveal on link).
   */
  reveal(sessionId: string, besidePaneId: string): void;
  link(terminalId: string, agentId: string): void;
  unlink(terminalId: string): void;
}

/**
 * Deterministic accent hue for an agent session, painted on everything the
 * link touches (chips, linked-pane borders, exec pulses). A small curated
 * palette keeps hues distinguishable and clear of the semantic colors
 * (accent green, warn amber, err red, rate purple).
 */
const AGENT_HUES = [205, 265, 320, 180, 230] as const;

export function agentHue(agentId: string): number {
  let h = 0;
  for (let i = 0; i < agentId.length; i++) {
    h = (h * 31 + agentId.charCodeAt(i)) >>> 0;
  }
  return AGENT_HUES[h % AGENT_HUES.length];
}

async function ok(res: Response): Promise<void> {
  if (!res.ok) {
    let message = `request failed with status ${res.status}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // non-JSON error body; keep the generic message
    }
    throw new ApiError(res.status, message);
  }
}

export async function listLinks(): Promise<Link[]> {
  const res = await api("/links");
  if (!res.ok) throw new ApiError(res.status, "failed to list links");
  return (await res.json()) as Link[];
}

export async function putLink(terminalId: string, agentId: string): Promise<void> {
  await ok(
    await api("/links", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ terminal_id: terminalId, agent_id: agentId }),
    }),
  );
}

export async function deleteLink(terminalId: string): Promise<void> {
  await ok(await api(`/links/${terminalId}`, { method: "DELETE" }));
}

/** The `@term:` reference for a terminal, quoted when the name needs it. */
export function termReference(name: string): string {
  return /\s/.test(name) ? `@term:"${name}"` : `@term:${name}`;
}
