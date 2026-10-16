/**
 * `GET /api/v1/agents/{id}/usage` — the agent account's plan windows, read
 * live from the agent's own CLI (Claude's `get_usage`, Codex's
 * `account/rateLimits/read`, Grok's `_x.ai/billing`, Antigravity's
 * `/usage`; no model call). The daemon caches an answer for a minute;
 * `refresh` asks again.
 */
import { api } from "../net/api";
import { usageRows, type UsageRow } from "../shared/usage";

/** Agents whose CLI reports plan usage a host can read (Grok through its
 *  billing extension, Antigravity through its CLI's `/usage`). Cursor
 *  reports none (the daemon answers 404), so nothing shows for it. */
export const USAGE_AGENTS: ReadonlySet<string> = new Set(["claude", "codex", "grok", "agy"]);

/** Agents whose chat stream never reports plan limits, so a chat's meter
 *  shows the account's read instead. The others report them with their own
 *  requests; asking ahead would spawn a CLI for numbers already on the way. */
export const ACCOUNT_USAGE_IN_CHAT: ReadonlySet<string> = new Set(["agy"]);

export interface AgentUsage {
  rows: UsageRow[];
  limitReached: boolean;
  checkedAtMs: number;
  /** The CLI couldn't answer (signed out, offline); its words. */
  error: string | null;
}

export async function getAgentUsage(id: string, refresh = false): Promise<AgentUsage> {
  const res = await api(`/agents/${encodeURIComponent(id)}/usage${refresh ? "?refresh=true" : ""}`);
  if (!res.ok) throw new Error(`Couldn't read usage (${res.status})`);
  const body = (await res.json()) as Record<string, unknown>;
  return {
    rows: usageRows(body.windows as Parameters<typeof usageRows>[0]),
    limitReached: body.limit_reached === true,
    checkedAtMs: typeof body.checked_at_ms === "number" ? body.checked_at_ms : Date.now(),
    error: typeof body.error === "string" ? body.error : null,
  };
}
