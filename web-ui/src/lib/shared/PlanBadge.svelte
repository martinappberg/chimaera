<script lang="ts">
  let { plan, compact = false, ended = false }: {
    plan: "pro" | "max" | null;
    compact?: boolean;
    /** The plan has ended: a quiet "Plan ended" mark instead of a paid one. */
    ended?: boolean;
  } = $props();
</script>

{#if ended}
  <span class="plan-badge ended" class:compact title="Your Chimaera plan has ended">Plan ended</span>
{:else if plan !== null}
  <span class="plan-badge" class:compact title={`Chimaera ${plan === "max" ? "Max" : "Pro"} plan`}>
    {plan === "max" ? "Max" : "Pro"}
  </span>
{/if}

<style>
  .plan-badge { display: inline-flex; align-items: center; flex: none; padding: 3px 7px; border: 1px solid var(--edge); border-radius: 5px; background: color-mix(in srgb, var(--fg) 3%, transparent); color: var(--muted); font-family: var(--ui-font); font-size: var(--text-xs); font-weight: 550; line-height: 1.15; white-space: nowrap; user-select: none; }
  .ended { border-style: dashed; background: transparent; }
  .compact { padding: 2px 5px; border-radius: 4px; font-size: calc(var(--text-xs) - 1px); }
</style>
