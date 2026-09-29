<script lang="ts">
  /**
   * The dashboard's one quiet activity line: "This week: 38 sessions · 1.2M
   * tokens" for this workspace (the last seven days), opening Settings →
   * Activity. Fetched while the dashboard is visible and on the history
   * nudge (a session ended); unknown tokens are left out, never shown as
   * zero; nothing recorded, no line.
   */
  import { untrack } from "svelte";
  import { aggTokens, fetchActivity, historyNudge, type ActivityAgg } from "../workspace/history";

  interface Props {
    wsId: string | null;
    visible: boolean;
    onOpenActivity?: () => void;
  }

  let { wsId, visible, onOpenActivity }: Props = $props();

  let week = $state<ActivityAgg | null>(null);
  let seq = 0;

  $effect(() => {
    const ws = wsId;
    if (!visible || ws === null) return;
    void $historyNudge;
    const mine = ++seq;
    untrack(() => {
      void fetchActivity({ workspaceId: ws, days: 7, weeks: 1 }).then(
        (r) => {
          if (mine === seq) week = r.week;
        },
        () => {
          // An older daemon (no activity route) or a blip: the line stays away.
          if (mine === seq) week = null;
        },
      );
    });
  });
</script>

{#if week !== null && week.sessions > 0}
  <button class="activity" title="the last seven days — open Activity" onclick={onOpenActivity}>
    This week: {week.sessions} session{week.sessions === 1 ? "" : "s"}{week.token_sessions > 0 ? ` · ${aggTokens(week)} tokens` : ""}
  </button>
{/if}

<style>
  .activity {
    appearance: none;
    align-self: flex-start;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    text-align: left;
    cursor: pointer;
  }
  .activity:hover {
    color: var(--fg);
  }
</style>
