import { writable } from "svelte/store";

/**
 * Shared reconnect scaffolding for the per-session sockets (`ws.ts`'s
 * SessionSocket and `chat/chatWs.ts`'s ChatSocket). Both reconnect forever on
 * unclean closes (the close-the-laptop path); this owns the pieces that must
 * behave identically across the two — the backoff curve, the global
 * reconnecting-indicator accounting, and the unknown-session retry ceiling —
 * so the two drivers can't drift.
 *
 * Battery discipline (rules/web-ui.md): a window can hold ~20 of these
 * sockets, and an unreachable daemon (a resumed laptop with a dead tunnel)
 * must not turn into an indefinite ~150-attempts/min storm. So the delay is
 * jittered (±20%, no thundering herd on one shared cause), a hidden document
 * takes a slow tier (at most ~one attempt per minute per socket), and two
 * "retry now" nudges keep the slow tier from ever delaying recovery: the
 * events socket's own recovery (the daemon is demonstrably back), and the
 * document returning to visible.
 */

/**
 * Number of session sockets currently trying to reconnect. The daemon dot in
 * the rail pulses while this is non-zero. Each socket contributes at most one
 * to the count while it is down (see Reconnector).
 */
export const reconnectingSockets = writable(0);

const INITIAL_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 10_000;
/** ± fraction of jitter applied to every retry delay. */
const JITTER = 0.2;
/** Hidden-document slow tier: the retry delay is floored at this. */
export const HIDDEN_RETRY_MS = 60_000;

/**
 * "unknown session" is retried this many times before it is fatal: during a
 * chat⇄terminal view switch the id briefly has no attachable process, and the
 * pane's socket must ride that out instead of dying on the first probe.
 */
export const UNKNOWN_SESSION_RETRIES = 12;

/**
 * The actual delay for one retry: the backoff, floored at the slow tier while
 * the document is hidden, then jittered ±JITTER so many sockets downed by one
 * cause don't probe in lockstep. Pure — `rand` is injectable for tests.
 */
export function retryDelayMs(
  backoffMs: number,
  hidden: boolean,
  rand: () => number = Math.random,
): number {
  const base = hidden ? Math.max(backoffMs, HIDDEN_RETRY_MS) : backoffMs;
  return Math.round(base * (1 - JITTER + 2 * JITTER * rand()));
}

function documentHidden(): boolean {
  return typeof document !== "undefined" && document.visibilityState === "hidden";
}

/** Every Reconnector with a retry timer armed right now (nudge targets). */
const waiting = new Set<Reconnector>();

/**
 * Fire every pending retry immediately. Called when the events socket comes
 * (back) up — the daemon is demonstrably reachable, so no session socket
 * should sit out the rest of a backoff — and on visibility return, so a
 * hidden-tier delay never outlives the user looking at the window.
 */
export function retryWaitingNow(): void {
  for (const r of [...waiting]) r.retryNow();
}

/**
 * One document-lifetime listener, armed lazily on the first schedule():
 * module-scoped (not component-scoped) on purpose — the sockets it serves
 * outlive any component, and tests run without a `document` at all.
 */
let visibilityArmed = false;
function armVisibilityRetry(): void {
  if (visibilityArmed || typeof document === "undefined") return;
  visibilityArmed = true;
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") retryWaitingNow();
  });
}

/**
 * Backoff state machine + reconnecting-indicator accounting for one socket.
 * The owning socket drives its own connect()/close() and supplies the retry
 * callback; this only owns the timing and this socket's single contribution to
 * `reconnectingSockets`. Exponential backoff doubles from INITIAL to MAX;
 * the delay actually slept is `retryDelayMs` (jitter + hidden slow tier).
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
    armVisibilityRetry();
    if (!this.reconnecting) {
      this.reconnecting = true;
      reconnectingSockets.update((n) => n + 1);
    }
    waiting.add(this);
    this.timer = setTimeout(() => {
      this.timer = null;
      waiting.delete(this);
      this.onRetry();
    }, retryDelayMs(this.backoffMs, documentHidden()));
    this.backoffMs = Math.min(this.backoffMs * 2, MAX_BACKOFF_MS);
  }

  /** Fire the pending retry immediately (a retryWaitingNow nudge). No-op
   *  when nothing is armed — never double-fires a retry. */
  retryNow(): void {
    if (this.timer === null) return;
    clearTimeout(this.timer);
    this.timer = null;
    waiting.delete(this);
    this.onRetry();
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
    waiting.delete(this);
  }
}
