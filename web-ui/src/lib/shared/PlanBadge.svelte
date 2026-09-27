<script lang="ts">
  let { plan, compact = false }: {
    plan: "pro" | "max" | null;
    compact?: boolean;
  } = $props();
</script>

{#if plan !== null}
  <span class="plan-badge" class:compact title={`Chimaera ${plan === "max" ? "Max" : "Pro"} plan`}>
    {plan === "max" ? "Max" : "Pro"}
  </span>
{/if}

<style>
  .plan-badge {
    display: inline-flex;
    align-items: center;
    flex: none;
    padding: 3px 7px;
    border: 1px solid color-mix(in srgb, var(--accent) 32%, var(--edge));
    border-radius: 6px;
    background: linear-gradient(135deg,
      color-mix(in srgb, var(--accent) 12%, transparent),
      color-mix(in srgb, var(--accent) 4%, transparent));
    color: color-mix(in srgb, var(--accent) 65%, var(--fg));
    font-family: var(--ui-font);
    font-size: var(--text-xs);
    font-weight: 650;
    line-height: 1;
    letter-spacing: .06em;
    text-transform: uppercase;
    white-space: nowrap;
    user-select: none;
  }
  .compact {
    padding: 3px 5px;
    border-radius: 5px;
    font-size: calc(var(--text-xs) - 1px);
    letter-spacing: .04em;
  }
</style>
