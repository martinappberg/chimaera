<script lang="ts">
  /**
   * The Timeline tab: the workspace's full history (what happened, written by
   * the daemon), grouped by day, with filter chips only for the kinds that
   * have entries, and "load older" paging into the JSONL. Same row anatomy
   * as the dashboard's "Since you left" (TimelineRow) — one vocabulary.
   */
  import { onMount } from "svelte";
  import type { DashCtx } from "../dashboard/dash";
  import type { LayoutCtrl } from "../layout/dnd";
  import { ApiError } from "../net/api";
  import { pageVisible } from "../shared/visibility";
  import type { Session } from "./sessions";
  import {
    deliverNote,
    loadOlderTimeline,
    refreshTimeline,
    timelineStore,
    type TimelineEntry,
  } from "./timeline.svelte";
  import {
    FILTER_LABELS,
    dayGroups,
    filterGroups,
    filtersPresent,
    groupTimeline,
    type TimelineFilter,
  } from "./timelineModel";
  import TimelineRow from "./TimelineRow.svelte";

  interface Props {
    dash: DashCtx;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    wsId: string | null;
    wsRoot: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
    /** False while this retained tab is behind another pane tab. */
    visible?: boolean;
  }

  let { dash, sessions, names, wsId, wsRoot, paneId, ctrl, visible = true }: Props = $props();

  let filter = $state<TimelineFilter>("all");

  const groups = $derived(groupTimeline(timelineStore.entries));
  const filters = $derived(filtersPresent(groups));
  // A filter whose rows vanished (paged away, a new workspace) falls back.
  $effect(() => {
    if (!filters.includes(filter)) filter = "all";
  });
  const shown = $derived(filterGroups(groups, filter));

  /** "Today"/"Yesterday" labels move at midnight: re-derive per minute while
   *  someone is looking (gated on pane + document visibility). */
  let now = $state(Date.now());
  $effect(() => {
    if (!visible || !$pageVisible) return;
    now = Date.now();
    const t = setInterval(() => (now = Date.now()), 60_000);
    return () => clearInterval(t);
  });
  const days = $derived(dayGroups(shown, now));

  // Refetch on return (the store already catches up on document visibility;
  // this covers the pane-tab case where the document stayed visible). The
  // first run only primes the edge detector — mount already fetched.
  let wasVisible = false;
  let primed = false;
  $effect(() => {
    const on = visible;
    if (primed && on && !wasVisible) refreshTimeline();
    primed = true;
    wasVisible = on;
  });
  onMount(() => {
    if (timelineStore.head === 0) refreshTimeline();
  });

  /** Per-note delivery state (seq → text), replaced never mutated. */
  let deliverStates = $state(new Map<number, string>());
  async function deliver(entry: TimelineEntry): Promise<void> {
    if (wsId === null) return;
    deliverStates = new Map(deliverStates).set(entry.seq, "delivering…");
    try {
      const res = await deliverNote(wsId, entry.seq);
      deliverStates = new Map(deliverStates).set(entry.seq, "delivered");
      dash.onOpenSession(res.session_id);
    } catch (e) {
      const msg =
        e instanceof ApiError && e.status === 404
          ? "this daemon can't deliver notes yet"
          : e instanceof Error
            ? e.message
            : String(e);
      deliverStates = new Map(deliverStates).set(entry.seq, `not delivered — ${msg}`);
    }
  }

  const openFile = (p: string) => ctrl.openFileFrom(paneId, p, false);
</script>

<div class="timeline">
  <div class="inner">
    <header class="head">
      <div class="titles">
        <h1>Timeline</h1>
        <p class="sub">What happened in this workspace — written by chimaera as agents, commands and jobs finish.</p>
      </div>
      {#if filters.length > 1}
        <div class="chips" role="group" aria-label="Show">
          {#each filters as f (f)}
            <button class="chip" class:on={filter === f} aria-pressed={filter === f} onclick={() => (filter = f)}>
              {FILTER_LABELS[f]}
            </button>
          {/each}
        </div>
      {/if}
    </header>

    {#if timelineStore.available === false}
      <p class="empty">This daemon has no Timeline yet — update chimaera to see what happened here.</p>
    {:else if timelineStore.error !== null && timelineStore.entries.length === 0}
      <p class="empty err">{timelineStore.error}</p>
    {:else if timelineStore.entries.length === 0}
      <p class="empty">
        {#if timelineStore.loading}loading…{:else}Nothing recorded yet. Finished agent turns, long or failed commands, ended jobs and recorded findings will land here.{/if}
      </p>
    {:else if shown.length === 0}
      <p class="empty">Nothing matches that filter.</p>
    {:else}
      {#each days as day (day.key)}
        <section class="day" aria-label={day.label}>
          <h2 class="lbl">{day.label}</h2>
          <div class="rows">
            {#each day.groups as g (g.key)}
              <TimelineRow
                group={g}
                {sessions}
                {names}
                {wsRoot}
                onOpenSession={dash.onOpenSession}
                onOpenFile={openFile}
                onOpenKnowledge={dash.onOpenKnowledge}
                onDeliver={deliver}
                deliverState={deliverStates.get(g.first.seq) ?? null}
              />
            {/each}
          </div>
        </section>
      {/each}
      <div class="foot">
        {#if timelineStore.more}
          <button class="opt quiet" disabled={timelineStore.loading} onclick={() => void loadOlderTimeline()}>
            {timelineStore.loading ? "loading…" : "load older"}
          </button>
        {:else}
          <span class="end">the beginning of what chimaera kept</span>
        {/if}
        {#if timelineStore.error !== null}
          <span class="err">{timelineStore.error}</span>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .timeline {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    background: var(--bg);
  }
  .inner {
    max-width: 920px;
    margin: 0 auto;
    padding: 26px 32px 48px;
    display: flex;
    flex-direction: column;
    gap: 22px;
  }
  .head {
    display: flex;
    align-items: flex-end;
    gap: 20px;
    flex-wrap: wrap;
  }
  .titles {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .sub {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .chips {
    margin-left: auto;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 3px 11px;
    border-radius: 999px;
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease;
  }
  .chip:hover {
    color: var(--fg);
  }
  .chip.on {
    color: var(--fg);
    border-color: var(--fg);
  }

  .day {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .lbl {
    margin: 0;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .rows {
    display: flex;
    flex-direction: column;
    border-bottom: 1px solid var(--edge);
  }

  .empty {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
  .err {
    color: var(--err);
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 12px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .end {
    opacity: 0.8;
  }
</style>
