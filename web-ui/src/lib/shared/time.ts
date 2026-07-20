/**
 * The chat surface's one elapsed/duration ladder — "7s", "1m 04s",
 * "1h 02m 03s": leading zero-units dropped, lower units zero-padded.
 * Shared by the turn timer, the turn-end badge, and the background tray's
 * elapsed column so the same concept can't render three subtly different
 * ways in one pane. Whole seconds in: callers own their ms→s floor and any
 * gating (the turn timer hides below 5s; the turn-end badge keeps a
 * decimal for sub-minute durations).
 */
export function formatElapsedSeconds(total: number): string {
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => n.toString().padStart(2, "0");
  if (h > 0) return `${h}h ${pad(m)}m ${pad(s)}s`;
  if (m > 0) return `${m}m ${pad(s)}s`;
  return `${s}s`;
}

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

/** Local calendar-day ordinal without DST-length assumptions. */
function localDay(date: Date): number {
  return Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / DAY_MS;
}

function localTime(date: Date, locale?: string): string {
  return new Intl.DateTimeFormat(locale, {
    hour: "numeric",
    minute: "2-digit",
  }).format(date);
}

/**
 * Human-scale timestamp for an assistant message. Relative labels are useful
 * only while they stay compact; older messages switch to the user's locale,
 * hour cycle, and calendar formatting so the transcript remains scannable.
 */
export function formatMessageTimestamp(
  timestampMs: number,
  nowMs = Date.now(),
  locale?: string,
): string {
  const sent = new Date(timestampMs);
  const now = new Date(nowMs);
  if (!Number.isFinite(timestampMs) || Number.isNaN(sent.getTime())) return "";

  const ageMs = Math.max(0, nowMs - timestampMs);
  if (ageMs < MINUTE_MS) return "now";
  if (ageMs < HOUR_MS) return `${Math.floor(ageMs / MINUTE_MS)}m ago`;
  if (ageMs < 2 * HOUR_MS) return "1h ago";

  const daysAgo = localDay(now) - localDay(sent);
  const time = localTime(sent, locale);
  if (daysAgo === 0) return `${time} today`;
  if (daysAgo === 1) return `${time} yesterday`;

  // A nearby day is easier to find by name than by date. Once it leaves that
  // one-week window, include the calendar date; include the year only when the
  // message no longer belongs to the viewer's current calendar year.
  if (daysAgo >= 2 && daysAgo <= 7) {
    const weekday = new Intl.DateTimeFormat(locale, { weekday: "short" }).format(sent);
    return `${weekday} ${time}`;
  }
  if (sent.getFullYear() === now.getFullYear()) {
    return new Intl.DateTimeFormat(locale, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    }).format(sent);
  }
  return new Intl.DateTimeFormat(locale, {
    year: "numeric",
    month: "short",
    day: "numeric",
  }).format(sent);
}

/** Full local timestamp for the hover tooltip behind the compact label. */
export function formatFullTimestamp(timestampMs: number, locale?: string): string {
  const date = new Date(timestampMs);
  if (!Number.isFinite(timestampMs) || Number.isNaN(date.getTime())) return "";
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "full",
    timeStyle: "short",
  }).format(date);
}

/**
 * Milliseconds until a compact message timestamp can change. ChatView takes
 * the minimum across its messages, keeping one precise timer per visible chat
 * instead of one interval per transcript row.
 */
export function messageTimestampRefreshIn(timestampMs: number, nowMs: number): number | null {
  if (!Number.isFinite(timestampMs)) return null;
  // ChatView's clock advances only when a timestamp label is due to change. A
  // newly appended message can therefore be newer than that cached value;
  // cap this first wake-up to one real minute instead of extending "now" by
  // however long the view clock has been idle.
  if (timestampMs > nowMs) return MINUTE_MS + 25;
  const ageMs = Math.max(0, nowMs - timestampMs);
  let refreshAt: number;
  if (ageMs < MINUTE_MS) {
    refreshAt = timestampMs + MINUTE_MS;
  } else if (ageMs < HOUR_MS) {
    refreshAt = timestampMs + (Math.floor(ageMs / MINUTE_MS) + 1) * MINUTE_MS;
  } else if (ageMs < 2 * HOUR_MS) {
    refreshAt = timestampMs + 2 * HOUR_MS;
  } else {
    const now = new Date(nowMs);
    refreshAt = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1).getTime();
  }
  // Avoid a tight loop if a timer fires a fraction early at the boundary.
  return Math.max(1_000, refreshAt - nowMs + 25);
}
