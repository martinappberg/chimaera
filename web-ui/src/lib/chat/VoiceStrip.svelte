<script lang="ts">
  import type { DictationState } from "./voice.svelte";

  interface Props {
    state: DictationState;
    /** The last few 0..1 input levels, oldest first — the waveform. */
    levels: readonly number[];
    finals: string;
    interim: string;
    /** The input recording, as the system names it ("" until it opens). */
    device: string;
    onCancel(): void;
  }

  let { state, levels, finals, interim, device, onCancel }: Props = $props();

  /** One line of context, not a transcript: the tail is what matters (CSS
   *  clips the rest from the left), so the DOM never holds a long
   *  recording's whole text. */
  const TAIL = 400;
  const shown = $derived.by(() => {
    const total = finals.length + (finals && interim ? 1 : 0) + interim.length;
    if (total <= TAIL) return { finals, interim };
    if (interim.length >= TAIL) return { finals: "", interim: interim.slice(-TAIL) };
    const room = TAIL - interim.length - (interim ? 1 : 0);
    return { finals: finals.slice(-room), interim };
  });
  const status = $derived(
    state === "starting" ? "Starting…" : state === "finishing" ? "Transcribing…" : "Listening…",
  );
</script>

<div
  class="voice-strip"
  class:quiet={state !== "listening"}
  title={device ? `Recording from ${device}` : undefined}
>
  <span class="bars" aria-hidden="true">
    {#each levels as level, i (i)}
      <span class="bar" style:--level={state === "listening" ? level : 0}></span>
    {/each}
  </span>
  <span class="words" role="status" aria-live="polite"
    ><span class="line"
      >{#if shown.finals || shown.interim}{#if shown.finals}<span class="final">{shown.finals}</span
          >{/if}{#if shown.finals && shown.interim}{" "}{/if}{#if shown.interim}<span class="interim"
            >{shown.interim}</span
          >{/if}{:else}<span class="status">{status}</span>{/if}</span
    ></span
  >
  {#if state === "finishing" && (shown.finals || shown.interim)}
    <span class="status trailing">Transcribing…</span>
  {/if}
  <button
    type="button"
    class="cancel"
    aria-label="discard dictation"
    title="Discard (Esc)"
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
    gap: 10px;
    min-height: 28px;
    margin: 0 2px 6px;
    padding: 3px 4px 3px 10px;
    border: 1px solid color-mix(in srgb, var(--err) 28%, var(--edge));
    border-radius: 8px;
    background: color-mix(in srgb, var(--err) 4%, transparent);
    font-size: var(--text-sm);
    line-height: 1.35;
    box-sizing: border-box;
  }
  .voice-strip.quiet {
    border-color: var(--edge);
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }
  /* The waveform: each bar is one recent 100 ms level, scaled on the
     compositor (transform only) — a flat row means the mic hears nothing. */
  .bars {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 2px;
    height: 14px;
  }
  .bar {
    width: 3px;
    height: 14px;
    border-radius: 2px;
    background: var(--err);
    transform: scaleY(calc(0.18 + var(--level, 0) * 0.82));
    transition: transform 90ms linear;
  }
  .quiet .bar {
    background: var(--muted);
    opacity: 0.6;
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
  .interim,
  .status {
    color: var(--muted);
  }
  .trailing {
    flex: none;
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
  @media (prefers-reduced-motion: reduce) {
    .bar {
      transition: none;
    }
  }
</style>
