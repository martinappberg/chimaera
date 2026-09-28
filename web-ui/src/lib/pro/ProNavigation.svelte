<script lang="ts">
  import PlanBadge from "../shared/PlanBadge.svelte";
  let { plan = null, onOpen }: { plan?: "pro" | "max" | null; onOpen: () => void } = $props();
</script>
<div class="pro-navigation">
  <button onclick={onOpen} aria-label="Chimaera Pro">
    <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path d="M8 1.5 13.6 4.7v6.6L8 14.5l-5.6-3.2V4.7Z" fill="none" stroke="currentColor" stroke-width="1.2" /></svg>
    <span>Chimaera Pro</span><PlanBadge {plan} compact />
  </button>
</div>
<style>
  .pro-navigation { flex: none; border-top: 1px solid var(--edge); padding: 5px 8px; }
  button { display: flex; align-items: center; gap: 8px; width: 100%; padding: 7px 8px; border: 0; border-radius: 5px; background: transparent; color: var(--muted); text-align: left; font: inherit; font-size: var(--text-sm); cursor: pointer; }
  button:hover { color: var(--fg); background: var(--row-hover); }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
  button span { flex: 1; }
</style>
