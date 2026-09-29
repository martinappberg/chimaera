<script lang="ts">
  import type { DictationState } from "./voice.svelte";

  interface Props {
    state: DictationState;
    /** 0..1 microphone level. */
    level: number;
    finals: string;
    interim: string;
    /** What ends the recording, in words ("release Space to insert"). */
    hint: string;
    onCancel(): void;
  }

  let { state, level, finals, interim, hint, onCancel }: Props = $props();

  /** The strip is one line of context, not a transcript: the tail is what
   *  matters (CSS clips the rest from the left), so the DOM never holds a
   *  long recording's whole text. */
  const TAIL = 400;
  const shown = $derived.by(() => {
    const total = finals.length + (finals && interim ? 1 : 0) + interim.length;
    if (total <= TAIL) return { finals, interim };
    if (interim.length >= TAIL) return { finals: "", interim: interim.slice(-TAIL) };
    const room = TAIL - interim.length - (interim ? 1 : 0);
    return { finals: finals.slice(-room), interim };
  });

  const label = $derived(
    state === "starting" ? "Starting mic…" : state === "finishing" ? "Transcribing…" : "Listening",
  );
</script>

<div class="voice-strip" class:finishing={state === "finishing"} class:starting={state === "starting"}>
  <span class="meter" aria-hidden="true" style:--level={state === "listening" ? level : 0}>
    <span class="halo"></span>
    <span class="dot"></span>
  </span>
  <span class="label" role="status" aria-live="polite">{label}</span>
  <span class="words"
    ><span class="line"
      >{#if shown.finals}<span class="final">{shown.finals}</span>{/if}{#if shown.finals && shown.interim}{" "}{/if}{#if shown.interim}<span
          class="interim">{shown.interim}</span
        >{/if}</span
    ></span
  >
  <span class="hint">{hint}</span>
  <button
    type="button"
    class="cancel"
    aria-label="cancel dictation"
    title="cancel dictation (Esc)"
    onmousedown={(e) => e.preventDefault()}
    onclick={onCancel}
  >
    <svg viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
      <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
    </svg>
  </button>
</div>

<style>
  .voice-strip {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 26px;
    margin: 0 2px 6px;
    padding: 3px 4px 3px 8px;
    border: 1px solid color-mix(in srgb, var(--err) 30%, var(--edge));
    border-radius: 8px;
    background: color-mix(in srgb, var(--err) 5%, transparent);
    font-size: var(--text-sm);
    line-height: 1.35;
    box-sizing: border-box;
  }
  .voice-strip.finishing,
  .voice-strip.starting {
    border-color: var(--edge);
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }
  /* A red dot with a halo that swells with the input level: transform only,
     so the meter composites instead of repainting. */
  .meter {
    position: relative;
    flex: none;
    width: 14px;
    height: 14px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .dot {
    position: relative;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--err);
  }
  .halo {
    position: absolute;
    inset: 0;
    border-radius: 50%;
    background: color-mix(in srgb, var(--err) 28%, transparent);
    transform: scale(calc(0.55 + var(--level, 0) * 0.75));
    transition: transform 90ms linear;
  }
  .starting .dot {
    background: var(--muted);
  }
  .finishing .dot {
    background: var(--accent);
  }
  .starting .halo,
  .finishing .halo {
    background: none;
  }
  .label {
    flex: none;
    color: var(--fg);
    font-weight: 500;
  }
  /* Newest words stay in view: an rtl box overflows (and ellipsizes) at its
     left edge, and the isolated ltr line inside keeps the text itself in
     reading order. */
  .words {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    direction: rtl;
    text-align: left;
  }
  .line {
    direction: ltr;
    unicode-bidi: isolate;
  }
  .final {
    color: var(--fg);
  }
  .interim {
    color: var(--muted);
  }
  .hint {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .cancel {
    flex: none;
    width: 20px;
    height: 20px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: none;
    border-radius: 5px;
    background: none;
    color: var(--muted);
    cursor: pointer;
  }
  .cancel:hover {
    color: var(--fg);
    background: var(--row-hover);
  }
  @media (max-width: 520px) {
    .hint {
      display: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .halo {
      transition: none;
    }
  }
</style>
