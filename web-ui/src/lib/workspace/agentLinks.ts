/**
 * Linked terminals: user-granted edges between an agent session and the
 * terminal sessions it may reach (one agent per terminal — a re-link moves
 * the leash; an agent may hold many). The daemon owns the edges; the UI
 * mirrors them from /ws/events snapshots and mutates via PUT/DELETE.
 *
 * Browser tabs link the same way (the agent browser, #273): a shared
 * browser pane is one the agent's browser tools may read and drive. Same
 * one-agent-per-tab rule; mirrored from the sessions frame's
 * `browser_links` (`browser/agentBrowserRuntime.ts`).
 */

import { api, ApiError } from "../net/api";
import { decodeBrowserLinks, type BrowserLink } from "../net/browserWire";

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
  /** Share browser tab `tab` with an agent (moves it from any other). */
  linkBrowser(tab: string, agentId: string): void;
  unlinkBrowser(tab: string): void;
  /** Show browser tab `tab` in its pane (an agent pane's chip). */
  revealBrowser(tab: string): void;
  /** What a chip calls browser tab `tab`; null when this window doesn't
   *  hold it (no chip: there is nothing here to reveal). */
  browserLabel(tab: string): string | null;
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

export async function putBrowserLink(tab: string, agentId: string): Promise<void> {
  await ok(
    await api("/links/browser", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ tab, agent_id: agentId }),
    }),
  );
}

export async function deleteBrowserLink(tab: string): Promise<void> {
  await ok(await api(`/links/browser/${encodeURIComponent(tab)}`, { method: "DELETE" }));
}

/** Every shared browser tab (the same list as the sessions frame's
 *  `browser_links`): the fallback poll's source while `/ws/events` is down. */
export async function listBrowserLinks(): Promise<BrowserLink[]> {
  const res = await api("/links/browser");
  if (!res.ok) throw new ApiError(res.status, "failed to list browser links");
  return decodeBrowserLinks(await res.json()) ?? [];
}

/** A selected terminal always travels by ID: display names can collide or change. */
export function termReference(sessionId: string): string {
  return `@term:${sessionId}`;
}
