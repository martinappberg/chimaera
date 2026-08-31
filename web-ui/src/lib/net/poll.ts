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

/**
 * Run `tick` immediately, then forever at `delayMs(hidden)` — re-evaluated
 * at every arm, so a tier change (events recovery, tab hidden) takes effect
 * at the next schedule — with an immediate catch-up tick when the document
 * returns to visible (a slow-tier delay armed while hidden must never make
 * a fresh look at the world wait out its remainder). Returns the stop
 * function. `tick` should not throw (both pollers catch internally); a
 * rejection still re-arms.
 */
export function startVisibilityPoll(
  tick: () => void | Promise<void>,
  delayMs: (hidden: boolean) => number,
): () => void {
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const hidden = (): boolean =>
    typeof document !== "undefined" && document.visibilityState === "hidden";

  const run = async (): Promise<void> => {
    try {
      await tick();
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

  const onVisibility = (): void => {
    if (stopped || document.visibilityState !== "visible") return;
    // Catch up NOW instead of waiting out a delay armed under the slow tier.
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    void run();
  };

  if (typeof document !== "undefined") {
    document.addEventListener("visibilitychange", onVisibility);
  }
  void run();

  return () => {
    stopped = true;
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    if (typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", onVisibility);
    }
  };
}
