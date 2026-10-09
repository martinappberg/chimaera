<script lang="ts">
  // The native queue stays mounted before any prompt arrives. The dialog is
  // part of the entry, never a separately loaded chunk: the prompt is often
  // the very step that restores the connection a chunk would load over.
  import { onMount } from "svelte";
  import { answerAskpass, askpassActive, listAskpass, onAskpass, onAskpassDone, type AskpassPrompt } from "../net/native";
  import { asyncDisposer } from "../shared/asyncDisposer";
  import { modalFocus } from "../shared/modalFocus";
  import AskpassDialog from "./AskpassDialog.svelte";
  import { askpassBelongsToHost } from "./askpassScope";

  let { hostAlias }: { hostAlias: string | null } = $props();
  let queue = $state<AskpassPrompt[]>([]);
  const askpass = $derived(queue[0] ?? null);
  $effect(() => { askpassActive.set(askpass !== null); });

  function enqueue(prompt: AskpassPrompt): void {
    if (!askpassBelongsToHost(prompt, hostAlias) || queue.some((p) => p.id === prompt.id)) return;
    queue = [...queue, prompt];
  }
  onMount(() => {
    let alive = true;
    const unlisteners = [asyncDisposer(onAskpass(enqueue)), asyncDisposer(onAskpassDone((id) => {
      queue = queue.filter((prompt) => prompt.id !== id);
    }))];
    void listAskpass().then((pending) => { if (alive) pending.forEach(enqueue); });
    return () => { alive = false; unlisteners.forEach((stop) => stop()); askpassActive.set(false); };
  });
  function answer(id: number, secret: string | null): void {
    if (queue[0]?.id !== id) return;
    void answerAskpass(id, secret);
    queue = queue.slice(1);
  }
</script>

{#if askpass !== null}
  <!-- This original focus owner survives every queued prompt. -->
  <div class="focus-owner" use:modalFocus={{ priority: 1 }}>
    <AskpassDialog prompt={askpass} onAnswer={answer} />
  </div>
{/if}

<style>
  .focus-owner { display: contents; }
</style>
