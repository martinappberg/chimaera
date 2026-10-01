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
 * takes a slow tier — but only from the third consecutive failure, so a
 * transient blip still recovers in seconds — and two "retry now" nudges keep
 * the slow tier from ever delaying recovery: the events socket's own recovery
 * (the daemon is demonstrably back), and the document returning to visible.
 * The nudge itself is deferred, spread, and damped (see nudgeReconnectors) so
 * a crash-looping daemon can't weaponize it into a synchronized herd.
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
/** Hidden-document slow tier: the retry delay is floored at this (nominal —
 *  48–72s after jitter) once the grace attempts are spent. */
export const HIDDEN_RETRY_MS = 60_000;
/** Retries that keep the FAST backoff even while hidden. A transient drop
 *  (daemon restart, tunnel blip) must not cost a hidden window a minute —
 *  the floor is for the persistently-unreachable case only. */
export const HIDDEN_GRACE_ATTEMPTS = 2;
/** A nudge herd fires at most once per this window (flap damping). */
export const NUDGE_DAMP_MS = 10_000;
/** Each nudged socket retries at a random point inside this window, so one
 *  shared cause doesn't produce a synchronized connect herd. */
export const NUDGE_SPREAD_MS = 2_000;

/**
 * "unknown session" is retried this many times before it is fatal: during a
 * chat⇄terminal view switch the id briefly has no attachable process, and the
 * pane's socket must ride that out instead of dying on the first probe.
 */
export const UNKNOWN_SESSION_RETRIES = 12;

/**
 * A chat or terminal socket that authenticated and has heard nothing for this
 * long is open and kept, not reconnecting. A browser cannot read the mark a
 * keeper puts on a socket it keeps (`X-Chimaera-Sockets: kept`), so this
 * silence is how the UI knows: a keeper that keeps a sleeping cloud machine's
 * sockets (directly, or behind this computer's relay) takes what is sent and
 * delivers it once the machine answers, and says nothing until then. A
 * connection that closes sooner was refused. Shorter than the 2 s grace
 * before a view says "Reconnecting…", so a kept socket never shows it.
 */
export const QUIET_OPEN_MS = 1500;

/**
 * The actual delay for one retry: the backoff, floored at the slow tier while
 * the document is hidden — but only once `attempt` (1-based count of
 * consecutive failures) has spent the grace attempts — then jittered ±JITTER
 * so many sockets downed by one cause don't probe in lockstep. Pure — `rand`
 * is injectable for tests.
 */
export function retryDelayMs(
  backoffMs: number,
  hidden: boolean,
  attempt: number,
  rand: () => number = Math.random,
): number {
  const base =
    hidden && attempt > HIDDEN_GRACE_ATTEMPTS
      ? Math.max(backoffMs, HIDDEN_RETRY_MS)
      : backoffMs;
  return Math.round(base * (1 - JITTER + 2 * JITTER * rand()));
}

function documentHidden(): boolean {
  return typeof document !== "undefined" && document.visibilityState === "hidden";
}

/** Every Reconnector currently down (retry pending OR attempt in flight) —
 *  the nudge targets. Entered when the indicator flips on, left in clear(). */
const down = new Set<Reconnector>();

let lastNudgeAt = 0;

/**
 * Nudge every down socket to retry soon. Called when the events socket comes
 * (back) up — the daemon is demonstrably reachable, so no session socket
 * should sit out the rest of a backoff — and on visibility return, so a
 * hidden-tier delay never outlives the user looking at the window. Shaped to
 * never amplify: each socket re-arms on its own timer at a random 0–2s slot
 * (deferred out of the caller's stack — the events message handler must
 * finish applying its snapshot first, and one socket's failure can't touch
 * the others), and the whole herd is damped to once per NUDGE_DAMP_MS so a
 * crash-looping daemon (accept → one frame → die) can't bypass the backoff.
 */
export function nudgeReconnectors(rand: () => number = Math.random): void {
  const now = Date.now();
  if (now - lastNudgeAt < NUDGE_DAMP_MS) return;
  lastNudgeAt = now;
  for (const r of [...down]) r.nudge(Math.round(rand() * NUDGE_SPREAD_MS));
}

/** Sockets parked because their project's owner (a cloud machine) is
 *  asleep. No timer runs for them: retrying a sleeping owner on a backoff
 *  only churns (each attempt is answered "asleep" again). A user action dials
 *  its own socket with wake intent; {@link ownerAwake} dials them all. */
const parked = new Set<() => void>();

/** Park one socket until its owner answers again; returns the unpark (call
 *  it when the socket dials, or closes for good). */
export function parkUntilAwake(retry: () => void): () => void {
  parked.add(retry);
  return () => {
    parked.delete(retry);
  };
}

/** Re-read a surface (the file tree, the Timeline) once its project answers
 *  again, after a read of it met a project state (its owner asleep,
 *  reconnecting, unreachable). It parks like a socket — no timer, no backoff
 *  churn against a sleeping owner — and, since a re-read costs a request,
 *  waits for the document to be visible: a hidden tab reads once when it is
 *  next seen. `pollMs` adds one slow recheck for a state no sign announces the
 *  end of (a project routed elsewhere that has come home: nothing on this
 *  computer says so). `refetch` that meets the state again calls this again.
 *  Returns the cancel (call it when the surface goes away or reads some other
 *  way first). */
export function refetchWhenOwnerAwake(refetch: () => void, { pollMs }: { pollMs?: number } = {}): () => void {
  let done = false;
  let unpark: () => void = () => {};
  let timer: ReturnType<typeof setTimeout> | null = null;
  let onVisible: (() => void) | null = null;
  const cancel = (): void => {
    done = true;
    unpark();
    if (timer !== null) clearTimeout(timer);
    timer = null;
    if (onVisible !== null) document.removeEventListener("visibilitychange", onVisible);
    onVisible = null;
  };
  const fire = (): void => {
    if (done) return;
    if (documentHidden()) {
      if (onVisible === null) {
        onVisible = () => {
          if (!documentHidden()) fire();
        };
        document.addEventListener("visibilitychange", onVisible);
      }
      return;
    }
    cancel();
    refetch();
  };
  unpark = parkUntilAwake(fire);
  if (pollMs !== undefined) {
    timer = setTimeout(() => {
      timer = null;
      fire();
    }, pollMs);
  }
  return cancel;
}

/** A sign the owner answers again (a placement read says owned, a project
 *  view's events socket is up, a row became reachable): dial every parked
 *  socket once, passively, each on its own 0–2s slot out of the caller's
 *  stack. One that finds the owner still asleep parks again. */
export function ownerAwake(rand: () => number = Math.random): void {
  for (const retry of [...parked]) {
    parked.delete(retry);
    setTimeout(retry, Math.round(rand() * NUDGE_SPREAD_MS));
  }
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
    if (document.visibilityState === "visible") nudgeReconnectors();
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
  /** Consecutive failures since the last success (grace-attempt counter). */
  private attempts = 0;
  /** A nudge landed while an attempt was in flight: that attempt's failure
   *  retries immediately instead of scheduling a fresh (possibly floored)
   *  delay — one unlucky in-flight socket must not lag its nudged siblings. */
  private nudged = false;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly onRetry: () => void) {}

  /**
   * Schedule the next reconnect attempt: mark this socket reconnecting (once)
   * so the indicator counts it, arm the retry at the current backoff (or NOW
   * if a nudge arrived mid-attempt), then double the backoff (capped at MAX)
   * for the attempt after.
   */
  schedule(): void {
    armVisibilityRetry();
    if (!this.reconnecting) {
      this.reconnecting = true;
      down.add(this);
      reconnectingSockets.update((n) => n + 1);
    }
    this.attempts += 1;
    const delay = this.nudged
      ? 0 // still deferred: schedule() runs inside socket close handlers
      : retryDelayMs(this.backoffMs, documentHidden(), this.attempts);
    this.nudged = false;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.onRetry();
    }, delay);
    this.backoffMs = Math.min(this.backoffMs * 2, MAX_BACKOFF_MS);
  }

  /**
   * A nudge (see nudgeReconnectors): with a retry pending, pull it in to
   * `delayMs` (the caller's spread slot); with an attempt in flight, mark it
   * so the failure path retries immediately. No-op on a healthy socket.
   */
  nudge(delayMs: number): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = setTimeout(() => {
        this.timer = null;
        this.onRetry();
      }, delayMs);
    } else if (this.reconnecting) {
      this.nudged = true;
    }
  }

  /** A successful (re)connect: reset the backoff and clear the indicator. */
  succeeded(): void {
    this.backoffMs = INITIAL_BACKOFF_MS;
    this.attempts = 0;
    this.nudged = false;
    this.clear();
  }

  /** Drop this socket's contribution to the reconnecting indicator. */
  clear(): void {
    if (this.reconnecting) {
      this.reconnecting = false;
      down.delete(this);
      reconnectingSockets.update((n) => Math.max(0, n - 1));
    }
  }

  /** Cancel any pending retry timer (permanent close, or an owner-driven
   *  resync that is about to connect() itself — ws.ts resync()). Also
   *  forgets a mid-flight nudge: the owner is taking over the attempt. */
  cancel(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.nudged = false;
  }
}
