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

  // Same live-tick discipline as ComputeStrip: the fetched time_left only
  // moves per poll, so tick locally against the receipt time and re-sync on
  // every fetch.
  let now = $state(Date.now());
  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });

  const baseline = $derived(parseSlurmTimeLeft(alloc.time_left));
  const remaining = $derived(
    baseline === null
      ? null
      : Math.max(0, baseline - Math.floor((now - receivedAt) / 1000)),
  );
  const countdown = $derived(
    remaining === null
      ? alloc.time_left
      : remaining === 0
        ? "expiring…"
        : formatSlurmDuration(remaining),
  );
  /** Under ten minutes the countdown turns cautionary. */
  const closing = $derived(remaining !== null && remaining < 600);

  /** "4 cpu · 16G · gpu:1" — omit whatever the wire left empty. */
  const resources = $derived.by(() => {
    const parts: string[] = [];
    if (alloc.cpus !== "") parts.push(`${alloc.cpus} cpu`);
    if (alloc.mem !== "") parts.push(alloc.mem);
    if (alloc.gres !== "") parts.push(alloc.gres);
    return parts.join(" · ");
  });
</script>

<!-- The compute window's home-page identity card: everything the user asked
     "am I on a compute node, with what, for how long?" answers at a glance —
     node, partition, job id, resources, and a live walltime countdown. The
     rail's ComputeStrip carries the same truth inside a workspace; this is
     the front door's version. -->
<div
  class="banner"
  role="status"
  title={`slurm job ${alloc.job_id} on ${alloc.node} — this whole workbench lives inside the allocation and ends at walltime`}
>
  <div class="left">
    <div class="label">
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <rect x="2" y="2" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
        <rect x="9" y="2" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
        <rect x="2" y="9" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
        <rect x="9" y="9" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
      </svg>
      <span>compute session</span>
      {#if alloc.state !== "" && alloc.state !== "RUNNING"}
        <span class="state">{alloc.state}</span>
      {/if}
    </div>
    <div class="where">
      <span class="node">{alloc.node}</span>
      {#if alloc.partition !== ""}
        <span class="sep">·</span>
        <span>{alloc.partition} partition</span>
      {/if}
      <span class="sep">·</span>
      <span>job {alloc.job_id}</span>
    </div>
    {#if resources !== ""}
      <div class="res">{resources}</div>
    {/if}
  </div>
  <div class="right">
    <div class="countdown" class:closing>{countdown}</div>
    <div class="sub">
      {remaining === null ? "walltime" : "walltime remaining"}
    </div>
  </div>
</div>

<style>
  /* Token-only accent wash — unmistakably "inside a job" in both themes,
     without shouting over the workspace list below it. */
  .banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    margin: 0 0 18px;
    padding: 13px 16px;
    border: 1px solid color-mix(in srgb, var(--accent) 32%, var(--edge));
    border-radius: 9px;
    background: color-mix(in srgb, var(--accent) 7%, transparent);
  }

  .left {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }

  .label {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--accent);
  }

  .label svg {
    flex: none;
  }

  /* A non-RUNNING allocation state (COMPLETING, …) is worth caution. */
  .label .state {
    font-family: var(--mono);
    letter-spacing: normal;
    text-transform: none;
    color: var(--warn);
  }

  .where {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    font-family: var(--mono);
    font-size: 12.5px;
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .where .node {
    color: var(--fg);
    font-weight: 600;
  }

  .where .sep {
    opacity: 0.5;
  }

  .res {
    font-family: var(--mono);
    font-size: 12px;
    color: var(--muted);
  }

  .right {
    flex: none;
    text-align: right;
  }

  .countdown {
    font-family: var(--mono);
    font-size: 21px;
    font-weight: 600;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
    line-height: 1.15;
  }

  .countdown.closing {
    color: var(--warn);
  }

  .sub {
    font-size: 11px;
    color: var(--muted);
  }
</style>
