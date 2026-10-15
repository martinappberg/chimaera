/** One token-count spelling for the workbench, the drivers' `fmt_tokens`
 *  rule ("12.3k", "2.3M" from 999,950 up, whole numbers below a thousand):
 *  the chat's status line and message tooltips, the plugin cards. `compact`
 *  is the Activity table's column spelling: no decimal from 10k and 10M up
 *  ("45k", "1.5M"), so a column of totals lines up. */
export function formatTokenCount(n: number, compact = false): string {
  if (!Number.isFinite(n)) return "—";
  if (n >= 999_950) {
    const m = n / 1_000_000;
    return `${m.toFixed(compact && n >= 10_000_000 ? 0 : 1)}M`;
  }
  if (n >= 1000) {
    const k = n / 1000;
    return `${k.toFixed(compact && n >= 10_000 ? 0 : 1)}k`;
  }
  return String(Math.round(n));
}
