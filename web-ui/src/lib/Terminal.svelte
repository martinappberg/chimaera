<script lang="ts">
  import { untrack } from "svelte";
  import { focusTerminal, release, show } from "./termPool";

  interface Props {
    /** Session whose pooled terminal this pane shows. */
    sessionId: string;
    /** True when this pane is the focused pane and this tab is active. */
    focused: boolean;
    /** The pane's terminal font-size override (px); undefined = default. */
    fontSize?: number;
  }

  let { sessionId, focused, fontSize = undefined }: Props = $props();

  let host = $state<HTMLDivElement | null>(null);

  // Attach the pooled terminal into this pane's container; the cleanup
  // (tab switch, pane close, unmount) parks it back in the warm stash.
  // Font size is deliberately untracked here — the second effect handles
  // live size changes without a park/re-attach round trip.
  $effect(() => {
    const el = host;
    const id = sessionId;
    if (el === null) return;
    show(id, el, untrack(() => fontSize));
    return () => release(id, el);
  });

  // Live per-pane font-size changes: show() on an attached terminal just
  // re-measures and refits in place.
  $effect(() => {
    const size = fontSize;
    if (host !== null) show(sessionId, host, size);
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
