<script lang="ts">
  import { formatElapsedSeconds } from "../shared/time";
  import WorkTray from "../shared/WorkTray.svelte";
  import WorkTrayRow from "../shared/WorkTrayRow.svelte";
  import { backgroundKind, countKinds } from "./backgroundKinds";
  import type { BackgroundTask } from "./store.svelte";

  /**
   * The live monitor for BACKGROUND work — backgrounded commands, Monitor
   * watches, backgrounded agents and workflows the agent left running
   * detached from the turn. Sibling of
   * AgentsTray (subagents), same shared WorkTray/WorkTrayRow chrome.
   * Background tasks are cross-turn by definition: they outlive the turn
   * that started them, so without a stable surface they'd be invisible the
   * moment the transcript moves on. Collapsed by default to a one-line
   * per-kind count; expand for each task's kind, description, live
   * elapsed, status, and a stop button. Completion/failure verdicts land as
   * transcript "finished" rows (the store), not here — a finished task
   * simply leaves the tray.
   */
  interface Props {
    /** The agent's live background-task set (level-set from the wire). */
    tasks: BackgroundTask[];
    /** Stop one (claude stop_task, generic over its task registry).
     *  Omitted when unsupported. */
    onStop?: (id: string) => void;
    /** False while the owning retained chat tab is hidden. */
    visible?: boolean;
  }
  let { tasks, onStop, visible = true }: Props = $props();

  /** Bound to the shell's expanded state — the clock below gates on it. */
  let open = $state(false);

  /** 1 Hz clock for the elapsed column — only while the rows are actually
   *  visible: collapsed (the default) nothing reads `now`, so the effect
   *  bails and a long-running task doesn't tick a wake-up per second for
   *  hours. Re-arms on expand (with a fresh `now`), torn down with the
   *  tray (it unmounts on an empty set). */
  let now = $state(Date.now());
  $effect(() => {
    if (!visible || !open) return;
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

  /** The dot row stays glanceable: at most this many dots; the newest win
   *  (the entries are already the server-capped newest) and the count text
   *  stays the honest total. */
  const DOTS_MAX = 24;

  /** The collapsed header says exactly what is out, and doubles as the
   *  session's "still waiting on something" signal between turns: a watch is
   *  named ("Watching “build log”"), everything else counted per kind
   *  ("1 command · 1 agent in the background"). */
  const monitors = $derived(tasks.filter((t) => t.monitor));
  const label = $derived.by(() => {
    const watch =
      monitors.length === 1
        ? `Watching “${monitors[0].description}”`
        : monitors.length > 1
          ? `Watching ${monitors.length} monitors`
          : "";
    const rest = countKinds(tasks, ["monitor"]);
    if (watch === "") return `${rest} in the background`;
    return rest === "" ? watch : `${watch} · ${rest} in the background`;
  });

  function dotClass(state: string): string {
    if (state === "done") return "done";
    if (state.includes("fail") || state.includes("error")) return "err";
    return "";
  }
</script>

<WorkTray
  glyph={monitors.length > 0 ? "◉" : "⧖"}
  bind:open
  {visible}
  {label}
>
  {#each tasks as task (task.id)}
    {@const e = elapsed(task)}
    <WorkTrayRow
      onStop={onStop !== undefined ? () => onStop?.(task.id) : undefined}
      stopTitle={task.monitor ? "stop this monitor" : "stop this background task"}
      {visible}
    >
      <span class="kind" class:ambient={task.ambient} title={task.ambient ? "housekeeping the agent runs on its own" : undefined}
        >{backgroundKind(task)}</span
      >
      <!-- The lane name (local_bash, …) stays canonical in the tooltip;
           a workflow row leads with its meta.name. -->
      <span
        class="name"
        title={task.workflowName !== null
          ? `${task.taskType} · ${task.description}`
          : task.taskType}>{task.workflowName ?? task.description}</span
      >
      {#if task.agents.length > 0}
        <!-- Per-agent dots (newest {DOTS_MAX}); the count text is the honest
             aggregate, so the dots stay decoration for a screen reader. -->
        <span class="dots" aria-hidden="true">
          {#if task.agents.length > DOTS_MAX}<span class="dots-more">⋯</span>{/if}
          {#each task.agents.slice(-DOTS_MAX) as agent (agent.index)}
            <span
              class="wf-dot {dotClass(agent.state)}"
              title={`${agent.label} — ${agent.state}${agent.resultPreview !== null ? `: ${agent.resultPreview}` : ""}`}
            ></span>
          {/each}
        </span>
        <span class="count">{task.agentsDone}/{task.agentsTotal} agents</span>
      {/if}
      {#if task.status !== "running"}
        <span class="status">{task.status}</span>
      {:else if task.monitor}
        <span class="status">watching</span>
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
  .kind {
    flex: none;
    font-size: var(--text-xs);
    padding: 0 6px;
    border-radius: 999px;
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 6%, transparent);
  }
  .kind.ambient {
    opacity: 0.6;
  }
  .dots {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 3px;
  }
  .dots-more {
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 1;
  }
  .wf-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    border: 1px solid var(--accent);
    box-sizing: border-box;
  }
  .wf-dot.done {
    background: var(--accent);
  }
  .wf-dot.err {
    background: var(--err);
    border-color: var(--err);
  }
  .count {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .elapsed {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
</style>
