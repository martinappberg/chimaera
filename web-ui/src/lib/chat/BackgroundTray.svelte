<script lang="ts">
  import Chevron from "../shared/Chevron.svelte";
  import type { BackgroundTask } from "./store.svelte";

  /**
   * The live monitor for BACKGROUND work — backgrounded Bash commands and
   * workflows the agent left running detached from the turn. Sibling of
   * AgentsTray (subagents), same quiet pinned chrome. Background tasks are
   * cross-turn by definition: they outlive the turn that started them, so
   * without a stable surface they'd be invisible the moment the transcript
   * moves on. Collapsed by default to a one-line count; expand for each
   * task's description, live elapsed, status, and a stop button. Completion/
   * failure verdicts land as transcript notices (the store), not here — a
   * finished task simply leaves the tray.
   */
  interface Props {
    /** The agent's live background-task set (level-set from the wire). */
    tasks: BackgroundTask[];
    /** Stop one (claude stop_task, generic over its task registry).
     *  Omitted when unsupported. */
    onStop?: (id: string) => void;
  }
  let { tasks, onStop }: Props = $props();

  let open = $state(false);

  /** 1 Hz clock for the elapsed column — only while the tray is mounted
   *  (it unmounts with an empty set), and torn down with it. */
  let now = $state(Date.now());
  $effect(() => {
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  /** Elapsed since the driver first saw the task. The stamp is daemon-side
   *  epoch ms, so clamp: a skewed client clock must not render "-3s". */
  function elapsed(t: BackgroundTask): string {
    if (t.startedAtMs <= 0) return "";
    const s = Math.max(0, Math.floor((now - t.startedAtMs) / 1000));
    if (s >= 3600) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
    if (s >= 60) return `${Math.floor(s / 60)}m ${(s % 60).toString().padStart(2, "0")}s`;
    return `${s}s`;
  }
</script>

<div class="tray">
  <button
    class="tray-head"
    aria-expanded={open}
    onclick={() => (open = !open)}
    title={open ? "collapse" : "expand"}
  >
    <Chevron {open} />
    <span class="spark" aria-hidden="true">⧖</span>
    <!-- aria-live on the summary only: the count changing is worth announcing,
         the per-second elapsed ticks are not. -->
    <span class="head-label" role="status" aria-live="polite"
      >{tasks.length === 1
        ? "background task running"
        : `${tasks.length} background tasks running`}</span
    >
  </button>
  {#if open}
    <div class="rows">
      {#each tasks as task (task.id)}
        <div class="task">
          <span class="dot" aria-hidden="true"></span>
          <div class="body">
            <!-- The lane name (local_bash, …) stays canonical in the tooltip. -->
            <span class="name" title={task.taskType}>{task.description}</span>
            {#if task.status !== "running"}
              <span class="status">{task.status}</span>
            {/if}
            {#if elapsed(task) !== ""}
              <span class="elapsed">{elapsed(task)}</span>
            {/if}
          </div>
          {#if onStop !== undefined}
            <button class="stop" title="stop this background task" onclick={() => onStop?.(task.id)}>
              <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
                <rect x="4.5" y="4.5" width="7" height="7" rx="1" fill="currentColor" />
              </svg>
            </button>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  /* Same pinned-monitor chrome as AgentsTray (border-top, muted, theme
     tokens) so the work trays read as one family. */
  .tray {
    flex: none;
    border-top: 1px solid var(--edge);
    padding: 5px 14px 6px;
    font-size: var(--text-sm);
    max-height: 168px;
    overflow-y: auto;
    background: color-mix(in srgb, var(--accent) 4%, transparent);
    animation: rise 0.15s ease; /* shared keyframe in app.css */
  }
  .tray-head {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    padding: 1px 0 3px;
    cursor: pointer;
  }
  .head-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .spark {
    flex: none;
    color: var(--accent);
    /* Presence, not alarm — the same slow breathe as the agents tray. */
    animation: tray-breathe 1.8s ease-in-out infinite;
  }
  .task {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 2px 0;
  }
  .dot {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    animation: pulse 1.4s ease-in-out infinite; /* shared keyframe in app.css */
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 8px;
    overflow: hidden;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
    font-family: var(--mono, monospace);
  }
  .status {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .elapsed {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .stop {
    flex: none;
    display: inline-flex;
    background: none;
    border: none;
    color: var(--muted);
    padding: 2px;
    cursor: pointer;
    transition: color 0.12s ease;
  }
  .stop:hover {
    color: var(--err);
  }
  @keyframes tray-breathe {
    0%,
    100% {
      opacity: 0.9;
    }
    50% {
      opacity: 0.5;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .tray,
    .dot,
    .spark {
      animation: none;
    }
  }
</style>
