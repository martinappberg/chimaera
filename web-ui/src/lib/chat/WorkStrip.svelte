<script lang="ts">
  import type { Snippet } from "svelte";
  import CountdownRing from "../shared/CountdownRing.svelte";
  import GoalGlyph from "../shared/GoalGlyph.svelte";
  import TaskListGlyph from "../shared/TaskListGlyph.svelte";
  import ActivityGlyph from "../shared/ActivityGlyph.svelte";
  import { liveClock } from "../shared/liveClock.svelte";
  import { agentsLabel } from "./AgentsTray.svelte";
  import { backgroundLabel } from "./BackgroundTray.svelte";
  import { goalStatusWord, ringFraction, untilLabel, type GoalInfo } from "./ahead";
  import type { BackgroundTask, ChatBlock, PlanEntry } from "./store.svelte";
  import { planProgress, planSummary } from "./planCard";
  import type { WorkPart } from "./workStrip";

  /**
   * The pinned work cards folded into one line, for a chat with three or
   * more kinds of work at once, including the task list. The goal leads,
   * else the task list, else the kind with the most going on. Each kind
   * keeps its name beside its glyph and count. Each part is a button that
   * opens that kind's own rows (the trays' and the Ahead card's, drawn
   * embedded) under the line; one at a time.
   */
  interface Props {
    parts: WorkPart[];
    plan: PlanEntry[];
    planRunning: boolean;
    agents: Extract<ChatBlock, { kind: "tool" }>[];
    tasks: BackgroundTask[];
    goal: GoalInfo | null;
    /** Scheduled things: how many, and the soonest one's countdown. */
    scheduled: { count: number; at: number | null; from: number };
    visible?: boolean;
    agentRows: Snippet;
    backgroundRows: Snippet;
    goalRows: Snippet;
    scheduleRows: Snippet;
    planRows: Snippet;
  }
  let {
    parts,
    plan,
    planRunning,
    agents,
    tasks,
    goal,
    scheduled,
    visible = true,
    agentRows,
    backgroundRows,
    goalRows,
    scheduleRows,
    planRows,
  }: Props = $props();

  const progress = $derived(planProgress(plan));
  const lead = $derived.by<WorkPart | null>(() => {
    if (goal !== null) return "goal";
    if (parts.includes("plan")) return "plan";
    if (agents.length > 0 && agents.length >= tasks.length) return "agents";
    if (tasks.length > 0) return "background";
    return parts[0] ?? null;
  });
  let open = $state<WorkPart | null>(null);
  /** The open part, while it still has anything to show. */
  const shown = $derived(open !== null && parts.includes(open) ? open : null);

  const clock = liveClock(() => visible && scheduled.count > 0);
  const now = $derived(scheduled.count > 0 ? clock.ms : 0);
  const monitors = $derived(tasks.some((t) => t.monitor));
  const scheduleLabel = $derived(
    `${scheduled.count === 1 ? "1 scheduled" : `${scheduled.count} scheduled`}${scheduled.at !== null ? ` · next ${untilLabel(scheduled.at, now)}` : ""}`,
  );

  const names: Record<WorkPart, string> = {
    goal: "Goal", plan: "Tasks", agents: "Agents", background: "Running", schedule: "Scheduled",
  };
  /** Native focus may reveal only an edge of an overflowing button. Keep
   *  its whole label visible by moving this row's scroll, never the page. */
  function revealPart(event: FocusEvent) {
    const button = event.currentTarget as HTMLButtonElement;
    const row = button.parentElement;
    if (!row) return;
    const box = button.getBoundingClientRect();
    const bounds = row.getBoundingClientRect();
    if (box.left < bounds.left) row.scrollLeft += box.left - bounds.left;
    else if (box.right > bounds.right) row.scrollLeft += box.right - bounds.right;
  }
  function title(part: WorkPart): string {
    switch (part) {
      case "goal": {
        const word = goal !== null ? goalStatusWord(goal.status) : null;
        return `Goal: ${goal?.objective ?? ""}${word !== null ? ` (${word})` : ""}`;
      }
      case "agents":
        return agentsLabel(agents);
      case "plan":
        return `Tasks: ${progress.done}/${progress.total} done · ${planSummary(plan, planRunning)}`;
      case "background":
        return backgroundLabel(tasks);
      case "schedule":
        return scheduleLabel;
    }
  }
</script>

{#snippet glyph(part: WorkPart)}
  {#if part === "goal"}
    <GoalGlyph tone={goal?.status === "active" ? "active" : goal?.status === "paused" ? "idle" : "warn"} />
  {:else if part === "agents"}
    <span class="spark" class:still={!visible}>✳</span>
  {:else if part === "background"}
    <span class="spark" class:still={!visible}>{#if monitors}◉{:else}<ActivityGlyph />{/if}</span>
  {:else if part === "plan"}
    <TaskListGlyph />
  {:else}
    <CountdownRing
      fraction={ringFraction(scheduled.at, scheduled.from, now)}
      soon={scheduled.at !== null && scheduled.at - now < 60_000}
    />
  {/if}
{/snippet}

<div class="strip pinned-card quiet-scrollbar" class:visible class:plan-open={shown === "plan"}>
  <div class="line">
    {#each parts as part (part)}
      <button
        type="button"
        class="part"
        class:lead={part === lead}
        aria-expanded={shown === part}
        aria-label={title(part)}
        title={title(part)}
        onfocus={revealPart}
        onclick={() => (open = shown === part ? null : part)}
      >
        <span class="glyph">{@render glyph(part)}</span>
        <span class="kind">{names[part]}</span>
        {#if part === "goal"}
          <span class="text">{goal?.objective}</span>
        {:else if part === "plan"}
          <span class="n">{progress.done}/{progress.total}</span>
          {#if part === lead}
            <span class="text plan-summary">{planSummary(plan, planRunning)}</span>
          {/if}
        {:else}
          <span class="n">{part === "agents"
              ? agents.length
              : part === "background"
                ? tasks.length
                : untilLabel(scheduled.at, now) || scheduled.count}</span>
        {/if}
      </button>
    {/each}
  </div>
  {#if shown !== null}
    <!-- Tray rows start under the label, as in their own cards; the Ahead
         rows bring their own glyph column. -->
    <div class="rows" class:indent={shown === "agents" || shown === "background"}>
      {#if shown === "goal"}
        {@render goalRows()}
      {:else if shown === "plan"}
        {@render planRows()}
      {:else if shown === "agents"}
        {@render agentRows()}
      {:else if shown === "background"}
        {@render backgroundRows()}
      {:else}
        {@render scheduleRows()}
      {/if}
    </div>
  {/if}
</div>

<style>
  /* The trays' chrome (the shared .pinned-card recipe and padding), one line
     tall at rest. Parts tint on hover and while open; nothing changes size. */
  .strip {
    container-type: inline-size;
    padding: 3px 6px 4px 5px;
    font-size: var(--text-sm);
    line-height: 18px;
    color: var(--muted);
    max-height: 200px;
    overflow-y: auto;
  }
  /* The task list inside scrolls itself (and keeps the step in flight in
     view); a second clamp out here gave the pair two scrollbars. */
  .strip.plan-open {
    max-height: none;
    overflow-y: visible;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
    overflow-x: auto;
  }
  .part {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 22px;
    padding: 0 6px;
    border: none;
    border-radius: 5px;
    background: none;
    color: inherit;
    font: inherit;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    cursor: pointer;
    transition: color 0.12s ease, background-color 0.12s ease;
  }
  .part:hover,
  .part:focus-visible,
  .part[aria-expanded="true"] {
    color: var(--fg);
    background: var(--hover-tint);
  }
  .part.lead {
    flex: 1 0 120px;
    min-width: 120px;
  }
  .text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .kind,
  .n {
    flex: none;
  }
  @container (max-width: 520px) {
    .plan-summary {
      display: none;
    }
    .part.lead {
      min-width: 90px;
      flex-basis: 90px;
    }
  }
  .glyph {
    flex: none;
    width: 14px;
    height: 14px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .spark {
    display: inline-flex;
    align-items: center;
    color: var(--accent);
    animation: tray-breathe 1.8s ease-in-out infinite;
  }
  .spark.still {
    animation: none;
    color: var(--muted);
  }
  :global(html.app-hidden) .spark {
    animation-play-state: paused;
  }
  @media (prefers-reduced-motion: reduce) {
    .spark {
      animation: none;
    }
  }
  .rows {
    margin: 4px 6px 1px;
    padding: 3px 0 1px;
    border-top: 1px solid var(--edge);
  }
  .rows.indent {
    padding-left: 23px;
  }
</style>
