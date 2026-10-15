<script lang="ts">
  import { tick } from "svelte";
  import Chevron from "../shared/Chevron.svelte";
  import type { PlanEntry } from "./store.svelte";
  import {
    planFolds,
    planProgress,
    planSummary,
    stepDetail,
    stepKeys,
    stepMark,
    stepText,
    type StepMark,
  } from "./planCard";

  /**
   * The agent's plan as a card above the composer — one row per step, a
   * circle that says its state without colour (filled check: done; plain
   * circle: the one in flight; dashed: waiting), the current step in the
   * foreground, finished steps quiet. Every runtime's plan arrives as the
   * same level-set (`store.plan`), so this card is the same for all of them.
   * Rows update in place (keyed by the agent's id, else position) and the
   * card never reorders; a finished plan folds to one line once its turn
   * ends, one click from its steps.
   */
  interface Props {
    entries: PlanEntry[];
    /** The turn is running: a finished plan stays open until it ends. */
    running: boolean;
    /** False while the retained owner is hidden (no motion, no live
     *  announcements). */
    visible?: boolean;
  }

  let { entries, running, visible = true }: Props = $props();

  const progress = $derived(planProgress(entries));
  const autoFolded = $derived(planFolds(entries, running));
  /** A user toggle wins until the automatic state next changes (a new turn
   *  re-opens the steps; finishing folds them again). */
  let userOpen = $state<boolean | null>(null);
  $effect(() => {
    void autoFolded;
    userOpen = null;
  });
  const folded = $derived(userOpen === null ? autoFolded : !userOpen);
  const summary = $derived(planSummary(entries));
  const count = $derived(`${progress.done}/${progress.total}`);
  const toggleLabel = $derived(folded ? "show the plan's steps" : "fold the plan to one line");

  /** Keep the step in flight inside the bounded list — moving only the
   *  list's own scroll, never the page, and only when it left the view. */
  let list = $state<HTMLOListElement | undefined>();
  /** The list's box height: a pane resize can push the current step out. */
  let listHeight = $state(0);
  const keys = $derived(stepKeys(entries));
  const currentKey = $derived.by(() => {
    const i = entries.findIndex((e) => e.status === "in_progress");
    return i === -1 ? null : keys[i];
  });
  $effect(() => {
    const key = currentKey;
    void listHeight;
    const el = list;
    if (key === null || el === undefined || !visible) return;
    void tick().then(() => {
      const row = el.querySelector<HTMLElement>(`[data-step="${CSS.escape(key)}"]`);
      if (row === null) return;
      // The list is the rows' offset parent (position: relative).
      const top = row.offsetTop;
      const bottom = top + row.offsetHeight;
      if (top < el.scrollTop) el.scrollTop = top;
      else if (bottom > el.scrollTop + el.clientHeight) el.scrollTop = bottom - el.clientHeight;
    });
  });

  const MARK_WORD: Record<StepMark, string> = {
    done: "done",
    current: "in progress",
    todo: "to do",
    blocked: "waiting",
  };
</script>

{#snippet circle(mark: StepMark)}
  <svg class="mark {mark}" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
    {#if mark === "done"}
      <circle cx="8" cy="8" r="7" class="fill" />
      <path d="M4.9 8.3l2.1 2.1 4.1-4.5" class="tick" />
    {:else}
      <circle cx="8" cy="8" r="6.25" class="ring" />
    {/if}
  </svg>
{/snippet}

<section class="plan-card pinned-card" class:visible class:folded aria-label="plan">
  <!-- The toggle is a flex column of its own at the card's right, so no row —
       first, scrolled-to, or "12/15" wide — ever runs under it. -->
  <button
    class="toggle"
    aria-expanded={!folded}
    aria-label="{toggleLabel} ({progress.done} of {progress.total} done)"
    title={toggleLabel}
    onclick={() => (userOpen = folded)}
  >
    <!-- A folded finished plan already says its tally. -->
    {#if !(folded && progress.allDone)}<span class="count">{count}</span>{/if}
    <Chevron open={!folded} />
  </button>
  {#if folded}
    <button class="folded-line" aria-expanded="false" onclick={() => (userOpen = true)}>
      {@render circle(progress.allDone ? "done" : progress.current !== null ? "current" : "todo")}
      <span class="text">{summary}</span>
    </button>
  {:else}
    <ol class="steps" bind:this={list} bind:clientHeight={listHeight}>
      {#each entries as entry, i (keys[i])}
        {@const mark = stepMark(entry)}
        {@const detail = stepDetail(entry)}
        <li class="step {mark}" data-step={keys[i]}>
          {@render circle(mark)}
          <span class="body">
            <span class="line">
              <span class="text">{stepText(entry)}</span>
              <span class="sr-only">({MARK_WORD[mark]})</span>
              {#if entry.owner}<span class="owner">@{entry.owner}</span>{/if}
              {#if mark === "blocked"}
                <span class="aside">after {entry.blockedBy.map((id) => `#${id}`).join(", ")}</span>
              {/if}
            </span>
            {#if detail !== null && mark !== "done"}
              <span class="detail">{detail}</span>
            {/if}
          </span>
        </li>
      {/each}
    </ol>
  {/if}
  <!-- One polite announcement per step change, never per row. -->
  <span class="sr-only" role="status" aria-live={visible ? "polite" : "off"}
    >{progress.current !== null ? stepText(progress.current) : progress.allDone ? "plan done" : ""}</span
  >
</section>

<style>
  /* Chrome (edge, radius, tint, entrance) is the shared .pinned-card recipe
     in app.css; this is the card's own layout. */
  .plan-card {
    display: flex;
    align-items: flex-start;
    font-size: var(--text-sm);
    line-height: 1.45;
  }
  .toggle {
    order: 1;
    flex: none;
    margin: 4px 6px 0 0;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 6px;
    background: none;
    border: none;
    border-radius: 6px;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    cursor: pointer;
  }
  /* Folded, the card is one line: the toggle sits on that line's centre. */
  .plan-card.folded .toggle {
    margin-top: 0;
    align-self: center;
  }
  .toggle:hover,
  .toggle:focus-visible {
    color: var(--fg);
    background: var(--hover-tint);
  }
  .folded-line {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    flex: 1;
    min-width: 0;
    padding: 6px 8px 6px 11px;
    background: none;
    border: none;
    border-radius: var(--radius-card);
    color: var(--muted);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .folded-line:hover,
  .folded-line:focus-visible {
    color: var(--fg);
  }
  .steps {
    flex: 1;
    min-width: 0;
    list-style: none;
    margin: 0;
    padding: 5px 8px 6px 11px;
    /* About eight steps; a longer plan scrolls inside the card, which
       keeps the step in flight in view (see the script). */
    max-height: min(196px, 30vh);
    overflow-y: auto;
    position: relative;
  }
  .step {
    display: flex;
    align-items: flex-start;
    gap: 9px;
    padding: 2px 0;
    color: color-mix(in srgb, var(--fg) 80%, var(--muted));
  }
  .step.current {
    color: var(--fg);
    font-weight: 500;
  }
  .step.done {
    color: var(--muted);
  }
  .step.blocked {
    color: var(--muted);
  }
  .mark {
    flex: none;
    margin-top: 2px;
  }
  .folded-line .mark {
    margin-top: 0;
  }
  .mark .ring {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.4;
  }
  .mark.current .ring {
    stroke: var(--fg);
    stroke-width: 1.6;
  }
  /* Waiting reads as an outline still to be drawn. */
  .mark.todo .ring,
  .mark.blocked .ring {
    stroke: var(--muted);
    stroke-dasharray: 2.6 2.3;
  }
  .mark.done .fill {
    fill: var(--accent);
  }
  .mark.done .tick {
    fill: none;
    stroke: var(--bg);
    stroke-width: 1.7;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .body {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .line {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }
  /* A glance surface: subjects clip rather than wrap. */
  .text,
  .detail {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .detail {
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: normal;
  }
  .owner,
  .aside {
    flex: none;
    font-size: var(--text-xs);
    font-weight: normal;
    color: var(--muted);
  }
  /* Mixed toward --fg: accent over muted is too weak for an 11px chip on
     the light theme. */
  .owner {
    color: color-mix(in srgb, var(--accent) 70%, var(--fg));
  }
</style>
