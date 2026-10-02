<script lang="ts">
  import {
    endOwnJob,
    formatSlurmDuration,
    parseSlurmTimeLeft,
    type ComputeSelf,
  } from "./compute";
  import { closeThisWindow, clusterClose, isNativeShell } from "../net/native";
  import { ApiError, getHostLabel, getJobContext } from "../net/api";
  import { pageVisible } from "../shared/visibility";

  interface Props {
    /** The daemon's own allocation (present in every compute-node window). */
    self: ComputeSelf;
    /** CLIENT clock when the snapshot carrying `self` was received. */
    receivedAt: number;
  }

  let { self: alloc, receivedAt }: Props = $props();

  /** The end-job flow: idle → confirm (inline, Escape disarms) → cancelling
   *  (cancel in flight) → ended (this window's daemon is going down). A
   *  definitive failure returns to confirm with `cancelError` inline. */
  let phase = $state<"idle" | "confirm" | "cancelling" | "ended">("idle");
  /** Why the last cancel attempt FAILED (the job is still running). */
  let cancelError = $state<string | null>(null);
  let keepBtn = $state<HTMLButtonElement | null>(null);

  const native = isNativeShell();
  /** The cluster this job window was tunnelled to (the shell put the alias
   *  in the hash); "local" means there is no shell tunnel to speak of. */
  const loginAlias = getHostLabel();
  /** The cluster workspace this window shows (`cws=`); null on an old window. */
  const clusterWs = getJobContext()?.cws ?? null;
  /** The app's workspace window: the action closes this workspace only — the
   *  job (and any other workspace in it) keeps running. Elsewhere (a browser
   *  tab) this daemon can only end its own job. */
  const closesOnly = native && loginAlias !== "local" && clusterWs !== null;
  const verb = closesOnly ? "Close" : "Stop";

  // Same live-tick discipline as ComputeStrip: the fetched time_left only
  // moves per poll, so tick locally against the receipt time and re-sync on
  // every fetch. Once ended there's nothing left to count down. Gated on
  // document visibility (the compute.ts idiom) with a catch-up on return.
  let now = $state(Date.now());
  $effect(() => {
    if (phase === "ended" || !$pageVisible) return;
    now = Date.now();
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
      ? `Slurm job ${alloc.job_id} is ending, and this window's server with it`
      : `Slurm job ${alloc.job_id} on ${alloc.node}. This workspace runs inside the job and stops at its time limit.`,
  );

  async function endJob(): Promise<void> {
    if (phase !== "confirm") return;
    cancelError = null;
    phase = "cancelling";
    if (native && loginAlias !== "local" && clusterWs !== null) {
      // Native: the shell closes THIS workspace in its job (other workspaces
      // in the same job keep running) — its chimaera saves the chats and
      // exits, and the window's forward goes with it.
      try {
        await clusterClose(loginAlias, clusterWs);
      } catch (e) {
        cancelError = e instanceof Error ? e.message : String(e);
        phase = "confirm";
        return;
      }
      phase = "ended";
      return;
    }
    // Browser (or a window from before `cws=`): this daemon ends its own
    // job. Only a NETWORK-level death is the expected success signal (the
    // job takes this very daemon down, so the response often can't land); a
    // daemon that ANSWERED with an error means the job is still running —
    // say so instead of claiming ended.
    try {
      await endOwnJob();
    } catch (e) {
      if (e instanceof ApiError) {
        cancelError = e.message;
        phase = "confirm";
        return;
      }
      // fetch rejected mid-flight — the daemon died under us, as expected
    }
    phase = "ended";
  }

  function disarm(): void {
    phase = "idle";
    cancelError = null;
  }

  function onWindowKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape" && phase === "confirm") disarm();
  }
</script>

<svelte:window onkeydown={onWindowKeydown} />

<!-- The job window's home-page identity card: everything the user asked
     "am I on a compute node, with what, for how long?" answers at a glance —
     node, partition, job id, resources, and a live countdown to the job's
     time limit. The rail's ComputeStrip carries the same truth inside a
     workspace; this is the front door's version. -->
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
      aria-label={closesOnly ? "Close this workspace?" : "Stop this job?"}
      aria-describedby="compute-banner-end-copy"
    >
      <div class="confirm-text">
        <span class="confirm-copy" id="compute-banner-end-copy">
          {#if closesOnly}
            Close this workspace? Its chats are saved and the job keeps running. Terminals,
            running commands and unsaved edits in this window end.
          {:else}
            Stop this job? Chats are saved. Terminals, running commands and unsaved edits in this
            window end with it.
          {/if}
        </span>
        {#if cancelError !== null}
          <span class="confirm-err" role="alert">Couldn't stop it: {cancelError}</span>
        {/if}
      </div>
      <div class="confirm-actions">
        <button
          class="confirm-end"
          disabled={phase === "cancelling"}
          onclick={() => void endJob()}
          >{phase === "cancelling"
            ? `${closesOnly ? "Closing" : "Stopping"}…`
            : cancelError !== null
              ? "Try again"
              : verb}</button
        >
        <button
          class="confirm-keep"
          bind:this={keepBtn}
          disabled={phase === "cancelling"}
          onclick={disarm}>{closesOnly ? "Keep open" : "Keep running"}</button
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
        <span>Job</span>
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
        <div class="ending-msg">{closesOnly ? "Closing…" : "Stopping…"}</div>
        <div class="sub">
          {closesOnly
            ? "This workspace is closing. Its chats are saved."
            : "The job is ending, and this window with it. Chats are saved."}
        </div>
        {#if native}
          <button class="close-win" onclick={closeThisWindow}>Close window</button>
        {/if}
      </div>
    {:else}
      <div class="right">
        <div class="countdown" class:closing>{countdown}</div>
        <div class="sub">
          {remaining === null ? "time limit" : "left"}
        </div>
        <button
          class="end-job"
          title={closesOnly
            ? "Close this workspace — its chats are saved; the job keeps running"
            : `Stop this job (Slurm ${alloc.job_id}) — chats are saved`}
          onclick={() => (phase = "confirm")}>{verb}</button
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
    font-size: var(--text-xs);
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
    font-size: var(--text-sm);
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
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .right {
    flex: none;
    text-align: right;
  }

  .countdown {
    font-family: var(--mono);
    font-size: calc(var(--text-lg) + 5px);
    font-weight: 600;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
    line-height: 1.15;
  }

  .countdown.closing {
    color: var(--warn);
  }

  .sub {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  /* Quiet by default — only wears the danger tone once pointed at. */
  .end-job {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
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

  .confirm-text {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  .confirm-copy {
    min-width: 0;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--fg);
  }

  /* A definitive cancel failure (the daemon answered an error): the job is
     still running, so the confirm stays armed and says why. */
  .confirm-err {
    font-size: var(--text-xs);
    line-height: 1.4;
    color: var(--err);
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
    font-size: var(--text-sm);
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
    font-size: var(--text-sm);
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
    font-size: var(--text-lg);
    font-weight: 600;
    color: var(--warn);
    line-height: 1.3;
  }

  .close-win {
    appearance: none;
    font: inherit;
    font-size: var(--text-xs);
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
