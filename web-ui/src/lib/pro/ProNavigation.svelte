<script lang="ts">
  import PlanBadge from "../shared/PlanBadge.svelte";
  import BrandMark from "../shared/BrandMark.svelte";
  let { plan = null, onOpen }: { plan?: "pro" | "max" | null; onOpen: () => void } = $props();
</script>
<div class="pro-navigation">
  <button onclick={onOpen} aria-label="Chimaera Pro">
    <BrandMark size={22} />
    <span class="label">Chimaera Pro</span>{#if plan === "max"}<PlanBadge {plan} compact />{:else if plan === "pro"}<span class="active">Active</span>{/if}
  </button>
</div>
<style>
  .pro-navigation { flex: none; border-top: 1px solid var(--edge); padding: 5px 8px; }
  button { display: flex; align-items: center; gap: 7px; width: 100%; padding: 7px 7px; border: 0; border-radius: 5px; background: transparent; color: var(--muted); text-align: left; font: inherit; font-size: var(--text-sm); cursor: pointer; }
  button:hover { color: var(--fg); background: var(--row-hover); }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
  .label { flex: 1; }
  .active { font-size: var(--text-xs); color: var(--muted); }
</style>
