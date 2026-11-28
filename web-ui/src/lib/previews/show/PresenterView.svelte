<!--
  The presenter's view, layout only: this slide large, the next click beside
  it over the speaker notes, the timer, the place and the time of day under
  them, and the strip of every slide below. Each viewer draws its own
  pictures (`now`, `next`, `strip` snippets); the controller is
  `showController.svelte.ts`. Its look is the plugin deck viewer's
  (`plugins/ui/DeckNode.svelte`), so the two read as one product.
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
    /** The slides' width over height, for the next picture's frame. */
    ratio: number;
    notes: string;
    /** What the notes say when there are none. */
    noNotes?: string;
    elapsed: number;
    onResetTimer: () => void;
    /** Where the show stands ("3 / 12 · click 2 of 4"). */
    place: string;
    wallClock: string;
  }

  let { now, next, strip, ratio, notes, noNotes = "No notes for this slide.", elapsed, onResetTimer, place, wallClock }: Props = $props();
</script>

<div class="presenter" style:--show-ratio={ratio}>
  <div class="console">
    <div class="now">{@render now()}</div>
    <div class="side">
      <div class="next">
        <span class="cap">Next</span>
        {#if next === null}
          <p class="end">End of the show</p>
        {:else}
          <div class="pic">{@render next()}</div>
        {/if}
      </div>
      <div class="speak" role="note" aria-label="speaker notes">
        {#if notes.trim() !== ""}{notes}{:else}<span class="quiet">{noNotes}</span>{/if}
      </div>
    </div>
    <div class="foot">
      <button class="timer" onclick={onResetTimer} title="elapsed; click to start over">{clockText(elapsed)}</button>
      <span class="meta">{place}</span>
      <span class="grow"></span>
      <span class="meta">{wallClock}</span>
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
    background: var(--term-bg);
  }
  .console {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1.9fr) minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr) auto;
    gap: 10px;
    padding: 10px;
  }
  .now {
    min-width: 0;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .side {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .next {
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-height: 0;
  }
  /* The next click's frame: the slide's shape, as wide as the column, never
     taller than about two fifths of the view. */
  .pic {
    position: relative;
    align-self: center;
    width: min(100%, calc(38cqh * var(--show-ratio)));
    aspect-ratio: var(--show-ratio);
    overflow: hidden;
    border-radius: 3px;
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--fg) 12%, transparent);
  }
  .cap {
    font-size: var(--text-xs);
    font-weight: 600;
    color: color-mix(in srgb, var(--fg) 72%, var(--muted));
  }
  .end {
    margin: 0;
    padding: 0.8rem 0;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .speak {
    flex: 1;
    min-height: 0;
    overflow: auto;
    scrollbar-width: thin;
    padding: 0.6rem 0.8rem;
    border: 1px solid var(--edge);
    border-radius: 6px;
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
    align-items: baseline;
    gap: 0.9rem;
    min-width: 0;
  }
  .grow {
    flex: 1;
  }
  .timer {
    appearance: none;
    border: none;
    background: none;
    padding: 0 0.2rem;
    border-radius: 4px;
    font-family: var(--mono);
    font-size: 1.6rem;
    font-variant-numeric: tabular-nums;
    color: var(--fg);
    cursor: pointer;
  }
  .timer:hover {
    background: var(--row-hover);
  }
  .timer:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }
  .meta {
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
    .next {
      display: none;
    }
  }
</style>
