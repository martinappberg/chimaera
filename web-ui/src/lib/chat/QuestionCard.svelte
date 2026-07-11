<script lang="ts">
  /**
   * The agent asked the user structured questions (claude AskUserQuestion /
   * codex requestUserInput) — option buttons per question, multi-select
   * toggles, and a free-text "other" per question. One normalized card for
   * every driver.
   *
   * Two faces: interactive (`answered` null — the pending overlay) and
   * read-only history (`answered` set — the transcript's answered card,
   * chosen labels highlighted, no controls), so an answered question stays
   * visible instead of vanishing.
   */
  import type { PendingQuestion } from "./store.svelte";

  interface Props {
    request: PendingQuestion;
    onAnswer?: (answers: Record<string, string[]>) => void;
    /** Chosen labels per question id; non-null renders the read-only
     *  answered card. Empty object = resolved without an answer. */
    answered?: Record<string, string[]> | null;
  }

  let { request, onAnswer, answered = null }: Props = $props();

  const readOnly = $derived(answered !== null);
  /** Resolved with no recorded choice (expired ask, or a journal from
   *  before answers were recorded). */
  const unanswered = $derived(
    answered !== null && request.questions.every((q) => (answered[q.id] ?? []).length === 0),
  );
  function chosen(qid: string): string[] {
    return answered?.[qid] ?? [];
  }
  /** The answered card folds into history collapsed — a one-line summary of
   *  what was chosen is what matters when scanning back; click to expand the
   *  full options. */
  let open = $state(false);
  const answerSummary = $derived(
    unanswered
      ? "not answered"
      : request.questions
          .map((q) => chosen(q.id).join(", "))
          .filter((s) => s.length > 0)
          .join(" · "),
  );
  /** Free-text answers ride as labels; anything chosen that is not one of
   *  the offered options renders as its own chip. */
  function freeText(qid: string, options: { label: string }[]): string[] {
    const offered = new Set(options.map((o) => o.label));
    return chosen(qid).filter((label) => !offered.has(label));
  }

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
    onAnswer?.(answers);
  }
</script>

<div
  class="question"
  class:answered={readOnly}
  role="group"
  aria-label={readOnly ? "answered question" : "the agent has a question"}
>
  {#if readOnly}
    <!-- History face: collapsed to a one-line answer summary; expand for the
         full options (chosen lit, the road not taken dimmed). -->
    <button
      class="q-collapse"
      onclick={() => (open = !open)}
      aria-expanded={open}
      title={open ? "collapse" : "expand"}
    >
      <svg class="chev" class:open viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
        <path
          d="M5 3l6 5-6 5"
          fill="none"
          stroke="currentColor"
          stroke-width="1.6"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
      <span class="q-collapse-label">answered</span>
      <span class="q-collapse-answer" class:none={unanswered}>{answerSummary}</span>
    </button>
    {#if open}
      {#each request.questions as q, qi (qi)}
        <div class="q">
          {#if q.header.length > 0}
            <span class="q-header">{q.header}</span>
          {/if}
          <div class="q-text">{q.question}</div>
          <div class="q-options">
            {#each q.options as opt, oi (oi)}
              <span class="q-opt static" class:on={chosen(q.id).includes(opt.label)}>
                {opt.label}
              </span>
            {/each}
            {#each freeText(q.id, q.options) as free (free)}
              <span class="q-opt static on">{free}</span>
            {/each}
          </div>
        </div>
      {/each}
      {#if unanswered}
        <div class="q-note">no longer active — not answered</div>
      {/if}
    {/if}
  {:else}
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
      <button class="opt primary" disabled={!complete} onclick={submit}>answer</button>
    </div>
  {/if}
</div>

<style>
  .question {
    border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--edge));
    background: color-mix(in srgb, var(--accent) 5%, transparent);
    border-radius: 8px;
    padding: 10px 12px;
    margin: 6px 0;
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  /* History face: quiet — the accent tint belongs to the live ask only. */
  .question.answered {
    border-color: var(--edge);
    background: color-mix(in srgb, var(--fg) 2%, transparent);
    animation: none;
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
  /* Read-only chips: same shapes, no affordance; unchosen options dim. */
  .q-opt.static {
    cursor: default;
    display: inline-block;
  }
  .q-opt.static:not(.on) {
    color: var(--muted);
    border-color: color-mix(in srgb, var(--edge) 60%, transparent);
  }
  .q-note {
    margin-top: 8px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  /* Collapsed answered header: a quiet toggle showing what was chosen. */
  .q-collapse {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    background: none;
    border: none;
    padding: 0;
    margin: 0;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
    text-align: left;
  }
  .q-collapse .chev {
    flex: none;
    color: var(--muted);
    transition: transform 0.12s ease;
  }
  .q-collapse .chev.open {
    transform: rotate(90deg);
  }
  .q-collapse-label {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .q-collapse-answer {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .q-collapse-answer.none {
    color: var(--muted);
    font-style: italic;
  }
  .question.answered .q:first-of-type {
    margin-top: 10px;
  }
  @media (prefers-reduced-motion: reduce) {
    .q-collapse .chev {
      transition: none;
    }
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
  /* The answer button is the shared .opt.primary (app.css). */
</style>
