<script lang="ts">
  import { loadDialog } from "./lazyViews";
  import type { ComponentProps } from "svelte";
  import type QuickOpen from "../workspace/QuickOpen.svelte";
  import type CloseDirtyDialog from "./CloseDirtyDialog.svelte";
  import { focusOnMount } from "../shared/focusOnMount";
  import { modalFocus } from "../shared/modalFocus";

  type Props = { onCancel(): void } & (
    { kind: "quickOpen"; dialogProps: ComponentProps<typeof QuickOpen> }
    | { kind: "closeDirty"; dialogProps: ComponentProps<typeof CloseDirtyDialog> }
  );
  let { kind, dialogProps, onCancel }: Props = $props();
  const title = $derived(kind === "quickOpen" ? "quick open" : "close confirmation");
  function requestDialog() { return loadDialog(kind); }
  let surface = $state(requestDialog());
  function retry(): void { surface = requestDialog(); }
</script>

{#snippet fallback(failed: boolean)}
<div class="backdrop" role="presentation" onclick={onCancel} onkeydown={event => {
  if (event.key === "Escape") { event.stopPropagation(); onCancel(); }
}}>
  <div class="dialog" role="dialog" aria-modal="true" aria-label={title}
    tabindex="-1" onclick={event => event.stopPropagation()} onkeydown={event => {
      if (event.key === "Escape") onCancel();
      event.stopPropagation();
    }}>
    <h2>{failed ? `Couldn’t load ${title}` : `Loading ${title}…`}</h2>
    {#if failed}<p>Your work is still open. Retry, or cancel and return to it.</p>{/if}
    <div class="actions">
      <button use:focusOnMount onclick={onCancel}>Cancel</button>
      {#if failed}<button onclick={retry}>Retry</button>{/if}
    </div>
  </div>
</div>

{/snippet}

<!-- This restore owner survives the fallback/result swap; the loaded original
     dialog owns the top focus trap while it is present. Only one modal renders. -->
<div class="loader" use:modalFocus>
  {#await surface}
    {@render fallback(false)}
  {:then Dialog}
    <Dialog {...dialogProps} />
  {:catch}
    {@render fallback(true)}
  {/await}
</div>

<style>
  .loader { display: contents; }
  .backdrop { position: fixed; inset: 0; z-index: 110; background: var(--scrim); display: grid; place-items: center; padding: 24px; }
  .dialog { width: min(420px, 100%); background: var(--bg); color: var(--fg); border: 1px solid var(--edge); border-radius: 10px; padding: 18px 20px; box-shadow: 0 16px 48px var(--scrim); }
  h2 { margin: 0 0 12px; font-size: var(--text-md); }
  p { color: var(--muted); font-size: var(--text-sm); }
  .actions { display: flex; justify-content: flex-end; gap: 8px; }
  button { font: inherit; min-height: 40px; padding: 8px 12px; background: var(--bg); color: var(--fg); border: 1px solid var(--edge); border-radius: 7px; cursor: pointer; }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
</style>
