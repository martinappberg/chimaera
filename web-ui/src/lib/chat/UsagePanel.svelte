<script lang="ts">
  /**
   * Plan-usage windows (claude usage_report / codex account/rateLimits/read)
   * — a bar per limit window and when it resets, drawn by the shared
   * `UsageBars` the header's usage popover and Agent settings use too.
   * Rendered inline in the transcript from a `usage` block (/usage).
   * Subscription plans show utilization, never dollars.
   */
  import UsageBars from "../shared/UsageBars.svelte";
  import { usageRows } from "../shared/usage";
  import type { UsageWindow } from "./store.svelte";

  interface Props {
    windows: UsageWindow[];
  }

  let { windows }: Props = $props();
  const rows = $derived(usageRows(windows));
  // The reset wording reads as of when the panel was drawn: a transcript
  // block is a record, not a live meter (the header's popover is live).
  const now = Date.now();
</script>

<div class="usage-panel">
  {#if rows.length === 0}
    <span class="none">No usage data reported</span>
  {:else}
    <UsageBars {rows} {now} />
  {/if}
</div>

<style>
  .usage-panel {
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 9px 12px;
    margin: 6px 0;
    max-width: 420px;
  }
  .none {
    font-size: var(--text-sm);
    color: var(--muted);
  }
</style>
