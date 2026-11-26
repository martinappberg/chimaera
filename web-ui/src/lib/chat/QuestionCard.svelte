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
    /** False while the owning retained chat tab is hidden. */
    visible?: boolean;
  }

  let { request, onAnswer, answered = null, visible = true }: Props = $props();

  const readOnly = $derived(answered !== null);
  let remainingMs = $state<number | null>(null);
  const deadlinePassed = $derived(remainingMs !== null && remainingMs <= 0);
  const countdown = $derived(
    remainingMs === null ? null : Math.max(0, Math.ceil(remainingMs / 1000)),
  );

  // Codex owns the actual timeout in the driver; this is presentation only.
  // The event carries an absolute timestamp, so replay/reconnect never grants
  // the card a fresh countdown. Updating here cannot race the wire truth: the
  // driver resolves the card authoritatively with QuestionResolved.
  $effect(() => {
    const deadline = request.expiresAtMs;
    if (readOnly || deadline === null) {
      remainingMs = null;
      return;
    }
    // The absolute deadline remains authoritative; recompute immediately when
    // the tab becomes visible instead of ticking hidden cards four times/sec.
    if (!visible) return;
    const update = () => {
      remainingMs = Math.max(0, deadline - Date.now());
    };
    const initial = Math.max(0, deadline - Date.now());
    remainingMs = initial;
    if (initial === 0) return;
    const timer = setInterval(() => {
      update();
      if (Date.now() >= deadline) clearInterval(timer);
    }, 250);
    return () => clearInterval(timer);
  });
  /** Resolved with no recorded choice (expired ask, or a journal from
   *  before answers were recorded). */
  const unanswered = $derived(
    answered !== null && request.questions.every((q) => (answered[q.id] ?? []).length === 0),
  );
  function chosen(qid: string): string[] {
    return answered?.[qid] ?? [];
  }
  /** The answered card folds into history as what was asked and what was
   *  chosen, a line per question; click to expand the options not taken. */
  let open = $state(false);
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

  /** One question, one choice: picking it IS the answer — no second press.
   *  Anything richer (several questions, a multi-select) collects first. */
  const instant = $derived(request.questions.length === 1 && !request.questions[0].multiSelect);
  function choose(qi: number, oi: number, multi: boolean) {
    toggle(qi, oi, multi);
    if (instant) submit();
  }

  /** Options with something to say about themselves read as a list, one per
   *  line with its description; bare labels stay compact chips. */
  const described = (q: { options: { description: string }[] }) => q.options.some((o) => o.description !== "");

  let cardEl = $state<HTMLDivElement | null>(null);
  // Like the permission prompt: the ask takes the keyboard on arrival.
  // preventScroll keeps ChatView's own stick-to-bottom contract.
  $effect(() => {
    if (visible && !readOnly) cardEl?.focus({ preventScroll: true });
  });
  /** 1–9 pick an option of the first question still open (the only one, for
   *  an instant card); Enter sends a complete answer. Typing in "other…"
   *  is never read as a shortcut. */
  function onKeydown(e: KeyboardEvent) {
    if (readOnly || e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.target instanceof HTMLInputElement) return;
    if (e.key === "Enter" && e.target === e.currentTarget && complete) {
      e.preventDefault();
      submit();
      return;
    }
    if (!/^[1-9]$/.test(e.key)) return;
    const qi = Math.max(
      0,
      request.questions.findIndex((q, i) => q.multiSelect || (picked[i] ?? []).length === 0),
    );
    const q = request.questions[qi];
    const oi = Number(e.key) - 1;
    if (q === undefined || oi >= q.options.length) return;
    e.preventDefault();
    choose(qi, oi, q.multiSelect);
  }

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

<!-- Focusable so 1–9 / Enter answer without the mouse; the buttons remain
     the accessible path. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="question"
  class:answered={readOnly}
  class:visible
  role="group"
  aria-label={readOnly ? "answered question" : "the agent has a question"}
  tabindex={readOnly ? undefined : 0}
  bind:this={cardEl}
  onkeydown={onKeydown}
>
  {#if readOnly}
    <!-- History face: what was asked and what was answered, a line each;
         expand for the full options (chosen lit, the road not taken dimmed). -->
    <button
      class="q-collapse"
      onclick={() => (open = !open)}
      aria-expanded={open}
      title={open ? "collapse" : "show the options"}
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
      <span class="q-pairs">
        {#each request.questions as q, qi (qi)}
          <span class="q-pair">
            <span class="q-asked">{q.question}</span>
            <span class="q-collapse-answer" class:none={chosen(q.id).length === 0}
              >{chosen(q.id).length === 0 ? "not answered" : chosen(q.id).join(", ")}</span
            >
          </span>
        {/each}
      </span>
    </button>
    {#if open}
      {#each request.questions as q, qi (qi)}
        <div class="q">
          {#if request.questions.length > 1}
            <div class="q-text">{q.question}</div>
          {/if}
          <div class="q-options">
            {#each q.options as opt, oi (oi)}
              <span class="q-opt static" class:on={chosen(q.id).includes(opt.label)} title={opt.description}>
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
        <div class="q-head">
          {#if q.header.length > 0}
            <span class="q-header">{q.header}</span>
          {/if}
          {#if q.multiSelect}
            <span class="q-hint">choose any</span>
          {/if}
        </div>
        <div class="q-text">{q.question}</div>
        <div class="q-options" class:list={described(q)}>
          {#each q.options as opt, oi (oi)}
            <button
              class="q-opt"
              class:on={(picked[qi] ?? []).includes(oi)}
              aria-pressed={(picked[qi] ?? []).includes(oi)}
              onclick={() => choose(qi, oi, q.multiSelect)}
            >
              {#if oi < 9}<kbd>{oi + 1}</kbd>{/if}
              <span class="q-label">{opt.label}</span>
              {#if opt.description !== ""}<span class="q-desc">{opt.description}</span>{/if}
            </button>
          {/each}
        </div>
        <input
          class="q-other"
          placeholder="other…"
          aria-label="another answer"
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
    {#if !instant || countdown !== null}
      <div class="q-actions">
        {#if countdown !== null}
          <span class="deadline" class:passed={deadlinePassed} aria-live="polite">
            {deadlinePassed ? "auto-skip pending…" : `skips in ${countdown}s`}
          </span>
        {/if}
        {#if !instant}
          <button class="opt primary" disabled={!complete} onclick={submit}>answer<kbd>↵</kbd></button>
        {/if}
      </div>
    {/if}
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
    border-color: transparent;
    background: none;
    padding: 2px 0;
    animation: none;
  }
  .question:not(.visible) {
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
  .question {
    outline: none;
  }
  .question:focus-visible {
    box-shadow: 0 0 0 2px var(--focus-ring);
  }
  .q-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .q-head:empty {
    display: none;
  }
  .q-hint {
    color: var(--muted);
    font-size: var(--text-xs);
  }
  kbd {
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .opt kbd {
    margin-left: 7px;
  }
  /* Options that describe themselves: one per line, the label leading. */
  .q-options.list {
    flex-direction: column;
    flex-wrap: nowrap;
    gap: 4px;
  }
  .q-opt {
    display: inline-flex;
    align-items: baseline;
    gap: 8px;
  }
  .q-options.list .q-opt {
    display: flex;
    padding: 5px 10px;
  }
  .q-label {
    flex: none;
  }
  .q-desc {
    min-width: 0;
    color: var(--muted);
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
    align-items: baseline;
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
  .q-pairs {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .q-pair {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
  }
  /* The question yields first: the answer is what a scan back is after. */
  .q-asked {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
  }
  .q-collapse-answer {
    flex: none;
    max-width: 60%;
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
    align-items: center;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 10px;
  }
  .deadline {
    margin-right: auto;
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .deadline.passed {
    color: var(--warn);
  }
  /* The answer button is the shared .opt.primary (app.css). */
</style>
