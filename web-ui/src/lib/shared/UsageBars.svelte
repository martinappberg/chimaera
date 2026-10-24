<script lang="ts">
  import { resetsLabel, usageLevel, usageTitle, type UsageRow } from "./usage";

  /**
   * Limit windows as rows: a name, a thin bar, the percentage, and when it
   * resets underneath. One drawing for the chat's usage popover, the /usage
   * panel and Agent settings. The bar stays the accent until a window is
   * near its edge (warn) or spent (err).
   */
  interface Props {
    rows: UsageRow[];
    /** For the reset wording; the caller's clock, so a test or a frozen
     *  tab reads the same. */
    now: number;
    /** The agent says requests are refused: every bar reads as spent. */
    refused?: boolean;
  }

  let { rows, now, refused = false }: Props = $props();
</script>

<div class="usage-bars">
  {#each rows as row (row.label)}
    {@const level = usageLevel(row.utilization, refused && row.utilization >= 100)}
    {@const resets = resetsLabel(row.resetsAt, now)}
    <div class="row">
      <span class="label">{usageTitle(row.label)}</span>
      <span class="bar" aria-hidden="true"
        ><span class="fill {level}" style:width="{Math.min(100, Math.max(0, row.utilization))}%"></span
        >{#if row.mark != null && row.mark > 0 && row.mark < 100}<span
            class="mark"
            style:left="{row.mark}%"
          ></span>{/if}</span
      >
      <span class="pct {level}">{Math.floor(row.utilization)}%</span>
      {#if resets !== null}<span class="resets">resets {resets}</span>{/if}
    </div>
  {/each}
</div>

<style>
  /* One grid for every row, so the bars start together whatever a label's
     length; a row's reset line spans it underneath. */
  .usage-bars {
    display: grid;
    grid-template-columns: minmax(0, max-content) minmax(48px, 1fr) 40px;
    column-gap: 10px;
    row-gap: 2px;
    align-items: center;
    font-size: var(--text-sm);
  }
  .row {
    display: contents;
  }
  .label,
  .bar,
  .pct {
    margin-top: 5px;
  }
  .label {
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .bar {
    position: relative;
    height: 4px;
    border-radius: 2px;
    background: color-mix(in srgb, var(--fg) 9%, transparent);
  }
  /* A notch where the bar's own meaning changes (the context row: where the
     agent compacts). Stands a hair proud of the track so it reads at 4px,
     in the bar's ink rather than a new colour. */
  .mark {
    position: absolute;
    top: -2px;
    bottom: -2px;
    width: 2px;
    margin-left: -1px;
    border-radius: 1px;
    background: color-mix(in srgb, var(--fg) 42%, transparent);
    pointer-events: none;
  }
  .fill {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: var(--accent);
  }
  .fill.warn {
    background: var(--warn);
  }
  .fill.full {
    background: var(--err);
  }
  .pct {
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
  .pct.warn {
    color: var(--warn);
  }
  .pct.full {
    color: var(--err);
  }
  .resets {
    grid-column: 1 / -1;
    font-size: var(--text-xs);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
</style>
