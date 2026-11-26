/**
 * What a chat says while its agent reports the account's limit as reached
 * (the `rate_limit` event's `limit_reached`: Claude's `rejected`, Codex's
 * refused usage, Grok's spent credit period). Pure, so the wording is pinned
 * by tests. The words are the agent's own: its window label ("session
 * limit", "weekly Opus limit"), never a plan name the chat made up.
 */
import { resetsAtMs, resetsLabel, usageTitle } from "../shared/usage";
import type { AgentInfo } from "../workspace/launcher";

/** The part of the store's limit state the card reads. */
export interface ReachedLimit {
  label: string | null;
  resetsAt: string | null;
  limitReached: boolean;
}

export interface LimitNotice {
  /** "Session limit reached". */
  what: string;
  /** "resets in 2h 14m", or null when the agent gave no time or it has
   *  passed. */
  resets: string | null;
}

/** Null while no limit is reached. A reset time that has passed drops out of
 *  the line, and the line stays: only the agent says a limit is over (its
 *  next request's telemetry), never the clock. */
export function limitNotice(limit: ReachedLimit | null, nowMs: number, locale?: string): LimitNotice | null {
  if (limit === null || !limit.limitReached) return null;
  const at = resetsAtMs(limit.resetsAt);
  const when = at !== null && at > nowMs ? resetsLabel(limit.resetsAt, nowMs, locale) : null;
  return {
    what: `${usageTitle(limit.label ?? "usage limit")} reached`,
    resets: when === null ? null : `resets ${when}`,
  };
}

/** Whether this agent's sign-in can put another account in place: the
 *  daemon's own descriptor on the catalog row (none = no sign-in from the
 *  app; `relogin: false` = its login only finds the existing account).
 *  Never the agent's name. */
export function canSwitchAccount(agent: AgentInfo | null | undefined): boolean {
  return agent !== null && agent !== undefined && agent.installed && agent.signIn !== undefined && agent.signIn.relogin !== false;
}
