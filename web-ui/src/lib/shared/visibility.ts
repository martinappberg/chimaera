import { readable } from "svelte/store";

/**
 * Document visibility as a store: `true` while this window's document is
 * visible. The invariant (rules/web-ui.md): recurring work — 1 Hz tickers,
 * polls, presence animations — must be gated on visibility so a hidden
 * window costs ~nothing on battery. An `$effect` gated on `$pageVisible`
 * gets the catch-up for free: it re-runs on visibility return, so its first
 * statement can refresh whatever went stale while hidden (the
 * `workspace/compute.ts` idiom, without every component hand-rolling the
 * `visibilitychange` listener). The readable's start/stop notifier means the
 * one shared listener exists only while somebody subscribes.
 */
export const pageVisible = readable(
  typeof document === "undefined" || document.visibilityState === "visible",
  (set) => {
    if (typeof document === "undefined") return;
    // Re-read on (re)subscribe: the state may have moved while unobserved.
    set(document.visibilityState === "visible");
    const on = (): void => set(document.visibilityState === "visible");
    document.addEventListener("visibilitychange", on);
    return () => document.removeEventListener("visibilitychange", on);
  },
);
