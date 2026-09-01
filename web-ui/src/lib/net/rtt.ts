/**
 * Rolling estimate of the daemon link's round-trip time, fed by the /health
 * poll's fetch timing. On a tunneled remote this is what the user actually
 * feels: every keystroke echo costs one RTT, every cold HTTP fetch about two
 * (the ssh mux channel-open pays a full RTT before the request starts —
 * measured against a real login node, docs/perf-remote-plan.md).
 *
 * The estimate is the MINIMUM over a small rolling window: a keepalive-warm
 * health fetch costs ~1×RTT while a cold one costs ~2×RTT, so the window
 * minimum tracks the true floor and outliers (cold connections, a busy
 * reactor tick) fall away. Samples only come while /health succeeds — a dead
 * link reports the last known estimate, which the UI treats as stale the
 * same way it treats the health state itself.
 */

import { writable, type Readable } from "svelte/store";

const WINDOW = 8;

const samples: number[] = [];
const store = writable<number | null>(null);
let current: number | null = null;

/** Record one successful /health fetch's wall time. */
export function recordLinkRtt(ms: number): void {
  samples.push(ms);
  if (samples.length > WINDOW) samples.shift();
  current = Math.round(Math.min(...samples));
  store.set(current);
}

/** Reactive RTT estimate in ms; null until the first health fetch lands. */
export const linkRtt: Readable<number | null> = { subscribe: store.subscribe };

/** Non-reactive read for hot paths (the per-keystroke local-echo gate). */
export function linkRttNow(): number | null {
  return current;
}

/** Test/reconnect hook: drop the window (a re-homed daemon is a new link). */
export function resetLinkRtt(): void {
  samples.length = 0;
  current = null;
  store.set(null);
}
