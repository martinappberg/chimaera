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
  import { mayTakeFocus } from "./promptFocus";
  import { askedParts, askedText, optionLabel } from "./questionText";
  import type { PendingQuestion } from "./store.svelte";

  interface Props {
    request: PendingQuestion;
    /** False when the answer could not be sent (not connected): the card
     *  stays answerable. */
    onAnswer?: (answers: Record<string, string[]>) => boolean | void;
    /** Chosen labels per question id; non-null renders the read-only
     *  answered card. Empty object = resolved without an answer. */
    answered?: Record<string, string[]> | null;
    /** False while the owning retained chat tab is hidden. */
    visible?: boolean;
    /** Open a file the question links to. */
    onOpenPath?: (path: string, e: MouseEvent) => void;
  }

  let { request, onAnswer, answered = null, visible = true, onOpenPath }: Props = $props();

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

  const has = (qi: number) => (picked[qi] ?? []).length > 0 || (other[qi] ?? "").trim().length > 0;
  const complete = $derived(request.questions.every((_q, qi) => has(qi)));
  const last = $derived(request.questions.length - 1);

  /** One question, one choice: picking it IS the answer — no second press.
   *  Anything richer (several questions, a multi-select) collects first. */
  const instant = $derived(request.questions.length === 1 && !request.questions[0].multiSelect);
  /** Several questions are asked one at a time: the card shows the one the
   *  keys are on, the steps above it say where the others stand. */
  let cursor = $state(0);
  const at = $derived(Math.max(0, Math.min(cursor, last)));
  const current = $derived(request.questions[at]);

  function goTo(qi: number) {
    cursor = qi;
    // The control just used may belong to the question leaving the card:
    // the keys stay with the card.
    cardEl?.focus({ preventScroll: true });
  }
  /** The question to go to next: the first still open after this one, else
   *  the first open before it; -1 when no other is open. */
  const nextOpen = $derived.by(() => {
    const open = request.questions.map((_q, qi) => qi !== at && !has(qi));
    const later = open.indexOf(true, at + 1);
    return later !== -1 ? later : open.indexOf(true);
  });
  function choose(qi: number, oi: number, multi: boolean) {
    toggle(qi, oi, multi);
    if (multi) return;
    // One choice: a pick replaces whatever was typed instead.
    other[qi] = "";
    if (instant) submit();
    else if (nextOpen !== -1) goTo(nextOpen);
  }
  /** Enter: send once every question has an answer; until then, on to the
   *  next one still open. */
  function advance() {
    if (complete) submit();
    else if (nextOpen !== -1) goTo(nextOpen);
  }
  function typed(qi: number, multi: boolean) {
    if (!multi && (other[qi] ?? "").trim().length > 0) picked[qi] = [];
  }

  let cardEl = $state<HTMLDivElement | null>(null);
  // Like the permission prompt: the ask takes the keyboard on arrival.
  // preventScroll keeps ChatView's own stick-to-bottom contract.
  $effect(() => {
    if (visible && !readOnly && cardEl !== null && mayTakeFocus(cardEl)) cardEl.focus({ preventScroll: true });
  });
  /** 1–9 pick an option of the question showing; Enter moves on, and sends
   *  once all are answered. Typing in the text field is never a shortcut. */
  function onKeydown(e: KeyboardEvent) {
    if (readOnly || e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.target instanceof HTMLInputElement) return;
    if (e.key === "Enter" && e.target === e.currentTarget) {
      e.preventDefault();
      advance();
      return;
    }
    if (!/^[1-9]$/.test(e.key) || current === undefined) return;
    const oi = Number(e.key) - 1;
    if (oi >= current.options.length) return;
    e.preventDefault();
    choose(at, oi, current.multiSelect);
  }

  /** One answer per card: a second key or click before the daemon's
   *  `question_resolved` lands must not send another. */
  let sent = false;
  function submit() {
    if (sent) return;
    sent = true;
    const answers: Record<string, string[]> = {};
    request.questions.forEach((q, qi) => {
      const own = (picked[qi] ?? []).map((oi) => q.options[oi]?.label).filter((l) => l != null);
      const free = (other[qi] ?? "").trim();
      answers[q.id] = free.length > 0 ? [...own, free] : own;
    });
    // Never lose the answer to a closed socket: an unsent one unlatches.
    if (onAnswer?.(answers) === false) sent = false;
  }
</script>

<!-- No whitespace between parts: each already carries its own. -->
{#snippet inline(text: string, links: boolean)}
  <!-- prettier-ignore -->
  {#each askedParts(text) as part, pi (pi)}{#if part.kind === "code"}<code>{part.text}</code>{:else if part.kind === "strong"}<strong>{part.text}</strong>{:else if part.kind === "file" && links && onOpenPath !== undefined}{@const path = part.path}<button class="q-file" title={path} onclick={(e) => onOpenPath(path, e)}>{part.text}</button>{:else}{part.text}{/if}{/each}
{/snippet}

{#snippet tick()}
  <svg viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
    <path d="M3.5 8.5l3 3 6-7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
  </svg>
{/snippet}

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
            <span class="q-asked" title={askedText(q.question)}>{@render inline(q.question, false)}</span>
            <span class="q-collapse-answer" class:none={chosen(q.id).length === 0}
              >{chosen(q.id).length === 0
                ? "not answered"
                : chosen(q.id)
                    .map((label) => askedText(optionLabel(label).text))
                    .join(", ")}</span
            >
          </span>
        {/each}
      </span>
    </button>
    {#if open}
      {#each request.questions as q, qi (qi)}
        <div class="q-past">
          {#if request.questions.length > 1}
            <div class="q-text">{@render inline(q.question, false)}</div>
          {/if}
          <div class="q-chips">
            {#each q.options as opt, oi (oi)}
              <span class="q-chip" class:on={chosen(q.id).includes(opt.label)} title={opt.description}>
                {@render inline(optionLabel(opt.label).text, false)}
              </span>
            {/each}
            {#each freeText(q.id, q.options) as free (free)}
              <span class="q-chip on">{free}</span>
            {/each}
          </div>
        </div>
      {/each}
      {#if unanswered}
        <div class="q-note">no longer active — not answered</div>
      {/if}
    {/if}
  {:else if current !== undefined}
    {#if request.questions.length > 1}
      <!-- One question at a time; the steps say where the others stand. -->
      <div class="q-steps">
        {#each request.questions as q, qi (qi)}
          <button
            class="q-step"
            class:current={qi === at}
            class:done={has(qi)}
            aria-current={qi === at ? "step" : undefined}
            aria-label={`question ${qi + 1}${has(qi) ? ", answered" : ""}`}
            onclick={() => goTo(qi)}
          >
            <span class="q-step-mark">{#if has(qi)}{@render tick()}{:else}{qi + 1}{/if}</span>
            {#if q.header.length > 0}<span class="q-step-name">{q.header}</span>{/if}
          </button>
        {/each}
      </div>
    {:else if current.header.length > 0}
      <div class="q-head"><span class="q-header">{current.header}</span></div>
    {/if}
    <div class="q-text">{@render inline(current.question, true)}</div>
    <div class="q-options">
      {#each current.options as opt, oi (oi)}
        {@const shown = optionLabel(opt.label)}
        {@const on = (picked[at] ?? []).includes(oi)}
        <button class="q-opt" class:on aria-pressed={on} onclick={() => choose(at, oi, current.multiSelect)}>
          {#if current.multiSelect}
            <span class="q-check" class:on aria-hidden="true">{#if on}{@render tick()}{/if}</span>
          {/if}
          <span class="q-body">
            <span class="q-line">
              <span class="q-label">{@render inline(shown.text, false)}</span>
              {#if shown.recommended}<span class="q-rec">recommended</span>{/if}
            </span>
            {#if opt.description !== ""}<span class="q-desc">{@render inline(opt.description, false)}</span>{/if}
          </span>
          {#if on && !current.multiSelect}
            <span class="q-key on" aria-hidden="true">{@render tick()}</span>
          {:else if oi < 9}
            <kbd class="q-key">{oi + 1}</kbd>
          {/if}
        </button>
      {/each}
      <!-- The answer that is not on the list is the list's last row. -->
      <label class="q-other">
        <input
          placeholder="Something else…"
          aria-label="another answer"
          bind:value={other[at]}
          oninput={() => typed(at, current.multiSelect)}
          onkeydown={(e) => {
            if (e.key === "Enter" && !e.isComposing && has(at)) {
              e.preventDefault();
              advance();
            }
          }}
        />
        <kbd class="q-key" class:idle={(other[at] ?? "").trim().length === 0} aria-hidden="true">↵</kbd>
      </label>
    </div>
    {#if !instant || countdown !== null}
      <div class="q-actions">
        {#if countdown !== null}
          <span class="deadline" class:passed={deadlinePassed} aria-live="polite">
            {deadlinePassed ? "auto-skip pending…" : `skips in ${countdown}s`}
          </span>
        {:else if current.multiSelect}
          <span class="q-hint">choose any</span>
        {/if}
        {#if !instant}
          {#if complete || nextOpen === -1}
            <button class="opt primary" disabled={!complete} onclick={submit}>answer<kbd>↵</kbd></button>
          {:else}
            <button class="opt" onclick={advance}>next<kbd>↵</kbd></button>
          {/if}
        {/if}
      </div>
    {/if}
  {/if}
</div>

<style>
  .question {
    border: 1px solid color-mix(in srgb, var(--accent) 38%, var(--edge));
    background: color-mix(in srgb, var(--accent) 4%, transparent);
    border-radius: 10px;
    padding: 12px;
    margin: 6px 0;
    outline: none;
    /* The card answers to its own width (a narrow split, a phone). */
    container-type: inline-size;
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  .question:focus-visible {
    border-color: var(--focus-ring);
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

  /* Steps: one per question, the current one lit, answered ones ticked. */
  .q-steps {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin: 0 0 10px;
  }
  .q-step {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    background: none;
    border: 1px solid transparent;
    border-radius: 999px;
    padding: 2px 9px 2px 3px;
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .q-step:hover {
    color: var(--fg);
  }
  .q-step.current {
    color: var(--fg);
    font-weight: 500;
    background: color-mix(in srgb, var(--fg) 9%, transparent);
  }
  .q-step-mark {
    display: inline-grid;
    place-items: center;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: 1px solid color-mix(in srgb, var(--muted) 55%, transparent);
    font-size: 10px;
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  /* A step with no name is its number alone. */
  .q-step-mark:last-child {
    margin-right: -6px;
  }
  .q-step.done .q-step-mark {
    border-color: transparent;
    background: color-mix(in srgb, var(--accent) 75%, transparent);
    color: var(--bg);
  }

  .q-head {
    margin: 0 2px 6px;
  }
  .q-header {
    font-size: var(--text-xs);
    color: var(--accent-text, var(--accent));
    letter-spacing: 0.02em;
  }
  .q-text {
    font-size: var(--text-md);
    font-weight: 500;
    line-height: 1.45;
    margin: 0 2px 10px;
    overflow-wrap: anywhere;
  }
  code {
    font-family: var(--mono);
    font-size: 0.92em;
    padding: 1px 5px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--fg) 8%, transparent);
  }
  strong {
    font-weight: 600;
  }
  /* A file the question names: opens like any path in the chat. */
  .q-file {
    font: inherit;
    color: inherit;
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    text-decoration: underline;
    text-decoration-color: color-mix(in srgb, var(--muted) 60%, transparent);
    text-underline-offset: 3px;
    transition: text-decoration-color 0.12s ease;
  }
  .q-file:hover {
    text-decoration-color: var(--accent);
  }

  /* The options: one list, a line each, the whole line the target and its
     key at the end. The free-text answer is the list's last line. */
  .q-options {
    display: flex;
    flex-direction: column;
    border: 1px solid color-mix(in srgb, var(--edge) 85%, transparent);
    border-radius: 8px;
    background: color-mix(in srgb, var(--bg) 55%, transparent);
    overflow: hidden;
  }
  .q-opt,
  .q-other {
    box-sizing: border-box;
    display: flex;
    align-items: flex-start;
    gap: 10px;
    width: 100%;
    min-height: 34px;
    padding: 7px 9px 7px 12px;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    text-align: left;
    background: none;
    border: none;
    border-radius: 0;
    transition: background-color 0.12s ease;
  }
  .q-opt + .q-opt,
  .q-opt + .q-other {
    border-top: 1px solid color-mix(in srgb, var(--edge) 60%, transparent);
  }
  .q-opt {
    cursor: pointer;
  }
  .q-opt:hover {
    background: var(--row-hover, color-mix(in srgb, var(--fg) 5%, transparent));
  }
  .q-opt:focus-visible {
    outline: none;
    box-shadow: inset 0 0 0 1.5px var(--focus-ring);
  }
  .q-opt.on,
  .q-opt.on:hover {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .q-body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
    line-height: 1.45;
  }
  .q-line {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    column-gap: 8px;
  }
  .q-label {
    overflow-wrap: anywhere;
  }
  .q-rec {
    font-size: var(--text-xs);
    line-height: 1.5;
    padding: 0 7px;
    border-radius: 999px;
    color: var(--accent-text, var(--accent));
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .q-desc {
    color: var(--muted);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }
  /* Multi-select: a box that fills. */
  .q-check {
    flex: none;
    display: inline-grid;
    place-items: center;
    width: 15px;
    height: 15px;
    margin-top: 2px;
    border-radius: 4px;
    border: 1px solid color-mix(in srgb, var(--muted) 70%, transparent);
    color: var(--bg);
    transition:
      background-color 0.12s ease,
      border-color 0.12s ease;
  }
  .q-check.on {
    background: var(--accent);
    border-color: var(--accent);
  }
  kbd {
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  /* The key that picks the line; on a chosen single choice, its tick. */
  .q-key {
    flex: none;
    display: inline-grid;
    place-items: center;
    min-width: 18px;
    height: 18px;
    margin-top: 1px;
    box-sizing: border-box;
    border-radius: 5px;
    border: 1px solid color-mix(in srgb, var(--edge) 90%, transparent);
    line-height: 1;
    transition: opacity 0.12s ease;
  }
  .q-key.on {
    border-color: transparent;
    background: var(--accent);
    color: var(--bg);
  }
  /* Enter shows once there is something to send; its place is kept. */
  .q-key.idle {
    opacity: 0;
  }
  .opt kbd {
    margin-left: 7px;
  }
  .q-other {
    align-items: center;
    cursor: text;
  }
  .q-other input {
    flex: 1;
    min-width: 0;
    padding: 0;
    font: inherit;
    color: inherit;
    background: none;
    border: none;
    outline: none;
  }
  .q-other input::placeholder {
    color: var(--muted);
  }
  .q-other:focus-within {
    box-shadow: inset 0 0 0 1.5px color-mix(in srgb, var(--accent) 60%, var(--edge));
  }
  /* No keyboard, no key hints. */
  @media (hover: none) {
    .q-opt kbd.q-key,
    .q-other .q-key {
      display: none;
    }
  }

  .q-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: flex-end;
    gap: 6px 10px;
    margin-top: 10px;
  }
  .q-hint,
  .deadline {
    margin-right: auto;
    padding-left: 4px;
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .deadline.passed {
    color: var(--warn);
  }
  /* The buttons are the shared .opt / .opt.primary (app.css). */

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
  /* Opened: every option as a chip, the chosen lit, the rest dimmed. */
  .q-past {
    margin-top: 10px;
  }
  .q-past .q-text {
    margin: 0 0 6px;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .q-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .q-chip {
    font-size: var(--text-sm);
    padding: 2px 10px;
    border-radius: 6px;
    border: 1px solid color-mix(in srgb, var(--edge) 60%, transparent);
    color: var(--muted);
  }
  .q-chip.on {
    color: var(--fg);
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
  .q-note {
    margin-top: 8px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  /* Narrow: an answered question sits above its answer, both in full. */
  @container (max-width: 420px) {
    .q-pair {
      flex-direction: column;
      gap: 0;
    }
    .q-asked,
    .q-collapse-answer {
      max-width: 100%;
      white-space: normal;
      overflow-wrap: anywhere;
    }
  }
  /* Very narrow: the words get the room the key hints took. */
  @container (max-width: 250px) {
    .q-opt,
    .q-other {
      gap: 8px;
      padding: 7px 8px;
    }
    .q-opt kbd.q-key,
    .q-other .q-key {
      display: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .q-collapse .chev,
    .q-opt,
    .q-key,
    .q-check,
    .q-step {
      transition: none;
    }
  }
</style>
