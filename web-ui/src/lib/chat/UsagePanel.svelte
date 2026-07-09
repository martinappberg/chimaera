<script lang="ts">
  /**
   * Plan-usage windows (claude usage_report / codex account/read) — a small
   * bar per limit window. Rendered inline in the transcript from a `usage`
   * block. Subscription plans show utilization, never dollars.
   */
  import type { UsageWindow } from "./store.svelte";

  interface Props {
    windows: UsageWindow[];
  }

  let { windows }: Props = $props();
</script>

<div class="usage-panel">
  {#if windows.length === 0}
    <div class="usage-row"><span>no usage data reported</span></div>
  {/if}
  {#each windows as w (w.label)}
    <div class="usage-row">
      <span class="usage-label">{w.label}</span>
      <span class="usage-bar"><span
          class="usage-fill"
          class:high={w.utilization >= 80}
          style:width="{Math.min(100, Math.max(0, w.utilization))}%"
        ></span></span>
      <span class="usage-pct">{Math.floor(w.utilization)}%</span>
    </div>
  {/each}
</div>

<style>
  .usage-panel {
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 8px 10px;
    margin: 6px 0;
    font-size: var(--text-sm);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .usage-row {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .usage-label {
    flex: none;
    width: 120px;
    color: var(--muted);
  }
  .usage-bar {
    flex: 1;
    height: 4px;
    border-radius: 2px;
    background: color-mix(in srgb, var(--fg) 8%, transparent);
    overflow: hidden;
  }
  .usage-fill {
    display: block;
    height: 100%;
    background: var(--accent);
    border-radius: 2px;
  }
  .usage-fill.high {
    background: var(--warn);
  }
  .usage-pct {
    flex: none;
    width: 36px;
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
</style>
