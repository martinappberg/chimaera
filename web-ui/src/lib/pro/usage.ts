export interface UsagePresentation {
  label: string;
  progress: number | null;
  detail: string | null;
  limited: boolean;
}

/** Account limits are authoritative; an absent or zero allowance is not 0% used. */
export function usagePresentation(used: number | null | undefined, limit: number | null | undefined): UsagePresentation {
  if (typeof used !== "number" || typeof limit !== "number" || !Number.isFinite(used) || !Number.isFinite(limit) || used < 0 || limit < 0) {
    return { label: "Usage unavailable", progress: null, detail: null, limited: false };
  }
  if (limit === 0) return used === 0
    ? { label: "No allowance", progress: null, detail: null, limited: false }
    : { label: "Over limit", progress: 100, detail: "No current allowance", limited: true };
  const percent = used / limit * 100;
  const rounded = Math.round(percent * 10) / 10;
  const amount = percent > 999 ? ">999" : percent === 0 && used === 0 ? "0"
    : percent < 0.1 ? "<0.1"
    : percent < 100 && rounded === 100 ? ">99.9"
    : percent > 100 && rounded === 100 ? ">100"
    : new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(percent);
  return {
    label: Number.isFinite(percent) ? `${amount}% used` : "Over limit",
    progress: Math.max(0, Math.min(100, percent)),
    detail: used > limit ? "Over limit" : used === limit ? "Limit reached" : null,
    limited: used >= limit,
  };
}
