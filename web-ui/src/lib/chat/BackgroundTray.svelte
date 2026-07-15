<script lang="ts">
  import { formatElapsedSeconds } from "../shared/time";
  import WorkTray from "../shared/WorkTray.svelte";
  import WorkTrayRow from "../shared/WorkTrayRow.svelte";
  import type { BackgroundTask } from "./store.svelte";

  /**
   * The live monitor for BACKGROUND work — backgrounded Bash commands and
   * workflows the agent left running detached from the turn. Sibling of
   * AgentsTray (subagents), same shared WorkTray/WorkTrayRow chrome.
   * Background tasks are cross-turn by definition: they outlive the turn
   * that started them, so without a stable surface they'd be invisible the
   * moment the transcript moves on. Collapsed by default to a one-line
   * count; expand for each task's description, live elapsed, status, and a
   * stop button. Completion/failure verdicts land as transcript notices
   * (the store), not here — a finished task simply leaves the tray.
   */
  interface Props {
    /** The agent's live background-task set (level-set from the wire). */
    tasks: BackgroundTask[];
    /** Stop one (claude stop_task, generic over its task registry).
     *  Omitted when unsupported. */
    onStop?: (id: string) => void;
  }
  let { tasks, onStop }: Props = $props();

  /** Bound to the shell's expanded state — the clock below gates on it. */
  let open = $state(false);

  /** 1 Hz clock for the elapsed column — only while the rows are actually
   *  visible: collapsed (the default) nothing reads `now`, so the effect
   *  bails and a long-running task doesn't tick a wake-up per second for
   *  hours. Re-arms on expand (with a fresh `now`), torn down with the
   *  tray (it unmounts on an empty set). */
  let now = $state(Date.now());
  $effect(() => {
    if (!open) return;
    now = Date.now();
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  /** Elapsed since the driver first saw the task. The stamp is daemon-side
   *  epoch ms, so clamp: a skewed client clock must not render "-3s". */
  function elapsed(t: BackgroundTask): string {
    if (t.startedAtMs <= 0) return "";
    return formatElapsedSeconds(Math.max(0, Math.floor((now - t.startedAtMs) / 1000)));
  }
</script>

<WorkTray
  glyph="⧖"
  bind:open
  label={tasks.length === 1
    ? "background task running"
    : `${tasks.length} background tasks running`}
>
  {#each tasks as task (task.id)}
    {@const e = elapsed(task)}
    <WorkTrayRow
      onStop={onStop !== undefined ? () => onStop?.(task.id) : undefined}
      stopTitle="stop this background task"
    >
      <!-- The lane name (local_bash, …) stays canonical in the tooltip. -->
      <span class="name" title={task.taskType}>{task.description}</span>
      {#if task.status !== "running"}
        <span class="status">{task.status}</span>
      {/if}
      {#if e !== ""}
        <span class="elapsed">{e}</span>
      {/if}
    </WorkTrayRow>
  {/each}
</WorkTray>

<style>
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
</style>
