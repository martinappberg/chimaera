<script lang="ts">
  import {
    formatSlurmDuration,
    parseSlurmTimeLeft,
    type ComputeSelf,
  } from "./compute";

  interface Props {
    /** The daemon's own allocation (present in every compute-node window). */
    self: ComputeSelf;
    /** CLIENT clock when the snapshot carrying `self` was received. */
    receivedAt: number;
  }

  let { self: alloc, receivedAt }: Props = $props();

  // The fetched time_left only moves per poll (60s) — tick locally against
  // the receipt time so the countdown reads live, and re-sync on every fetch
  // (a new `receivedAt` resets the baseline).
  let now = $state(Date.now());
  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });

  /** Seconds left at the last fetch; null = not a duration (UNLIMITED, …). */
  const baseline = $derived(parseSlurmTimeLeft(alloc.time_left));
  const remaining = $derived(
    baseline === null
      ? null
      : Math.max(0, baseline - Math.floor((now - receivedAt) / 1000)),
  );
  const countdown = $derived(
    remaining === null
      ? // Slurm's %L emits INVALID/NOT_SET while a job transitions —
        // placeholder words, not durations, so show a dash. UNLIMITED (and
        // any other raw value) stays: Slurm vocabulary is never relabeled.
        alloc.time_left === "INVALID" || alloc.time_left === "NOT_SET"
        ? "—"
        : alloc.time_left
      : remaining === 0
        ? "expiring…"
        : formatSlurmDuration(remaining),
  );

  /** "4 cpu · 16G · gpu:1" — omit whatever the wire left empty. */
  const resources = $derived.by(() => {
    const parts: string[] = [];
    if (alloc.cpus !== "") parts.push(`${alloc.cpus} cpu`);
    if (alloc.mem !== "") parts.push(alloc.mem);
    if (alloc.gres !== "") parts.push(alloc.gres);
    return parts.join(" · ");
  });
</script>

<!-- The "you are inside a job" strip: this window's whole workbench lives in
     a Slurm allocation and ends at walltime — worn honestly, its own row
     above the daemon bar so neither crowds the other. Shown for ANY self
     state (a COMPLETING allocation is still the truth of this window). -->
<div
  class="compute-strip"
  role="status"
  title={`slurm job ${alloc.job_id} on ${alloc.node} — expires at walltime`}
>
  <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
    <rect x="2" y="2" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
    <rect x="9" y="2" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
    <rect x="2" y="9" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
    <rect x="9" y="9" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
  </svg>
  <span class="cs-time" class:expiring={remaining === 0}>{countdown}</span>
  {#if resources !== ""}
    <span class="cs-res">· {resources}</span>
  {/if}
  {#if alloc.state !== "" && alloc.state !== "RUNNING"}
    <span class="cs-state">{alloc.state}</span>
  {/if}
</div>

<style>
  /* Token-only, subtly tinted so the rail's bottom says "compute job" at a
     glance in both themes — accent wash + hairline top rule, muted text. */
  .compute-strip {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.35rem;
    min-width: 0;
    margin-top: 10px;
    padding: 4px 16px 4px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    background: color-mix(in srgb, var(--accent) 7%, transparent);
    border-top: 1px solid color-mix(in srgb, var(--accent) 30%, var(--edge));
  }

  .compute-strip svg {
    flex: none;
    color: var(--accent);
    opacity: 0.8;
  }

  .cs-time {
    flex: none;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }

  .cs-time.expiring {
    color: var(--warn);
  }

  .cs-res {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* A non-RUNNING state (COMPLETING, …) is worth the caution tone. */
  .cs-state {
    flex: none;
    margin-left: auto;
    color: var(--warn);
  }
</style>
