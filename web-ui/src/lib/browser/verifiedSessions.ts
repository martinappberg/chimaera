/**
 * Proxy sessions this window verified, by target — only the browser pane
 * (loaded on demand) imports this, so it stays out of the entry bundle.
 */

import type { ProxySession } from "./proxy";

/** How long a session this window minted and health-checked counts as
 *  good enough to show its page at once (a pane remounted by a move, a
 *  reopen) while the check runs again behind it. */
export const VERIFIED_FRESH_MS = 5 * 60_000;

/**
 * Proxy sessions this window verified, by target. A remounted browser pane
 * would otherwise sit on "connecting" for a mint and a health check (two
 * round trips, each a tunnel's latency on a remote) before showing a page
 * that never went anywhere. Bounded by the number of targets in a window.
 */
export class VerifiedSessions {
  private readonly byTarget = new Map<string, { session: ProxySession; at: number }>();

  constructor(
    private readonly freshMs = VERIFIED_FRESH_MS,
    private readonly now: () => number = () => Date.now(),
  ) {}

  private static key(host: string, port: number): string {
    return `${host.toLowerCase()}:${port}`;
  }

  remember(host: string, port: number, session: ProxySession): void {
    this.byTarget.set(VerifiedSessions.key(host, port), { session, at: this.now() });
  }

  forget(host: string, port: number): void {
    this.byTarget.delete(VerifiedSessions.key(host, port));
  }

  /** A session verified within `freshMs`, else null (a stale one is dropped). */
  recent(host: string, port: number): ProxySession | null {
    const key = VerifiedSessions.key(host, port);
    const hit = this.byTarget.get(key);
    if (hit === undefined) return null;
    if (this.now() - hit.at > this.freshMs) {
      this.byTarget.delete(key);
      return null;
    }
    return hit.session;
  }
}

/** This window's verified sessions. */
export const verifiedSessions = new VerifiedSessions();
