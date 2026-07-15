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
