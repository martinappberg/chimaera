<!--
  The presenter's view, layout only: this slide large on the left, its top
  level with the next click's frame on the right, the speaker notes under
  that, the timer, the place and its click dots and the time of day below,
  and the strip of every slide at the bottom. Each viewer draws its own
  pictures (`now`, `next`, `strip`, `dots` snippets); the controller is
  `showController.svelte.ts`. Slides sit on the sunken surface the slide
  viewers' stages use, so a white slide reads as an object in light themes.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { clockText } from "./showSync";

  interface Props {
    /** The slide as the audience sees it now. */
    now: Snippet;
    /** What the next click shows; null at the end of the show. */
    next: Snippet | null;
    /** The slide strip (optional: a one-slide deck has none). */
    strip?: Snippet;
    /** The current slide's click dots beside the place (optional). */
    dots?: Snippet;
    /** The slides' width over height, for the next picture's frame. */
    ratio: number;
    notes: string;
    /** What the notes area says when there are none (default: nothing). */
    noNotes?: string;
    elapsed: number;
    onResetTimer: () => void;
    /** Where the show stands among the slides it shows: "3 / 12". */
    place: string;
    wallClock: string;
  }

  let { now, next, strip, dots, ratio, notes, noNotes = "", elapsed, onResetTimer, place, wallClock }: Props = $props();
</script>

<div class="presenter" style:--show-ratio={ratio}>
  <div class="console">
    <div class="now">{@render now()}</div>
    <div class="side">
      <div class="pic" class:end={next === null}>
        {#if next === null}
          <p class="end-note">End of the show</p>
        {:else}
          {@render next()}
          <span class="cap" aria-hidden="true">Next</span>
        {/if}
      </div>
      <div class="speak" role="note" aria-label="speaker notes">
        {#if notes.trim() !== ""}{notes}{:else if noNotes !== ""}<span class="quiet">{noNotes}</span>{/if}
      </div>
    </div>
    <div class="foot">
      <button class="timer" ondblclick={onResetTimer} title="elapsed · double-click to start over">{clockText(elapsed)}</button>
      <span class="place">{place}</span>
      {#if dots !== undefined}<span class="dots">{@render dots()}</span>{/if}
      <span class="grow"></span>
      <span class="clock">{wallClock}</span>
    </div>
  </div>
  {@render strip?.()}
</div>

<style>
  .presenter {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    container: presenter / size;
    background: color-mix(in srgb, var(--fg) 4%, var(--term-bg));
  }
  .console {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 2.2fr) minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr) auto;
    gap: 12px;
    padding: 12px 12px 6px;
  }
  .now {
    min-width: 0;
    min-height: 0;
    display: flex;
    align-items: flex-start;
    justify-content: center;
  }
  .side {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  /* The next click's frame: the slide's shape, as wide as the column, never
     taller than about two fifths of the view; the same size at the end. */
  .pic {
    position: relative;
    flex: none;
    align-self: flex-start;
    width: min(100%, calc(38cqh * var(--show-ratio)));
    aspect-ratio: var(--show-ratio);
    overflow: hidden;
    border-radius: 3px;
    box-shadow:
      0 0 0 1px color-mix(in srgb, var(--fg) 10%, transparent),
      0 1px 6px color-mix(in srgb, var(--fg) 10%, transparent);
  }
  .pic.end {
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--fg) 7%, var(--term-bg));
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--fg) 8%, transparent);
  }
  .end-note {
    margin: 0;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  /* A quiet label in the frame's corner, legible on any slide. */
  .cap {
    position: absolute;
    top: 5px;
    left: 5px;
    z-index: 1;
    padding: 0 5px;
    border-radius: 3px;
    background: color-mix(in srgb, var(--term-bg) 82%, transparent);
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 1.5;
    pointer-events: none;
  }
  .speak {
    flex: 1;
    min-height: 0;
    overflow: auto;
    scrollbar-width: thin;
    padding: 0 2px;
    color: var(--fg);
    font-size: 1.05rem;
    line-height: 1.55;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .quiet {
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .foot {
    grid-column: 1 / -1;
    display: flex;
    align-items: center;
    gap: 0.8rem;
    min-width: 0;
  }
  .grow {
    flex: 1;
  }
  .timer {
    appearance: none;
    flex: none;
    border: none;
    background: none;
    padding: 0 0.2rem;
    margin-left: -0.2rem;
    border-radius: 4px;
    font-family: var(--mono);
    font-size: 1.6rem;
    font-variant-numeric: tabular-nums;
    color: var(--fg);
    cursor: default;
    user-select: none;
  }
  .timer:hover {
    background: var(--row-hover);
  }
  .timer:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }
  .place {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-md);
    font-variant-numeric: tabular-nums;
    color: color-mix(in srgb, var(--fg) 88%, var(--muted));
    white-space: nowrap;
  }
  .dots {
    flex: none;
    display: inline-flex;
    min-width: 0;
  }
  .clock {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    color: var(--muted);
    white-space: nowrap;
  }

  /* A narrow pane: this slide over the notes; the next click goes. */
  @container presenter (max-width: 560px) {
    .console {
      grid-template-columns: 1fr;
      grid-template-rows: minmax(0, 1.4fr) minmax(0, 1fr) auto;
    }
    .pic {
      display: none;
    }
  }
</style>
