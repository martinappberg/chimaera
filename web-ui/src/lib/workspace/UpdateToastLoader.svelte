<script lang="ts">
  import { onMount } from "svelte";
  import { dismissAnswer, snoozeUpdate, type UpdateNotice } from "./update.svelte";
  import { updateToastPresentation, type ToastPresentation } from "./updateToastPresentation";

  let { notice }: { notice: UpdateNotice } = $props();
  let presentation = $state<ToastPresentation>({ phase: "loading" });
  let retry = $state<() => void>(() => {});
  const answer = $derived(notice.kind === "checking" || notice.kind === "current" ||
    notice.kind === "dev" || notice.kind === "managed" || notice.kind === "with-app" ||
    notice.kind === "failed");
  onMount(() => {
    const original = updateToastPresentation(() => import("./UpdateToast.svelte"),
      (next) => { presentation = next; });
    retry = original.retry;
    return original.dispose;
  });
</script>

{#if presentation.phase === "ready"}
  {@const Toast = presentation.component}
  <!-- The current notice, never the one captured before the import settled. -->
  <Toast {notice} />
{:else}
  <aside class="loading-toast" role="status" aria-live="polite">
    <span>{presentation.phase === "failed" ? "Couldn’t load the update notice." : "Loading update notice…"}</span>
    {#if presentation.phase === "failed"}<button onclick={retry}>Retry</button>{/if}
    <button onclick={answer ? dismissAnswer : snoozeUpdate}>{answer ? "Close" : "Later"}</button>
  </aside>
{/if}

<style>
  .loading-toast { position: fixed; bottom: 20px; right: 20px; z-index: 150; max-width: min(360px, calc(100vw - 40px)); padding: 14px 16px; border: 1px solid var(--edge); border-radius: 10px; background: var(--bg); color: var(--fg); box-shadow: 0 4px 20px var(--scrim); font-size: var(--text-sm); }
  button { margin-left: 10px; padding: 4px 10px; border: 1px solid var(--edge); border-radius: 6px; background: var(--bg); color: var(--fg); font: inherit; cursor: pointer; }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
</style>
