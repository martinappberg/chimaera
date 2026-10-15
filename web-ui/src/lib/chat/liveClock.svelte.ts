import { fromStore } from "svelte/store";
import { pageVisible } from "../shared/visibility";

/**
 * One shared 1 Hz wall clock for every live activity line's elapsed. A
 * reader registers only while it is live, its view visible and the document
 * shown (rules/web-ui.md: recurring work is gated on visibility); one
 * interval runs while any reader is registered and none when nobody is.
 * Registering re-reads the time, so a line returning from a hidden tab is
 * current on its first paint.
 */
let now = $state(Date.now());
let readers = 0;
let interval: ReturnType<typeof setInterval> | null = null;
const shown = fromStore(pageVisible);

function register(): () => void {
  now = Date.now();
  readers += 1;
  if (interval === null) interval = setInterval(() => (now = Date.now()), 1000);
  return () => {
    readers -= 1;
    if (readers === 0 && interval !== null) {
      clearInterval(interval);
      interval = null;
    }
  };
}

/** The shared clock, ticking for this reader while `active()` holds and the
 *  document is shown. Call during component initialisation (it owns one
 *  `$effect`); read `.ms` only while active, so idle lines never re-render. */
export function liveClock(active: () => boolean): { readonly ms: number } {
  $effect(() => {
    if (!active() || !shown.current) return;
    return register();
  });
  return {
    get ms() {
      return now;
    },
  };
}
