<script lang="ts">
  // The native queue stays mounted before any prompt/chunk arrives. A failed
  // view load never answers, drops or replaces its original native prompt.
  import { onMount, type Component } from "svelte";
  import { answerAskpass, askpassActive, listAskpass, onAskpass, onAskpassDone, type AskpassPrompt } from "../net/native";
  import { asyncDisposer } from "../shared/asyncDisposer";
  import { modalFocus } from "../shared/modalFocus";
  import { focusOnMount } from "../shared/focusOnMount";
  import { loadPaneView, retryPaneView } from "../layout/lazyViews";
  import { askpassBelongsToHost } from "./askpassScope";

  let { hostAlias }: { hostAlias: string | null } = $props();
  let queue = $state<AskpassPrompt[]>([]);
  const askpass = $derived(queue[0] ?? null);
  let retried = $state<Promise<Component<any>> | null>(null);
  const surface = $derived(askpass === null ? null : retried ?? loadPaneView("askpass"));
  $effect(() => { askpassActive.set(askpass !== null); });

  function enqueue(prompt: AskpassPrompt): void {
    if (!askpassBelongsToHost(prompt, hostAlias) || queue.some((p) => p.id === prompt.id)) return;
    if (queue.length === 0) retried = null;
    queue = [...queue, prompt];
  }
  onMount(() => {
    let alive = true;
    const unlisteners = [asyncDisposer(onAskpass(enqueue)), asyncDisposer(onAskpassDone((id) => {
      if (queue[0]?.id === id) retried = null;
      queue = queue.filter((prompt) => prompt.id !== id);
    }))];
    void listAskpass().then((pending) => { if (alive) pending.forEach(enqueue); });
    return () => { alive = false; unlisteners.forEach((stop) => stop()); askpassActive.set(false); };
  });
  function answer(id: number, secret: string | null): void {
    if (queue[0]?.id !== id) return;
    void answerAskpass(id, secret);
    queue = queue.slice(1);
    retried = null;
  }
  function retry(id: number, error: unknown): void {
    if (queue[0]?.id === id) retried = retryPaneView("askpass", error);
  }
</script>

{#snippet fallback(id: number, failed: boolean, error?: unknown)}
  <div class="backdrop" role="presentation" onclick={() => answer(id, null)}
    onkeydown={(event) => { if (event.key === "Escape") { event.stopPropagation(); answer(id, null); } }}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="dialog" role="dialog" aria-modal="true" aria-label="SSH authentication"
      tabindex="-1" onclick={(event) => event.stopPropagation()}>
      <p>{failed ? "Couldn’t load the SSH prompt." : "Loading SSH prompt…"}</p>
      <div class="actions">
        <button use:focusOnMount onclick={() => answer(id, null)}>cancel</button>
        {#if failed}<button onclick={() => retry(id, error)}>Retry</button>{/if}
      </div>
    </div>
  </div>
{/snippet}

{#if askpass !== null}
  <!-- This original focus owner survives loading and every queued prompt. -->
  <div class="loader" use:modalFocus={{ priority: 1 }}>
    {#await surface}
      {@render fallback(askpass.id, false)}
    {:then Dialog}
      {#if Dialog}<Dialog prompt={askpass} onAnswer={answer}/>{/if}
    {:catch error}
      {@render fallback(askpass.id, true, error)}
    {/await}
  </div>
{/if}

<style>
  .loader { display: contents; }
  .backdrop { position: fixed; inset: 0; z-index: 230; display: grid; place-items: center; padding: 24px; background: var(--scrim); backdrop-filter: blur(2px); }
  .dialog { width: min(440px, 100%); padding: 20px; background: var(--bg); color: var(--fg); border: 1px solid var(--edge); border-radius: 10px; box-shadow: 0 16px 48px var(--scrim); }
  p { margin: 0 0 14px; font-size: var(--text-sm); }
  .actions { display: flex; justify-content: flex-end; gap: 8px; }
  button { font: inherit; padding: 7px 16px; background: var(--bg); color: var(--fg); border: 1px solid var(--edge); border-radius: 6px; cursor: pointer; }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
</style>
