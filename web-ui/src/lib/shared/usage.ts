/**
 * Plan usage and the context window, read the same way wherever they show:
 * the chat header's usage glyph and popover, the /usage panel, and Agent
 * settings. Pure, so the thresholds and the wording are pinned by tests.
 */

/** One limit window, as every surface draws it. */
export interface UsageRow {
  label: string;
  /** 0-100. */
  utilization: number;
  /** Epoch seconds (the streamed events, Codex) or an ISO timestamp
   *  (Claude's `get_usage`); null when unknown. */
  resetsAt: string | null;
  /** 0-100: a point on the bar worth marking (the context row's
   *  auto-compact threshold); absent on every limit window. */
  mark?: number | null;
}

/** The wire's `{label, utilization, resets_at?}` windows. Labels are unique
 *  on the wire (the rows are keyed by them); a repeat is dropped here too. */
export function usageRows(
  windows: readonly { label?: unknown; utilization?: unknown; resets_at?: unknown }[] | null | undefined,
): UsageRow[] {
  const rows: UsageRow[] = [];
  for (const w of windows ?? []) {
    if (typeof w.label !== "string" || typeof w.utilization !== "number") continue;
    if (rows.some((r) => r.label === w.label)) continue;
    rows.push({
      label: w.label,
      utilization: Math.min(100, Math.max(0, w.utilization)),
      resetsAt: typeof w.resets_at === "string" ? w.resets_at : null,
    });
  }
  return rows;
}

/** Epoch seconds (all digits) or an ISO timestamp → epoch ms. */
export function resetsAtMs(resetsAt: string | null): number | null {
  if (resetsAt === null) return null;
  const text = resetsAt.trim();
  if (/^\d+$/.test(text)) return Number(text) * 1000;
  const at = Date.parse(text);
  return Number.isNaN(at) ? null : at;
}

const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;

/** When a window resets, in the fewest words: "in 40m" and "in 3h 12m"
 *  within a day, "Thu 09:00" within a week, else "Oct 21". */
export function resetsLabel(resetsAt: string | null, nowMs: number, locale?: string): string | null {
  const at = resetsAtMs(resetsAt);
  if (at === null) return null;
  const left = at - nowMs;
  if (left <= 0) return "now";
  const minutes = Math.ceil(left / MINUTE);
  if (minutes < 60) return `in ${minutes}m`;
  if (left < DAY) {
    const hours = Math.floor(minutes / 60);
    const rest = minutes % 60;
    return rest === 0 ? `in ${hours}h` : `in ${hours}h ${rest}m`;
  }
  const when = new Date(at);
  if (left < 7 * DAY) {
    return when.toLocaleString(locale, { weekday: "short", hour: "2-digit", minute: "2-digit" });
  }
  return when.toLocaleDateString(locale, { month: "short", day: "numeric" });
}

/** Quiet below 80%; warn from there; full at 100% or when the agent says
 *  requests are refused. */
export type UsageLevel = "quiet" | "warn" | "full";
export const USAGE_WARN_PCT = 80;

export function usageLevel(pct: number, refused = false): UsageLevel {
  if (refused || pct >= 100) return "full";
  return pct >= USAGE_WARN_PCT ? "warn" : "quiet";
}

/** What the header says when it says anything: the most pressing of the
 *  context window and the limit windows. Null while everything is quiet —
 *  the header then shows only its faint glyph. */
export interface UsageHeadline {
  /** "context", or the window's label without " limit" ("weekly"). */
  label: string;
  pct: number;
  level: UsageLevel;
}

export function usageHeadline(
  contextPct: number | null,
  limits: readonly UsageRow[],
  limitReached: boolean,
): UsageHeadline | null {
  const worstLimit = limits.reduce<UsageRow | null>(
    (worst, row) => (worst === null || row.utilization > worst.utilization ? row : worst),
    null,
  );
  const short = (label: string) => label.replace(/\s+limit$/i, "");
  if (limitReached) {
    return {
      label: worstLimit !== null ? short(worstLimit.label) : "usage",
      pct: worstLimit?.utilization ?? 100,
      level: "full",
    };
  }
  const candidates: { label: string; pct: number }[] = limits.map((row) => ({
    label: short(row.label),
    pct: row.utilization,
  }));
  if (contextPct !== null) candidates.push({ label: "context", pct: contextPct });
  const top = candidates.reduce<{ label: string; pct: number } | null>(
    (best, c) => (best === null || c.pct > best.pct ? c : best),
    null,
  );
  if (top === null) return null;
  const level = usageLevel(top.pct);
  return level === "quiet" ? null : { label: top.label, pct: top.pct, level };
}

/** A label as a row heading: "weekly limit" → "Weekly limit". */
export function usageTitle(label: string): string {
  return label.length === 0 ? label : label[0].toUpperCase() + label.slice(1);
}
