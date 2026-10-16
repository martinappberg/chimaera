<script lang="ts">
  import type { Snippet } from "svelte";

  /**
   * A loading line that stays out of sight unless the wait is real: a lazy
   * chunk usually lands in a few milliseconds, and flashing "Loading…" for
   * that long reads as a glitch. Mount it in an `{#await}`'s pending branch;
   * the branch unmounting is what cancels it.
   */
  interface Props {
    children: Snippet;
    /** How long a load may take before it is worth saying so. */
    after?: number;
  }

  let { children, after = 800 }: Props = $props();

  let slow = $state(false);
  $effect(() => {
    const timer = setTimeout(() => (slow = true), after);
    return () => clearTimeout(timer);
  });
</script>

{#if slow}<p role="status">{@render children()}</p>{/if}
