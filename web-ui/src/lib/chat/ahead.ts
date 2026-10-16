/**
 * Work that resumes the agent on its own — Claude's `/loop` (a pending
 * wakeup, interval jobs) and goals (Claude `/goal`, Codex thread goals) —
 * as the Ahead card, the transcript lines and the rail read it. Pure: the
 * store folds the wire through `parseSchedule` / `parseGoal` / `goalLines`,
 * the card formats with `untilLabel` / `ringFraction`.
 *
 * Words are the agents' own. Claude's goal reads "Goal achieved", "Goal could
 * not be achieved", "Goal not yet met" and counts "turns"; Codex's statuses
 * are its TUI's ("paused", "stalled", "usage limited", "limited by budget").
 */

export interface ScheduledWakeup {
  atMs: number;
  scheduledAtMs: number;
  /** The agent's own reason. */
  reason: string;
  /** What the wakeup will send. */
  prompt: string;
}

export interface ScheduledJob {
  id: string;
  cron: string;
  /** The agent's words for the schedule ("Every 10 minutes"); may be "". */
  schedule: string;
  prompt: string;
  recurring: boolean;
  nextAtMs: number | null;
  createdAtMs: number;
}

export interface GoalInfo {
  objective: string;
  /** Verbatim: codex active|paused|blocked|usageLimited|budgetLimited|complete;
   *  claude active. */
  status: string;
  setAtMs: number;
  /** Claude: checks so far (Claude calls them turns). */
  turns: number | null;
  lastCheck: string | null;
  tokensUsed: number | null;
  tokenBudget: number | null;
  timeUsedSecs: number | null;
}

/** A goal transcript line: one quiet activity row. */
export interface GoalLine {
  label: string;
  detail: string | null;
  /** Hover text (Claude's own reason for an end). */
  title: string | null;
  tone: "plain" | "done" | "warn";
}

const str = (v: unknown, max = 4000): string => (typeof v === "string" ? v.slice(0, max) : "");
const num = (v: unknown): number | null =>
  typeof v === "number" && Number.isFinite(v) && v >= 0 ? v : null;

export function parseSchedule(ev: Record<string, unknown>): {
  wakeup: ScheduledWakeup | null;
  jobs: ScheduledJob[];
} {
  const w = ev.wakeup as Record<string, unknown> | null | undefined;
  const wakeup =
    w && typeof w === "object" && num(w.at_ms) !== null
      ? {
          atMs: num(w.at_ms) as number,
          scheduledAtMs: num(w.scheduled_at_ms) ?? (num(w.at_ms) as number),
          reason: str(w.reason, 500),
          prompt: str(w.prompt, 500),
        }
      : null;
  const seen = new Set<string>();
  const jobs: ScheduledJob[] = [];
  for (const raw of Array.isArray(ev.jobs) ? ev.jobs.slice(0, 50) : []) {
    const j = raw as Record<string, unknown>;
    const id = str(j?.id, 64);
    // Keyed rows: an id the wire repeated must not take the card down.
    if (id === "" || seen.has(id)) continue;
    seen.add(id);
    jobs.push({
      id,
      cron: str(j.cron, 256),
      schedule: str(j.schedule, 500),
      prompt: str(j.prompt, 500),
      recurring: j.recurring === true,
      nextAtMs: num(j.next_at_ms),
      createdAtMs: num(j.created_at_ms) ?? 0,
    });
  }
  return { wakeup, jobs };
}

export function parseGoal(raw: unknown): GoalInfo | null {
  const g = raw as Record<string, unknown> | null | undefined;
  if (!g || typeof g !== "object" || str(g.objective) === "") return null;
  return {
    objective: str(g.objective),
    status: str(g.status, 40) || "active",
    setAtMs: num(g.set_at_ms) ?? 0,
    turns: num(g.turns),
    lastCheck: str(g.last_check, 1000) || null,
    tokensUsed: num(g.tokens_used),
    tokenBudget: num(g.token_budget),
    timeUsedSecs: num(g.time_used_secs),
  };
}

/** A goal still in play — not finished. Codex keeps a finished goal on its
 *  thread as `complete`. */
export function goalLive(goal: GoalInfo | null): goal is GoalInfo {
  return goal !== null && goal.status !== "complete";
}

/** Codex's own words for a stopped goal (its TUI's); `null` while it runs. */
export function goalStatusWord(status: string): string | null {
  switch (status) {
    case "active":
    case "complete":
      return null;
    case "blocked":
      return "stalled";
    case "usageLimited":
      return "usage limited";
    case "budgetLimited":
      return "limited by budget";
    default:
      return status;
  }
}

/** "41k", "1.2M" — token counts at a glance. */
export function compactCount(n: number): string {
  if (n < 1000) return String(Math.round(n));
  if (n < 1_000_000) return `${n < 10_000 ? (n / 1000).toFixed(1).replace(/\.0$/, "") : Math.round(n / 1000)}k`;
  return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, "")}M`;
}

/** "14m", "1h 05m", "40s". */
export function shortDuration(secs: number): string {
  if (secs < 60) return `${Math.max(1, Math.round(secs))}s`;
  const m = Math.round(secs / 60);
  if (m < 60) return `${m}m`;
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, "0")}m`;
}

/** The lines a goal event adds to the transcript, given the goal before it.
 *  `lastObjective` is the last goal this transcript named: a process restart
 *  re-reports the same goal, which is not a new one. */
export function goalLines(
  prev: GoalInfo | null,
  next: GoalInfo | null,
  ended: Record<string, unknown> | null,
  lastObjective: string | null,
): GoalLine[] {
  const lines: GoalLine[] = [];
  if (next !== null && goalLive(next) && next.objective !== lastObjective && (prev === null || prev.objective !== next.objective)) {
    lines.push({ label: "Goal set", detail: next.objective, title: null, tone: "plain" });
  } else if (next !== null && prev !== null && prev.objective === next.objective) {
    // A failed check (Claude): its count moved and it says why.
    if ((next.turns ?? 0) > (prev.turns ?? 0) && next.lastCheck !== null) {
      lines.push({ label: "Goal not yet met", detail: next.lastCheck, title: null, tone: "plain" });
    }
    if (next.status !== prev.status && next.status !== "complete") {
      const word = goalStatusWord(next.status);
      if (word === null && goalStatusWord(prev.status) !== null) {
        lines.push({ label: "Goal resumed", detail: null, title: null, tone: "plain" });
      } else if (word !== null) {
        const label =
          next.status === "paused"
            ? "Goal paused"
            : next.status === "blocked"
              ? "Goal stalled"
              : next.status === "usageLimited"
                ? "Goal hit its usage limit"
                : next.status === "budgetLimited"
                  ? "Goal reached its token budget"
                  : `Goal ${word}`;
        lines.push({ label, detail: null, title: null, tone: next.status === "paused" ? "plain" : "warn" });
      }
    }
  }
  if (ended !== null) {
    const outcome = str(ended.outcome, 40);
    const stats = [
      num(ended.duration_secs) !== null ? shortDuration(num(ended.duration_secs) as number) : null,
      num(ended.turns) !== null ? `${ended.turns} ${ended.turns === 1 ? "turn" : "turns"}` : null,
      num(ended.tokens) !== null ? `${compactCount(num(ended.tokens) as number)} tokens` : null,
    ]
      .filter((s) => s !== null)
      .join(" · ");
    const reason = str(ended.reason, 1000) || null;
    if (outcome === "achieved") {
      lines.push({ label: "Goal achieved", detail: stats || null, title: reason, tone: "done" });
    } else if (outcome === "failed") {
      lines.push({ label: "Goal could not be achieved", detail: reason, title: null, tone: "warn" });
    } else if (outcome === "cleared") {
      lines.push({ label: "Goal cleared", detail: str(ended.objective) || null, title: null, tone: "plain" });
    }
  }
  return lines;
}

/** When a scheduled thing happens, said the calm way: minutes while it is
 *  far, seconds only in its last minute, a clock time past an hour, a day
 *  name past today. `null` at = unknown. */
export function untilLabel(atMs: number | null, nowMs: number): string {
  if (atMs === null) return "";
  const left = atMs - nowMs;
  if (left <= 0) return "now";
  const secs = Math.ceil(left / 1000);
  if (secs < 60) return `in ${secs}s`;
  // To the nearest minute: 75 s is "in 1m", not a promise of two.
  const mins = Math.round(secs / 60);
  if (mins < 60) return `in ${mins}m`;
  const at = new Date(atMs);
  const now = new Date(nowMs);
  const time = at.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  if (at.toDateString() === now.toDateString()) return `at ${time}`;
  const tomorrow = new Date(nowMs);
  tomorrow.setDate(now.getDate() + 1);
  if (at.toDateString() === tomorrow.toDateString()) return `tomorrow ${time}`;
  return `${at.toLocaleDateString([], { weekday: "short" })} ${time}`;
}

/** The countdown ring's fill: the share of the wait still to go. */
export function ringFraction(atMs: number | null, fromMs: number, nowMs: number): number {
  if (atMs === null || atMs <= fromMs) return 0;
  return Math.min(1, Math.max(0, (atMs - nowMs) / (atMs - fromMs)));
}

/** A recurring job's span for its ring: the gap between runs when the cron
 *  is a plain step (`*\/N * * * *`, `0 *\/N * * *`), else ten minutes. */
export function jobPeriodMs(cron: string): number {
  const f = cron.trim().split(/\s+/);
  if (f.length !== 5) return 600_000;
  const step = (field: string) => (/^\*\/(\d+)$/.exec(field) ? Number(/^\*\/(\d+)$/.exec(field)?.[1]) : null);
  if (step(f[0]) !== null && f.slice(1).every((x) => x === "*")) return (step(f[0]) as number) * 60_000;
  if (f[0] === "*" && f.slice(1).every((x) => x === "*")) return 60_000;
  if (/^\d+$/.test(f[0]) && step(f[1]) !== null && f.slice(2).every((x) => x === "*")) return (step(f[1]) as number) * 3_600_000;
  if (/^\d+$/.test(f[0]) && f[1] === "*" && f.slice(2).every((x) => x === "*")) return 3_600_000;
  if (/^\d+$/.test(f[0]) && /^\d+$/.test(f[1]) && f.slice(2).every((x) => x === "*")) return 86_400_000;
  return 600_000;
}
