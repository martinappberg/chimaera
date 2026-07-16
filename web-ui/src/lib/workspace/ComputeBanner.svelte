<script lang="ts">
  import {
    formatSlurmDuration,
    parseSlurmTimeLeft,
    type ComputeSelf,
  } from "./compute";
  import { cancelComputeSession } from "./computeSessions";
  import { closeThisWindow, isNativeShell } from "../net/native";

  interface Props {
    /** The daemon's own allocation (present in every compute-node window). */
    self: ComputeSelf;
    /** CLIENT clock when the snapshot carrying `self` was received. */
    receivedAt: number;
  }

  let { self: alloc, receivedAt }: Props = $props();

  /** The end-job flow: idle → confirm (inline, Escape disarms) → cancelling
   *  (DELETE in flight) → ended (this window's daemon is going down). */
  let phase = $state<"idle" | "confirm" | "cancelling" | "ended">("idle");
  let keepBtn = $state<HTMLButtonElement | null>(null);

  const native = isNativeShell();

  // Same live-tick discipline as ComputeStrip: the fetched time_left only
  // moves per poll, so tick locally against the receipt time and re-sync on
  // every fetch. Once ended there's nothing left to count down.
  let now = $state(Date.now());
  $effect(() => {
    if (phase === "ended") return;
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });

  // Focus the SAFE option on arm, so a stray Enter can't end the job.
  $effect(() => {
    if (phase === "confirm") keepBtn?.focus();
  });

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

  const bannerTitle = $derived(
    phase === "ended"
      ? `slurm job ${alloc.job_id} cancelled — the allocation and this window's daemon are ending`
      : `slurm job ${alloc.job_id} on ${alloc.node} — this whole workbench lives inside the allocation and ends at walltime`,
  );

  async function endJob(): Promise<void> {
    if (phase !== "confirm") return;
    phase = "cancelling";
    try {
      await cancelComputeSession(alloc.job_id);
    } catch {
      // EXPECTED: scancel kills this window's own daemon, so the DELETE
      // often dies mid-flight — that failure IS the success signal here.
    }
    phase = "ended";
  }

  function onWindowKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape" && phase === "confirm") phase = "idle";
  }
</script>

<svelte:window onkeydown={onWindowKeydown} />

<!-- The compute window's home-page identity card: everything the user asked
     "am I on a compute node, with what, for how long?" answers at a glance —
     node, partition, job id, resources, and a live walltime countdown. The
     rail's ComputeStrip carries the same truth inside a workspace; this is
     the front door's version. -->
<div
  class="banner"
  class:arming={phase === "confirm" || phase === "cancelling"}
  role="status"
  title={bannerTitle}
>
  {#if phase === "confirm" || phase === "cancelling"}
    <!-- Inline mega-confirm (no modal): the whole banner turns into the
         question, in the danger tone, until answered or Escape disarms. -->
    <div
      class="confirm"
      role="alertdialog"
      aria-label={`cancel slurm job ${alloc.job_id}?`}
      aria-describedby="compute-banner-end-copy"
    >
      <span class="confirm-copy" id="compute-banner-end-copy">
        cancel slurm job {alloc.job_id}? the whole allocation ends — every terminal, agent,
        and unsaved change in this window dies
      </span>
      <div class="confirm-actions">
        <button
          class="confirm-end"
          disabled={phase === "cancelling"}
          onclick={() => void endJob()}
          >{phase === "cancelling" ? "ending…" : "end the job"}</button
        >
        <button
          class="confirm-keep"
          bind:this={keepBtn}
          disabled={phase === "cancelling"}
          onclick={() => (phase = "idle")}>keep</button
        >
      </div>
    </div>
  {:else}
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
    {#if phase === "ended"}
      <!-- Terminal state: the daemon behind this window is about to die, so
           the page stops pretending — no countdown, no further actions. -->
      <div class="right">
        <div class="ending-msg">allocation ending…</div>
        <div class="sub">
          slurm job {alloc.job_id} cancelled — this window's daemon is going down
        </div>
        {#if native}
          <button class="close-win" onclick={closeThisWindow}>close window</button>
        {/if}
      </div>
    {:else}
      <div class="right">
        <div class="countdown" class:closing>{countdown}</div>
        <div class="sub">
          {remaining === null ? "walltime" : "walltime remaining"}
        </div>
        <button
          class="end-job"
          title={`cancel slurm job ${alloc.job_id} — the whole allocation ends`}
          onclick={() => (phase = "confirm")}>end job</button
        >
      </div>
    {/if}
  {/if}
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

  /* Armed confirm: the same calm card, re-toned into the danger wash. */
  .banner.arming {
    border-color: color-mix(in srgb, var(--err) 45%, var(--edge));
    background: color-mix(in srgb, var(--err) 7%, transparent);
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

  /* Quiet by default — only wears the danger tone once pointed at. */
  .end-job {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: 11px;
    color: var(--muted);
    cursor: pointer;
    margin: 5px -7px 0 0;
    padding: 2px 7px;
    border-radius: 4px;
  }

  .end-job:hover,
  .end-job:focus-visible {
    color: var(--err);
    background: color-mix(in srgb, var(--err) 10%, transparent);
  }

  .confirm {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    width: 100%;
  }

  .confirm-copy {
    min-width: 0;
    font-size: 12.5px;
    line-height: 1.45;
    color: var(--fg);
  }

  .confirm-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }

  .confirm-end {
    appearance: none;
    font: inherit;
    font-size: 12px;
    white-space: nowrap;
    cursor: pointer;
    padding: 4px 10px;
    border-radius: 6px;
    border: 1px solid color-mix(in srgb, var(--err) 55%, var(--edge));
    background: color-mix(in srgb, var(--err) 12%, transparent);
    color: var(--err);
  }

  .confirm-end:hover:enabled {
    background: color-mix(in srgb, var(--err) 20%, transparent);
  }

  .confirm-end:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .confirm-keep {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: 12px;
    color: var(--muted);
    cursor: pointer;
    padding: 4px 8px;
    border-radius: 6px;
  }

  .confirm-keep:hover:enabled,
  .confirm-keep:focus-visible {
    color: var(--fg);
  }

  .ending-msg {
    font-family: var(--mono);
    font-size: 15px;
    font-weight: 600;
    color: var(--warn);
    line-height: 1.3;
  }

  .close-win {
    appearance: none;
    font: inherit;
    font-size: 11.5px;
    margin-top: 7px;
    padding: 3px 10px;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: none;
    color: var(--fg);
    cursor: pointer;
  }

  .close-win:hover {
    border-color: color-mix(in srgb, var(--fg) 25%, var(--edge));
  }
</style>
