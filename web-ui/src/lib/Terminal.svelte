<script lang="ts">
  import { focusTerminal, release, show } from "./termPool";

  interface Props {
    /** Session whose pooled terminal this pane shows. */
    sessionId: string;
    /** True when this pane is the focused pane and this tab is active. */
    focused: boolean;
  }

  let { sessionId, focused }: Props = $props();

  let host = $state<HTMLDivElement | null>(null);

  // Attach the pooled terminal into this pane's container; the cleanup
  // (tab switch, pane close, unmount) parks it back in the warm stash.
  $effect(() => {
    const el = host;
    const id = sessionId;
    if (el === null) return;
    show(id, el);
    return () => release(id, el);
  });

  $effect(() => {
    if (focused) focusTerminal(sessionId);
  });
</script>

<div class="term-view" bind:this={host}></div>

<style>
  .term-view {
    position: absolute;
    inset: 0;
  }
</style>
