import { writable } from "svelte/store";

/**
 * Shared reconnect scaffolding for the per-session sockets (`ws.ts`'s
 * SessionSocket and `chat/chatWs.ts`'s ChatSocket). Both reconnect forever on
 * unclean closes (the close-the-laptop path); this owns the pieces that must
 * behave identically across the two — the backoff curve, the global
 * reconnecting-indicator accounting, and the unknown-session retry ceiling —
 * so the two drivers can't drift.
 */

/**
 * Number of session sockets currently trying to reconnect. The daemon dot in
 * the rail pulses while this is non-zero. Each socket contributes at most one
 * to the count while it is down (see Reconnector).
 */
export const reconnectingSockets = writable(0);

const INITIAL_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 10_000;

/**
 * "unknown session" is retried this many times before it is fatal: during a
 * chat⇄terminal view switch the id briefly has no attachable process, and the
 * pane's socket must ride that out instead of dying on the first probe.
 */
export const UNKNOWN_SESSION_RETRIES = 12;

/**
 * Backoff state machine + reconnecting-indicator accounting for one socket.
 * The owning socket drives its own connect()/close() and supplies the retry
 * callback; this only owns the timing and this socket's single contribution to
 * `reconnectingSockets`. Exponential backoff doubles from INITIAL to MAX.
 */
export class Reconnector {
  private reconnecting = false;
  private backoffMs = INITIAL_BACKOFF_MS;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly onRetry: () => void) {}

  /**
   * Schedule the next reconnect attempt: mark this socket reconnecting (once)
   * so the indicator counts it, arm the retry at the current backoff, then
   * double the backoff (capped at MAX) for the attempt after.
   */
  schedule(): void {
    if (!this.reconnecting) {
      this.reconnecting = true;
      reconnectingSockets.update((n) => n + 1);
    }
    this.timer = setTimeout(() => {
      this.timer = null;
      this.onRetry();
    }, this.backoffMs);
    this.backoffMs = Math.min(this.backoffMs * 2, MAX_BACKOFF_MS);
  }

  /** A successful (re)connect: reset the backoff and clear the indicator. */
  succeeded(): void {
    this.backoffMs = INITIAL_BACKOFF_MS;
    this.clear();
  }

  /** Drop this socket's contribution to the reconnecting indicator. */
  clear(): void {
    if (this.reconnecting) {
      this.reconnecting = false;
      reconnectingSockets.update((n) => Math.max(0, n - 1));
    }
  }

  /** Cancel any pending retry timer (a permanent close). */
  cancel(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }
}
