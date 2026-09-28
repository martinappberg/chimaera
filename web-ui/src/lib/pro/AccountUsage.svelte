<script lang="ts">
  import type { ProStatus } from "../net/native";
  import { usagePresentation } from "./usage";

  let { usage, limits }: { usage: ProStatus["usage"]; limits: ProStatus["limits"] } = $props();
  const rows = $derived([
    { name: "Cloud work this month", ...usagePresentation(usage?.cloud_hours, limits?.cloud_hours) },
    { name: "Project storage", ...usagePresentation(usage?.storage_bytes, limits?.storage_bytes) },
  ]);
</script>

<details class="usage-details">
  <summary>Usage and plan details</summary>
  <div class="usage-grid">
    {#each rows as row (row.name)}
      <div class="usage-item" class:limited={row.limited}>
        <span class="usage-label">{row.name}</span>
        <p class="usage-value">{row.label}</p>
        {#if row.progress !== null}
          <progress max="100" value={row.progress} aria-label={row.name} aria-valuetext={`${row.label}${row.detail ? ` · ${row.detail}` : ""}`}></progress>
        {:else}<div class="unknown-track" aria-hidden="true"></div>{/if}
        {#if row.detail}<p class="usage-detail">{row.detail}</p>{/if}
      </div>
    {/each}
  </div>
  <p class="usage-note">Based on your account’s current allowances. Local work remains available when a cloud limit is reached.</p>
</details>

<style>
  .usage-details { container: account-usage / inline-size; margin-top: 20px; border-top: 1px solid var(--edge); }
  summary { padding: 15px 0 0; color: var(--muted); font-size: var(--text-sm); cursor: pointer; }
  summary:hover { color: var(--fg); }
  summary:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
  .usage-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 28px; margin-top: 24px; }
  .usage-item { min-width: 0; }
  .usage-label { color: var(--muted); font-size: var(--text-sm); }
  .usage-value { margin: 9px 0 13px; font-size: 22px; font-weight: 500; font-variant-numeric: tabular-nums; line-height: 1.25; }
  progress, .unknown-track { display: block; width: 100%; height: 5px; border: 0; border-radius: 3px; overflow: hidden; background: var(--edge); }
  progress { appearance: none; color: var(--accent); accent-color: var(--accent); }
  progress::-webkit-progress-bar { background: var(--edge); border-radius: 3px; }
  progress::-webkit-progress-value { background: var(--accent); border-radius: 3px; }
  progress::-moz-progress-bar { background: var(--accent); border-radius: 3px; }
  .limited progress { color: var(--warn); accent-color: var(--warn); }
  .limited progress::-webkit-progress-value { background: var(--warn); }
  .limited progress::-moz-progress-bar { background: var(--warn); }
  .usage-detail { margin: 8px 0 0; color: var(--warn); font-size: var(--text-xs); }
  .usage-note { margin: 18px 0 0; color: var(--muted); font-size: var(--text-xs); line-height: 1.6; }
  @container account-usage (max-width: 420px) { .usage-grid { grid-template-columns: 1fr; gap: 25px; } }
</style>
