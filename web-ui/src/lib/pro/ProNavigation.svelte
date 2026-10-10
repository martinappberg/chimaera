<script lang="ts">
  import PlanBadge from "../shared/PlanBadge.svelte";
  let { plan = null, onOpen }: { plan?: "pro" | "max" | null; onOpen: () => void } = $props();
</script>

{#if plan !== null}
  <button onclick={onOpen} aria-label={`Open Chimaera Pro — ${plan === "max" ? "Max" : "Pro"} plan`} title="Open Chimaera Pro">
    <PlanBadge {plan} compact />
  </button>
{/if}

<style>
  button { display: inline-flex; align-items: center; justify-content: center; flex: none; min-height: 28px; padding: 3px 1px; border: 0; border-radius: 5px; background: transparent; color: var(--muted); font: inherit; cursor: pointer; }
  button:hover { background: var(--row-hover); }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 1px; }
</style>
