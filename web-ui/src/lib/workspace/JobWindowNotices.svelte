<script lang="ts">
  /**
   * What a cluster workspace's window (served from inside a Slurm job) says
   * about the job's end (docs/design/hpc-portal-plan.md §4.5):
   * - under an hour left (and once more under ten minutes): a non-blocking
   *   banner offering to continue in a new job — it queues now with the same
   *   setup, and when it starts this job's workspaces move over and this
   *   window follows (the shell opens it there);
   * - once the shell reports the job ended, or this workspace was closed
   *   (`host-status` "ended" on this window's key): a calm overlay — the work
   *   is gone, the chats aren't.
   * A person starts every job (plan §2.4): nothing here queues anything
   * without a click.
   */
  import { clusterContinueJob, closeThisWindow, isNativeShell, openWindow } from "../net/native";
  import { pageVisible } from "../shared/visibility";
  import { modalFocus } from "../shared/modalFocus";
  import { focusOnMount } from "../shared/focusOnMount";
  import { parseSlurmTimeLeft, type ComputeSelf } from "./compute";
  import {
    CONTINUE_LAST_CALL_SECS,
    CONTINUE_OFFER_SECS,
    endedScreenWords,
    stopsInWords,
  } from "./cluster";

  interface Props {
    /** The cluster this window's job runs on. */
    alias: string;
    /** The cluster workspace the job serves; null on a window from before. */
    cws: string | null;
    /** The daemon's own allocation (null until the compute snapshot lands). */
    self: ComputeSelf | null;
    /** Client clock when the snapshot carrying `self` arrived. */
    receivedAt: number;
    /** Set once the shell reported the job ended (`reason` = Slurm's state). */
    ended: { reason: string | null } | null;
    /** The reconnect strip holds the top edge right now — sit below it. */
    stacked?: boolean;
  }

  let { alias, cws, self: alloc, receivedAt, ended, stacked = false }: Props = $props();

  const nativeJob = $derived(isNativeShell() && cws !== null);
  /** An attached job (held by the app) can't continue: its reminder only
   *  says when it ends. */
  const attached = $derived(alloc?.attached === true);
  const canContinue = $derived(nativeJob && !attached);

  // A minute's resolution is all "Stops in 58 min" needs; paused while
  // hidden, caught up on return (the effect re-runs).
  let now = $state(Date.now());
  $effect(() => {
    if (!$pageVisible || ended !== null || !nativeJob) return;
    now = Date.now();
    const t = setInterval(() => (now = Date.now()), 60_000);
    return () => clearInterval(t);
  });

  const remaining = $derived.by(() => {
    if (alloc === null) return null;
    const base = parseSlurmTimeLeft(alloc.time_left);
    if (base === null) return null;
    return Math.max(0, base - Math.floor((now - receivedAt) / 1000));
  });

  /** Which reminder applies now: the hour one, then the ten-minute one. */
  const tier = $derived(
    remaining === null || remaining <= 0 || remaining >= CONTINUE_OFFER_SECS
      ? null
      : remaining < CONTINUE_LAST_CALL_SECS
        ? "last"
        : "hour",
  );
  /** The reminder the user dismissed — the ten-minute one still comes. */
  let dismissed = $state<"hour" | "last" | null>(null);
  let phase = $state<"offer" | "asking" | "waiting">("offer");
  let continueError = $state<string | null>(null);
  /** The "new job waiting" note was dismissed (the job stays queued). */
  let waitingHidden = $state(false);

  const showBanner = $derived(
    nativeJob &&
      ended === null &&
      (phase === "waiting"
        ? !waitingHidden
        : tier !== null && dismissed !== "last" && !(dismissed === "hour" && tier === "hour")),
  );

  async function continueOnNewNode(): Promise<void> {
    if (cws === null || phase !== "offer") return;
    phase = "asking";
    continueError = null;
    try {
      const result = await clusterContinueJob(alias, { workspaceId: cws });
      if (result.kind === "refused") {
        continueError = result.message;
        phase = "offer";
        return;
      }
      phase = "waiting";
    } catch (e) {
      continueError = e instanceof Error ? e.message : String(e);
      phase = "offer";
    }
  }

  function dismiss(): void {
    // The waiting note is information, not a question: hiding it keeps the
    // queued job — the window still re-homes when it starts.
    if (phase === "waiting") waitingHidden = true;
    else dismissed = tier;
  }

  /** On its way to another job: the shell reopens this window there. */
  const moving = $derived(ended !== null && ended.reason === "moving");

  let leaving = $state(false);
  let leaveError = $state<string | null>(null);

  /** Back to the cluster: raise (or open) the local Home, then retire this
   *  window — its server is gone with the job. */
  async function backToHost(): Promise<void> {
    if (leaving) return;
    leaving = true;
    leaveError = null;
    try {
      await openWindow(null, null);
      closeThisWindow();
    } catch (e) {
      leaveError = e instanceof Error ? e.message : String(e);
      leaving = false;
    }
  }
</script>

{#if ended !== null}
  <div class="ended-overlay">
    <div
      class="ended-panel"
      role="alertdialog"
      aria-modal="true"
      aria-label={endedScreenWords(ended.reason)}
      tabindex="-1"
      use:modalFocus
    >
      <p class="ended-copy">
        {#if moving}<span class="spinner" aria-hidden="true"></span>{/if}
        {endedScreenWords(ended.reason)}
      </p>
      {#if leaveError !== null}<p class="ended-err">{leaveError}</p>{/if}
      <div class="ended-acts">
        {#if isNativeShell()}
          <button class="quiet" onclick={closeThisWindow}>Close window</button>
          {#if !moving}
            <button class="primary" disabled={leaving} use:focusOnMount onclick={() => void backToHost()}
              >Back to {alias}</button
            >
          {/if}
        {/if}
      </div>
    </div>
  </div>
{:else if showBanner}
  <div class="job-banner" class:stacked role="status" aria-live="polite">
    {#if phase === "waiting"}
      <span class="spinner" aria-hidden="true"></span>
      <span class="copy">New job waiting for a node — you keep working here until it starts.</span>
    {:else}
      <span class="copy">
        <strong>{stopsInWords(remaining ?? 0)}</strong>
        {#if attached}
          It's held by this app, so it can't continue in a new job — start one from the cluster page to keep working. Your chats are saved.
        {:else}
          Continue in a new job and your chats come with you.
        {/if}
        {#if continueError !== null}<span class="err">{continueError}</span>{/if}
      </span>
      {#if canContinue}
        <button class="primary" disabled={phase === "asking"} onclick={() => void continueOnNewNode()}
          >{phase === "asking" ? "Queuing…" : "Continue in a new job"}</button
        >
      {/if}
    {/if}
    <button class="dismiss" aria-label="Dismiss" title="Dismiss" onclick={dismiss}>×</button>
  </div>
{/if}

<style>
  .job-banner {
    position: fixed;
    top: 14px;
    left: 50%;
    z-index: 185; /* over the update toast, under the reconnect overlays */
    width: min(520px, calc(100vw - 28px));
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 10px 10px 14px;
    transform: translateX(-50%);
    background: var(--overlay-bg);
    border: 1px solid color-mix(in srgb, var(--warn) 40%, var(--edge));
    border-radius: 9px;
    box-shadow: 0 8px 28px color-mix(in srgb, var(--fg) 12%, transparent);
    animation: banner-in 0.12s ease-out;
  }

  .job-banner.stacked {
    top: 84px;
  }

  @keyframes banner-in {
    from {
      opacity: 0;
      transform: translate(-50%, -4px);
    }
  }

  .copy {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--fg);
  }

  .copy strong {
    font-weight: 600;
    color: var(--warn);
  }

  .err {
    display: block;
    margin-top: 3px;
    font-size: var(--text-xs);
    color: var(--err);
    white-space: pre-wrap;
  }

  .primary {
    appearance: none;
    flex: none;
    border: 1px solid color-mix(in srgb, var(--accent) 50%, var(--edge));
    background: color-mix(in srgb, var(--accent) 10%, var(--bg));
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 4px 11px;
    border-radius: 6px;
    cursor: pointer;
    white-space: nowrap;
  }

  .primary:hover:enabled {
    border-color: var(--accent);
  }

  .primary:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .dismiss {
    appearance: none;
    flex: none;
    border: none;
    background: none;
    color: var(--muted);
    font-size: var(--text-lg);
    line-height: 1;
    padding: 2px 6px;
    border-radius: 4px;
    cursor: pointer;
  }

  .dismiss:hover {
    color: var(--fg);
  }

  .spinner {
    flex: none;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 1.5px solid color-mix(in srgb, var(--accent) 30%, transparent);
    border-top-color: var(--accent);
    animation: spin 0.9s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  :global(html.app-hidden) .spinner {
    animation-play-state: paused;
  }

  .ended-overlay {
    position: fixed;
    inset: 0;
    z-index: 195; /* over the reconnect surfaces: the job is gone, not the link */
    display: flex;
    align-items: flex-start;
    justify-content: center;
    background: var(--scrim);
    animation: fade 0.12s ease-out;
  }

  @keyframes fade {
    from {
      opacity: 0;
    }
  }

  .ended-panel {
    margin-top: 22vh;
    width: min(420px, calc(100vw - 2rem));
    padding: 20px 20px 16px;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 9px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.22);
  }

  .ended-copy {
    margin: 0;
    font-size: var(--text-md);
    line-height: 1.5;
    color: var(--fg);
  }

  .ended-copy .spinner {
    display: inline-block;
    vertical-align: -1px;
    margin-right: 8px;
  }

  .ended-err {
    margin: 8px 0 0;
    font-size: var(--text-sm);
    color: var(--err);
  }

  .ended-acts {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }

  .quiet {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--muted);
    cursor: pointer;
    padding: 4px 8px;
    border-radius: 4px;
  }

  .quiet:hover {
    color: var(--fg);
  }
</style>
