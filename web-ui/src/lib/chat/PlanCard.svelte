<script lang="ts">
  import { tick } from "svelte";
  import { cubicOut } from "svelte/easing";
  import { slide } from "svelte/transition";
  import Chevron from "../shared/Chevron.svelte";
  import TaskListGlyph from "../shared/TaskListGlyph.svelte";
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
   * checkbox that says its state without colour (check: done; dash:
   * in progress; empty: waiting), the current step in the
   * foreground, finished steps quiet. Every runtime's plan arrives as the
   * same level-set (`store.plan`), so this card is the same for all of them.
   * Rows update in place (keyed by the agent's id, else position) and the
   * card never reorders. Idle plans fold to one line; unfinished steps remain
   * unchanged until the agent updates them, never inferred completion. Finished plans
   * leave when the agent moves on — the host unmounts them.
   */
  interface Props {
    entries: PlanEntry[];
    /** The turn is running: a finished plan stays open until it ends. */
    running: boolean;
    /** False while the retained owner is hidden (no motion, no live
     *  announcements). */
    visible?: boolean;
    /** The work strip owns the header and disclosure. */
    embedded?: boolean;
  }

  let { entries, running, visible = true, embedded = false }: Props = $props();

  const progress = $derived(planProgress(entries));
  const autoFolded = $derived(planFolds(entries, running));
  /** A user toggle wins until the automatic state next changes (a new turn
   *  re-opens the steps; finishing folds them again). */
  let userOpen = $state<boolean | null>(null);
  $effect(() => {
    void autoFolded;
    userOpen = null;
  });
  const folded = $derived(!embedded && (userOpen === null ? autoFolded : !userOpen));
  const summary = $derived(planSummary(entries, running));
  const inactive = $derived(!running);
  const count = $derived(`${progress.done}/${progress.total}`);
  const toggleLabel = $derived(folded ? "show tasks" : "collapse tasks");

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

  /** Motion only where someone can see it: a hidden tab or reduced motion
   *  gets the end state at once. */
  const animates = () =>
    visible && !(typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches);

  /** The agent moved on: the card folds away instead of vanishing. */
  function leave(node: HTMLElement) {
    if (!animates()) return { duration: 0 };
    const fold = slide(node, { duration: 260, easing: cubicOut });
    return { ...fold, css: (t: number, u: number) => `${fold.css?.(t, u) ?? ""};opacity: ${t * t}` };
  }

  /** A step just finished: the filled checkbox grows in and the tick draws.
   *  Local, so a card mounting with finished steps (a reopened chat) stays
   *  still — only a step finishing in view moves. */
  function fillIn(_node: Element) {
    if (!animates()) return { duration: 0 };
    return {
      duration: 220,
      easing: cubicOut,
      css: (t: number) => `transform: scale(${0.55 + 0.45 * t}); opacity: ${t}`,
    };
  }
  function drawTick(_node: Element) {
    if (!animates()) return { duration: 0 };
    return { delay: 90, duration: 240, easing: cubicOut, css: (_t: number, u: number) => `stroke-dashoffset: ${(9.2 * u).toFixed(2)}` };
  }

  /** Fold and unfold resize the card: animate its height from what it was
   *  to what it became (measured before and after the DOM update), so the
   *  steps fold into the summary line instead of snapping. */
  let card = $state<HTMLElement | undefined>();
  let heightBefore = 0;
  let heightMotion: Animation | null = null;
  $effect.pre(() => {
    void folded;
    heightBefore = card?.offsetHeight ?? 0;
  });
  $effect(() => {
    void folded;
    const el = card;
    const from = heightBefore;
    heightBefore = 0;
    if (el === undefined || from === 0 || !animates()) return;
    const to = el.offsetHeight;
    if (Math.abs(to - from) < 2) return;
    // One at a time, from wherever the last one had got to (the measure
    // above read its animated height); only the latest unclips the card.
    heightMotion?.cancel();
    // Clipped only while it moves: at rest the card's focus rings show.
    el.style.overflow = "hidden";
    const motion = el.animate([{ height: `${from}px` }, { height: `${to}px` }], {
      duration: 240,
      easing: "cubic-bezier(0.2, 0, 0, 1)",
    });
    heightMotion = motion;
    motion.onfinish = motion.oncancel = () => {
      if (heightMotion !== motion) return;
      heightMotion = null;
      el.style.removeProperty("overflow");
    };
  });

  const MARK_WORD: Record<StepMark, string> = {
    done: "done",
    current: "in progress",
    todo: "to do",
    blocked: "waiting",
  };
</script>

{#snippet checkbox(mark: StepMark)}
  <svg class="mark {mark}" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
    {#if mark === "done"}
      <rect x="1.5" y="1.5" width="13" height="13" rx="2.5" class="fill" in:fillIn />
      <path d="M4.9 8.3l2.1 2.1 4.1-4.5" class="tick" in:drawTick />
    {:else}
      <rect x="1.75" y="1.75" width="12.5" height="12.5" rx="2.5" class="ring" />
      {#if mark === "current"}<path d="M5 8h6" class="dash" />{/if}
    {/if}
  </svg>
{/snippet}

<section
  class="plan-card"
  class:pinned-card={!embedded}
  class:visible
  class:folded
  class:inactive
  aria-label="Tasks"
  bind:this={card}
  out:leave
>
  {#if !embedded}
  <button
    class="heading"
    aria-expanded={!folded}
    aria-label="{toggleLabel} ({progress.done} of {progress.total} done)"
    title={folded ? `${toggleLabel} · ${summary}` : toggleLabel}
    onclick={() => (userOpen = folded)}
  >
    <span class="list-icon"><TaskListGlyph /></span>
    <span class="kind">Tasks</span>
    <span class="summary">{folded ? summary : ''}</span>
    <span class="count">{count}</span>
    <Chevron open={!folded} />
  </button>
  {/if}
  {#if !folded}
    <ol class="steps quiet-scrollbar" bind:this={list} bind:clientHeight={listHeight}>
      {#each entries as entry, i (keys[i])}
        {@const mark = stepMark(entry)}
        {@const detail = stepDetail(entry)}
        <li class="step {mark}" data-step={keys[i]}>
          {@render checkbox(mark)}
          <span class="body">
            <span class="line">
              <span class="text">{stepText(entry, running)}</span>
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
  <span class="sr-only" role="status" aria-live={visible ? "polite" : "off"}
    >{progress.current !== null ? stepText(progress.current, running) : progress.allDone ? "tasks done" : ""}</span
  >
</section>

<style>
  /* Chrome (edge, radius, tint, entrance) is the shared .pinned-card recipe
     in app.css; this is the card's own layout. */
  .plan-card {
    display: flex;
    flex-direction: column;
    font-size: var(--text-sm);
    line-height: 1.45;
  }
  .heading {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    min-width: 0;
    padding: 6px 11px;
    background: none;
    border: none;
    border-radius: var(--radius-card);
    color: var(--muted);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .heading:hover,
  .heading:focus-visible {
    color: var(--fg);
    background: var(--hover-tint);
  }
  .list-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 14px;
    height: 14px;
  }
  .list-icon,
  .kind,
  .count {
    flex: none;
  }
  .kind {
    font-size: var(--text-xs);
    font-weight: 500;
  }
  .summary {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .steps {
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
  .plan-card:not(.inactive) .step.current {
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
  .mark .ring {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.4;
  }
  .mark .dash {
    stroke: currentColor;
    stroke-width: 1.6;
    stroke-linecap: round;
  }
  .plan-card:not(.inactive) .mark.current .ring {
    stroke: var(--fg);
    stroke-width: 1.6;
  }
  /* Waiting reads as an outline still to be drawn. */
  .mark.blocked .ring {
    stroke: var(--muted);
    stroke-dasharray: 2.6 2.3;
  }
  .mark.done .fill {
    fill: var(--accent);
    transform-box: fill-box;
    transform-origin: center;
  }
  .mark.done .tick {
    fill: none;
    stroke: var(--bg);
    stroke-width: 1.7;
    stroke-linecap: round;
    stroke-linejoin: round;
    /* The path's length, so the finishing tick can draw itself. */
    stroke-dasharray: 9.2;
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
