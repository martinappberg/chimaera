<script lang="ts">
  /**
   * "+54 −3": how many lines a change added and removed, in the one colour
   * pair every place that says so shares (an activity line, an edit row, a
   * diff's header). Tabular figures, so a growing count never jostles what
   * follows it. Nothing is drawn for a side that is zero.
   */
  interface Props {
    added: number;
    removed: number;
  }

  let { added, removed }: Props = $props();
</script>

<span class="change" aria-label={`${added} line${added === 1 ? "" : "s"} added, ${removed} removed`}
  >{#if added > 0}<span class="add">+{added}</span>{/if}{#if removed > 0}<span class="del">−{removed}</span>{/if}</span
>

<style>
  .change {
    flex: none;
    display: inline-flex;
    gap: 5px;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .add {
    color: var(--git-added);
  }
  .del {
    color: var(--git-deleted);
  }
</style>
