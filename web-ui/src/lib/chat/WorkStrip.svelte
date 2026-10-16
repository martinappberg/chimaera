<script lang="ts">
  import type { Snippet } from "svelte";
  import CountdownRing from "../shared/CountdownRing.svelte";
  import GoalGlyph from "../shared/GoalGlyph.svelte";
  import { liveClock } from "../shared/liveClock.svelte";
  import { agentsLabel } from "./AgentsTray.svelte";
  import { backgroundLabel } from "./BackgroundTray.svelte";
  import { goalStatusWord, ringFraction, untilLabel, type GoalInfo } from "./ahead";
  import type { BackgroundTask, ChatBlock } from "./store.svelte";

  /**
   * The pinned work cards folded into one line, for a chat with three or
   * more kinds of work at once (subagents, background work, a goal, a
   * schedule): one border instead of a tower of them. The goal leads — it
   * says why the rest is happening — else the kind with the most going on;
   * every other kind is a glyph and a number. Each part is a button that
   * opens that kind's own rows (the trays' and the Ahead card's, drawn
   * embedded) under the line; one at a time.
   */
  interface Props {
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
  }
  let {
    agents,
    tasks,
    goal,
    scheduled,
    visible = true,
    agentRows,
    backgroundRows,
    goalRows,
    scheduleRows,
  }: Props = $props();

  type Part = "goal" | "agents" | "background" | "schedule";
  const parts = $derived.by<Part[]>(() => {
    const out: Part[] = [];
    if (goal !== null) out.push("goal");
    if (agents.length > 0) out.push("agents");
    if (tasks.length > 0) out.push("background");
    if (scheduled.count > 0) out.push("schedule");
    return out;
  });
  const lead = $derived.by<Part | null>(() => {
    if (goal !== null) return "goal";
    if (agents.length > 0 && agents.length >= tasks.length) return "agents";
    if (tasks.length > 0) return "background";
    return parts[0] ?? null;
  });
  let open = $state<Part | null>(null);
  /** The open part, while it still has anything to show. */
  const shown = $derived(open !== null && parts.includes(open) ? open : null);

  const clock = liveClock(() => visible && scheduled.count > 0);
  const now = $derived(scheduled.count > 0 ? clock.ms : 0);
  const monitors = $derived(tasks.some((t) => t.monitor));
  const scheduleLabel = $derived(
    `${scheduled.count === 1 ? "1 scheduled" : `${scheduled.count} scheduled`}${scheduled.at !== null ? ` · next ${untilLabel(scheduled.at, now)}` : ""}`,
  );

  function title(part: Part): string {
    switch (part) {
      case "goal": {
        const word = goal !== null ? goalStatusWord(goal.status) : null;
        return `Goal: ${goal?.objective ?? ""}${word !== null ? ` (${word})` : ""}`;
      }
      case "agents":
        return agentsLabel(agents);
      case "background":
        return backgroundLabel(tasks);
      case "schedule":
        return scheduleLabel;
    }
  }
</script>

{#snippet glyph(part: Part)}
  {#if part === "goal"}
    <GoalGlyph tone={goal?.status === "active" ? "active" : goal?.status === "paused" ? "idle" : "warn"} />
  {:else if part === "agents"}
    <span class="spark" class:still={!visible}>✳</span>
  {:else if part === "background"}
    <span class="spark" class:still={!visible}>{monitors ? "◉" : "⧖"}</span>
  {:else}
    <CountdownRing
      fraction={ringFraction(scheduled.at, scheduled.from, now)}
      soon={scheduled.at !== null && scheduled.at - now < 60_000}
    />
  {/if}
{/snippet}

<div class="strip pinned-card" class:visible>
  <div class="line">
    {#each parts as part (part)}
      <button
        type="button"
        class="part"
        class:lead={part === lead}
        aria-expanded={shown === part}
        title={title(part)}
        onclick={() => (open = shown === part ? null : part)}
      >
        <span class="glyph">{@render glyph(part)}</span>
        {#if part === lead}
          <span class="text"
            >{part === "goal"
              ? goal?.objective
              : part === "agents"
                ? agentsLabel(agents)
                : part === "background"
                  ? backgroundLabel(tasks)
                  : scheduleLabel}</span
          >
        {:else}
          <span class="n"
            >{part === "agents"
              ? agents.length
              : part === "background"
                ? tasks.length
                : untilLabel(scheduled.at, now) || scheduled.count}</span
          >
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
    padding: 3px 6px 4px 5px;
    font-size: var(--text-sm);
    line-height: 18px;
    color: var(--muted);
    max-height: 200px;
    overflow-y: auto;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
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
    flex: 1;
    min-width: 0;
  }
  .text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
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
