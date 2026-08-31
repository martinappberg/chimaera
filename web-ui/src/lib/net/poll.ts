/**
 * Visibility-aware polling cadence — the shared shape for the HTTP pollers
 * (`api.ts`'s pollHealth, `workspace/sessions.ts`'s pollSessions). The
 * invariant (rules/web-ui.md): recurring work must be gated on document
 * visibility or an equivalent liveness signal — a hidden window on battery
 * must cost ~nothing, and a 5s poll per window forever is the opposite.
 *
 * Two tiers: FAST is the recovery probe for a visible window whose events
 * socket is down; SLOW is the safety-net cadence — enough to notice a daemon
 * build change or re-trip the 401 overlay, cheap enough to run forever.
 */

export const POLL_FAST_MS = 5_000;
export const POLL_SLOW_MS = 60_000;

/**
 * /health cadence. While /ws/events is up the socket IS the liveness signal
 * and health is only a safety net (hostname/build freshness, the 401 trip) —
 * the slow tier. While events is down, health is the recovery probe — the
 * fast tier, but only in a visible window: hidden windows take the slow tier
 * and rely on startVisibilityPoll's catch-up fetch on visibility return.
 */
export function healthPollDelayMs(eventsUp: boolean, hidden: boolean): number {
  return eventsUp || hidden ? POLL_SLOW_MS : POLL_FAST_MS;
}

/**
 * /sessions fallback cadence. Only armed at all while /ws/events is down
 * (App gates it), so this is purely the recovery probe: fast while someone
 * is looking, slow while hidden (catch-up on visibility return).
 */
export function sessionsPollDelayMs(hidden: boolean): number {
  return hidden ? POLL_SLOW_MS : POLL_FAST_MS;
}

/** A kick() within this window of the last tick is a no-op — an events-flap
 *  transition per second must not become a fetch per second. */
export const KICK_DAMP_MS = 10_000;

export interface PollHandle {
  /** Tear the poller down: cancel the timer, unhook the listener. */
  stop(): void;
  /**
   * Ask for a prompt out-of-cadence tick (e.g. the events socket flipped —
   * reachability deserves a fresh probe). Deferred a beat so callers inside
   * event handlers finish first, and damped: a no-op within KICK_DAMP_MS of
   * the last tick, so flap storms collapse to one probe per window.
   */
  kick(): void;
}

/**
 * Run `tick` immediately, then forever at `delayMs(hidden)` — re-evaluated
 * at every arm, so a tier change (events recovery, tab hidden) takes effect
 * at the next schedule — with an immediate catch-up tick when the document
 * returns to visible (a slow-tier delay armed while hidden must never make
 * a fresh look at the world wait out its remainder). A `tick` rejection is
 * caught here (the pollers also report errors through their own callbacks)
 * and still re-arms the next tick. The tick itself should carry a timeout
 * on any fetch it makes: the chain arms the NEXT tick only after this one
 * settles, so an unbounded hang (a dead tunnel's ~75s TCP connect) would
 * otherwise stretch the cadence to hang+delay.
 */
export function startVisibilityPoll(
  tick: () => void | Promise<void>,
  delayMs: (hidden: boolean) => number,
): PollHandle {
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let lastTickAt = 0;

  const hidden = (): boolean =>
    typeof document !== "undefined" && document.visibilityState === "hidden";

  const run = async (): Promise<void> => {
    lastTickAt = Date.now();
    try {
      await tick();
    } catch {
      // The tick's own error path already reported it; the cadence is ours.
    } finally {
      // `timer === null` guards the overlap: a catch-up run racing an
      // in-flight scheduled run must still arm exactly one next tick.
      if (!stopped && timer === null) {
        timer = setTimeout(() => {
          timer = null;
          void run();
        }, delayMs(hidden()));
      }
    }
  };

  const runNow = (): void => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    void run();
  };

  const onVisibility = (): void => {
    if (stopped || document.visibilityState !== "visible") return;
    // Catch up NOW instead of waiting out a delay armed under the slow tier.
    runNow();
  };

  if (typeof document !== "undefined") {
    document.addEventListener("visibilitychange", onVisibility);
  }
  void run();

  return {
    stop(): void {
      stopped = true;
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      if (typeof document !== "undefined") {
        document.removeEventListener("visibilitychange", onVisibility);
      }
    },
    kick(): void {
      if (stopped || Date.now() - lastTickAt < KICK_DAMP_MS) return;
      setTimeout(() => {
        if (!stopped) runNow();
      }, 0);
    },
  };
}
