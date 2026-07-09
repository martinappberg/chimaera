<script lang="ts">
  /**
   * The agent asked the user structured questions (claude AskUserQuestion /
   * codex requestUserInput) — option buttons per question, multi-select
   * toggles, and a free-text "other" per question. One normalized card for
   * every driver.
   */
  import type { PendingQuestion } from "./store.svelte";

  interface Props {
    request: PendingQuestion;
    onAnswer: (answers: Record<string, string[]>) => void;
  }

  let { request, onAnswer }: Props = $props();

  // Keyed by question/option INDEX, not by the model-authored id/label — those
  // are untrusted and may collide (two options both "Yes"), which would break
  // a keyed {#each} and make selection track duplicates as one. Labels are
  // rejoined only when building the wire answer.
  let picked = $state<Record<number, number[]>>({});
  let other = $state<Record<number, string>>({});

  function toggle(qi: number, oi: number, multi: boolean) {
    const current = picked[qi] ?? [];
    if (multi) {
      picked[qi] = current.includes(oi) ? current.filter((i) => i !== oi) : [...current, oi];
    } else {
      picked[qi] = [oi];
    }
  }

  const complete = $derived(
    request.questions.every(
      (_q, qi) => (picked[qi] ?? []).length > 0 || (other[qi] ?? "").trim().length > 0,
    ),
  );

  function submit() {
    const answers: Record<string, string[]> = {};
    request.questions.forEach((q, qi) => {
      const own = (picked[qi] ?? []).map((oi) => q.options[oi]?.label).filter((l) => l != null);
      const free = (other[qi] ?? "").trim();
      answers[q.id] = free.length > 0 ? [...own, free] : own;
    });
    onAnswer(answers);
  }
</script>

<div class="question" role="group" aria-label="the agent has a question">
  {#each request.questions as q, qi (qi)}
    <div class="q">
      {#if q.header.length > 0}
        <span class="q-header">{q.header}</span>
      {/if}
      <div class="q-text">{q.question}</div>
      <div class="q-options">
        {#each q.options as opt, oi (oi)}
          <button
            class="q-opt"
            class:on={(picked[qi] ?? []).includes(oi)}
            title={opt.description}
            aria-pressed={(picked[qi] ?? []).includes(oi)}
            onclick={() => toggle(qi, oi, q.multiSelect)}
          >
            {opt.label}
          </button>
        {/each}
      </div>
      <input
        class="q-other"
        placeholder="other…"
        bind:value={other[qi]}
        onkeydown={(e) => {
          if (e.key === "Enter" && complete) {
            e.preventDefault();
            submit();
          }
        }}
      />
    </div>
  {/each}
  <div class="q-actions">
    <button class="q-submit" disabled={!complete} onclick={submit}>answer</button>
  </div>
</div>

<style>
  .question {
    border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--edge));
    background: color-mix(in srgb, var(--accent) 5%, transparent);
    border-radius: 8px;
    padding: 10px 12px;
    margin: 6px 0;
    animation: rise 0.15s ease;
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(3px);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .question {
      animation: none;
    }
  }
  .q + .q {
    margin-top: 12px;
    padding-top: 10px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 60%, transparent);
  }
  .q-header {
    display: inline-block;
    font-size: var(--text-xs);
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--edge));
    border-radius: 999px;
    padding: 0 8px;
    margin-bottom: 4px;
  }
  .q-text {
    font-size: var(--text-md);
    margin-bottom: 8px;
  }
  .q-options {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .q-opt {
    font: inherit;
    font-size: var(--text-sm);
    padding: 3px 12px;
    border-radius: 6px;
    border: 1px solid var(--edge);
    background: none;
    color: var(--fg);
    cursor: pointer;
    text-align: left;
    transition:
      border-color 0.12s ease,
      background-color 0.12s ease;
  }
  .q-opt:hover {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
  .q-opt.on {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    border-color: color-mix(in srgb, var(--accent) 60%, var(--edge));
  }
  .q-other {
    margin-top: 8px;
    width: 100%;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 4px 10px;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
  }
  .q-other:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
  .q-actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 10px;
  }
  .q-submit {
    font: inherit;
    font-size: var(--text-sm);
    padding: 3px 14px;
    border-radius: 5px;
    border: 1px solid color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 15%, transparent);
    color: var(--fg);
    cursor: pointer;
  }
  .q-submit:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .q-submit:not(:disabled):hover {
    background: color-mix(in srgb, var(--accent) 24%, transparent);
  }
</style>
