import { get } from "svelte/store";
import { pageVisible } from "../shared/visibility";

/**
 * A 1 Hz wall clock for a live activity line's elapsed, ticking only while
 * `active()` holds AND the document is visible (rules/web-ui.md: recurring
 * work is gated on visibility). Re-reads the time whenever it starts again,
 * so a line returning from a hidden tab is current on its first paint. Call
 * during component initialisation (it owns an `$effect`).
 */
export function liveClock(active: () => boolean): { readonly ms: number } {
  let ms = $state(Date.now());
  let shown = $state(get(pageVisible));
  $effect(() => pageVisible.subscribe((v) => (shown = v)));
  $effect(() => {
    if (!active() || !shown) return;
    ms = Date.now();
    const iv = setInterval(() => (ms = Date.now()), 1000);
    return () => clearInterval(iv);
  });
  return {
    get ms() {
      return ms;
    },
  };
}
