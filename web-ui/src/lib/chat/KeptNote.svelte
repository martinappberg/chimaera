<script lang="ts">
  /**
   * Where a Chimaera Pro project's work came back to this computer with files
   * both sides changed while apart: one quiet line across the column, like
   * the transfer notes (`TransferNote.svelte`), with a Review action that
   * opens the review of both versions. Shown only while something there
   * still waits for a choice; the review settles it and the line goes.
   */
  import { backNote } from "../pro/kept";

  interface Props {
    /** Files that return kept in both versions. */
    total: number;
    onReview(): void;
  }

  let { total, onReview }: Props = $props();
</script>

<div class="kept-note">
  <p class="rule">
    <span class="note"
      >{backNote(total)}<button type="button" class="review" onclick={onReview}>Review</button></span
    >
  </p>
</div>

<style>
  .kept-note {
    margin: 16px 0 8px;
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 1.5;
  }
  .rule {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    margin: 0;
  }
  .rule::before,
  .rule::after {
    content: "";
    flex: 1;
    min-width: 16px;
    border-top: 1px solid var(--edge);
  }
  .note {
    max-width: 80%;
    text-align: center;
  }
  .review {
    appearance: none;
    margin-left: 8px;
    padding: 1px 8px;
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--edge));
    border-radius: 999px;
    background: transparent;
    color: var(--fg);
    font: inherit;
    cursor: pointer;
    white-space: nowrap;
  }
  .review:hover {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .review:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 2px;
  }
</style>
