/** Protect a new tab from a snapshot fetched before its create request. The
 * grace expires independently of roster changes: fast exits can otherwise
 * leave a permanent “closing…” tab when no further events arrive. */
export class RecentSessions {
  private entries = new Map<string, number>();
  private timer: ReturnType<typeof setTimeout> | undefined;
  constructor(private expired: () => void, private graceMs = 10_000) {}

  add(id: string): void {
    this.entries.set(id, Date.now() + this.graceMs);
    this.schedule();
  }

  delete(id: string): void {
    this.entries.delete(id);
    this.schedule();
  }

  protect(live: Set<string>): void {
    for (const [id, until] of this.entries) {
      if (until <= Date.now()) this.entries.delete(id);
      else live.add(id);
    }
  }

  dispose(): void {
    clearTimeout(this.timer);
    this.entries.clear();
  }

  private schedule(): void {
    clearTimeout(this.timer);
    if (this.entries.size === 0) return;
    const next = Math.min(...this.entries.values());
    this.timer = setTimeout(() => {
      this.protect(new Set());
      this.expired();
      this.schedule();
    }, Math.max(0, next - Date.now()));
  }
}
