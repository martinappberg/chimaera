<script lang="ts">
  /** Facts as label → value pairs, one per line. */
  interface Props {
    items: { key: string; value: string }[];
  }

  let { items }: Props = $props();
</script>

{#if items.length > 0}
  <dl class="kv">
    {#each items as it, i (i)}
      <dt>{it.key}</dt>
      <dd>{it.value}</dd>
    {/each}
  </dl>
{/if}

<style>
  .kv {
    margin: 0;
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 3px 14px;
    font-size: var(--text-sm);
  }
  dt {
    color: var(--muted);
    font-size: var(--text-xs);
    padding-top: 1px;
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
</style>
