<script lang="ts">
  /**
   * "Since you left" — what happened while this viewer wasn't looking
   * (design §3): the Timeline past the viewer's last look, grouped per
   * session, bad news first, at most eight rows, "open timeline →" for the
   * rest. Empty is one honest line, never chrome. The baseline (the last
   * look) is captured by DashboardView; this component only renders.
   */
  import type { LayoutCtrl } from "../layout/dnd";
  import type { Session } from "../workspace/sessions";
  import { timelineStore, type SeenMark } from "../workspace/timeline.svelte";
  import TimelineRow from "../workspace/TimelineRow.svelte";
  import { formatClock, formatDuration, sinceYouLeft } from "../workspace/timelineModel";
  import type { DashCtx } from "./dash";

  interface Props {
    dash: DashCtx;
    /** The viewer's last look (null = never — everything is new). */
    baseline: SeenMark | null;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    wsRoot: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
    /** Wall clock, ticked by the parent while visible (the "3h 12m" ago). */
    now: number;
  }

  let { dash, baseline, sessions, names, wsRoot, paneId, ctrl, now }: Props = $props();

  const LIMIT = 8;
  const picked = $derived(sinceYouLeft(timelineStore.entries, baseline?.seq ?? 0, LIMIT));
  const away = $derived(baseline !== null && baseline.ts > 0 ? formatDuration(now - baseline.ts) : null);
</script>

<section class="since" aria-labelledby="since-title">
  <div class="shead">
    <span id="since-title" class="lbl">Since you left</span>
    {#if away !== null && picked.total > 0}<span class="sub">{away}</span>{/if}
    <button class="link" onclick={dash.onOpenTimeline}>open timeline →</button>
  </div>

  {#if timelineStore.error !== null && timelineStore.entries.length === 0}
    <p class="empty err">{timelineStore.error}</p>
  {:else if picked.total === 0}
    <p class="empty">
      {#if timelineStore.entries.length === 0}
        {#if timelineStore.loading && timelineStore.available === null}loading…{:else}Nothing recorded yet — finished turns, failed commands and ended jobs will show here.{/if}
      {:else if baseline !== null && baseline.ts > 0}
        Nothing new since {formatClock(baseline.ts)}.
      {:else}
        Nothing new.
      {/if}
    </p>
  {:else}
    <div class="rows">
      {#each picked.rows as g (g.key)}
        <TimelineRow
          group={g}
          {sessions}
          {names}
          {wsRoot}
          onOpenSession={dash.onOpenSession}
          onOpenFile={(p) => ctrl.openFileFrom(paneId, p, false)}
          onOpenKnowledge={dash.onOpenKnowledge}
        />
      {/each}
    </div>
    {#if picked.total > picked.rows.length}
      <button class="link more" onclick={dash.onOpenTimeline}>
        +{picked.total - picked.rows.length} more · open timeline →
      </button>
    {/if}
  {/if}
</section>

<style>
  .since {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .shead {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding-bottom: 6px;
  }
  .lbl {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .sub {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .link {
    margin-left: auto;
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--accent);
    cursor: pointer;
    white-space: nowrap;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link.more {
    margin-left: 0;
    align-self: flex-start;
    padding: 8px 0 0;
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
    padding: 2px 0 0;
  }
  .err {
    color: var(--err);
  }
</style>
